use anyhow::{bail, Context, Result};
use clap::Parser;
use std::collections::HashMap;
use std::path::PathBuf;
use wasmtime::*;

// ── CLI ─────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "yuri", about = "Local Vocalizer TTS via WASM")]
struct Cli {
    /// Text to synthesize (Russian)
    #[arg(short, long)]
    text: String,

    /// Output WAV file path
    #[arg(short, long, default_value = "output.wav")]
    output: PathBuf,

    /// Path to voice data directory
    #[arg(long, default_value = "wasm/voicedata")]
    voice_dir: PathBuf,

    /// Path to webtts.wasm
    #[arg(long, default_value = "wasm/webtts.wasm")]
    wasm: PathBuf,
}

// ── Emscripten State ────────────────────────────────────────────────────────

struct State {
    // Virtual file system
    files: HashMap<i32, VFile>,
    next_fd: i32,
    voice_dir: PathBuf,

    // Assets pre-loaded into WASM heap
    assets: HashMap<String, (u32, u32)>, // name -> (heap_ptr, byte_len)

    // Audio capture
    audio_samples: Vec<i16>,
    sample_rate: u32,

    // Emscripten runtime
    temp_ret: i32,

    // Engine flags
    init_complete: bool,
    needs_more_audio: bool, // asm_const [3] fired — need to call ttsSpeak(0,0)
    speak_complete: bool,   // asm_const [5] with speak completion

    // Stored memory handle (set after instantiation)
    memory: Option<Memory>,
}

struct VFile {
    data: Vec<u8>,
    position: usize,
}

impl State {
    fn new(voice_dir: PathBuf) -> Self {
        Self {
            files: HashMap::new(),
            next_fd: 10,
            voice_dir,
            assets: HashMap::new(),
            audio_samples: Vec::new(),
            sample_rate: 22050,
            temp_ret: 0,
            init_complete: false,
            needs_more_audio: false,
            speak_complete: false,
            memory: None,
        }
    }
}

// ── Memory Helpers ──────────────────────────────────────────────────────────

fn get_memory(caller: &mut Caller<'_, State>) -> Memory {
    caller.data().memory.unwrap()
}

fn read_cstring(memory: &Memory, store: &impl AsContext<Data = State>, ptr: u32) -> String {
    let data = memory.data(store);
    let mut end = ptr as usize;
    while end < data.len() && data[end] != 0 {
        end += 1;
    }
    String::from_utf8_lossy(&data[ptr as usize..end]).to_string()
}

fn read_i32(memory: &Memory, store: &impl AsContext<Data = State>, addr: u32) -> i32 {
    let data = memory.data(store);
    let off = addr as usize;
    i32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

fn write_i32(memory: &Memory, store: &mut impl AsContextMut<Data = State>, addr: u32, val: i32) {
    memory
        .write(store, addr as usize, &val.to_le_bytes())
        .unwrap();
}

/// Write a UTF-8 string (null-terminated) into WASM memory using _tts_malloc.
/// Returns the pointer in WASM memory.
fn alloc_string(store: &mut Store<State>, instance: &Instance, s: &str) -> Result<u32> {
    let malloc = instance
        .get_typed_func::<i32, i32>(&mut *store, "_malloc")
        .context("missing _malloc")?;
    let len = s.len() as i32 + 1; // +1 for null terminator
    let ptr = malloc.call(&mut *store, len)?;
    let memory = store.data().memory.context("missing memory")?;
    memory.write(&mut *store, ptr as usize, s.as_bytes())?;
    memory.write(&mut *store, ptr as usize + s.len(), &[0u8])?;
    Ok(ptr as u32)
}

// ── asm_const Dispatch ──────────────────────────────────────────────────────
//
// The WASM module calls _emscripten_asm_const_*(idx, ...) where idx selects
// a JS function from a dispatch table. We reimplement the 25 entries in Rust.

fn asm_const_dispatch(caller: &mut Caller<'_, State>, idx: i32, args: &[i32]) -> i32 {
    // Log ALL calls unconditionally
    eprintln!("[asm_const {}] args={:?}", idx, args);
    match idx {
        // [0] Parse config JSON, return heapSize
        //     If the WASM buffer is empty, write our config there.
        0 => {
            let ptr = args.first().copied().unwrap_or(0) as u32;
            let mem = get_memory(caller);
            let config_str = read_cstring(&mem, &*caller, ptr);

            if config_str.is_empty() || config_str.len() < 10 {
                // Build file list from voice directory
                let voice_dir = caller.data().voice_dir.clone();
                let files: Vec<serde_json::Value> = std::fs::read_dir(&voice_dir)
                    .map(|rd| {
                        rd.filter_map(|e| e.ok())
                            .filter(|e| e.path().is_file())
                            .map(|e| {
                                let name = e.file_name().to_string_lossy().to_string();
                                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                                serde_json::json!({"name": name, "size": size})
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let config = serde_json::json!({
                    "heapSize": 64 * 1024 * 1024,
                    "files": files,
                    "licensingmode": "unmetered",
                    "licensegraceperiod": 999999,
                    "licensingexplicit": false,
                    "companyname": "Code Factory",
                    "applicationname": "Vocalizer"
                });
                let json = config.to_string();
                eprintln!("[asm_const 0] INJECTING config ({}B) at {:#x}", json.len(), ptr);
                // Ensure we don't overflow the buffer (assume ~4KB allocated)
                let max_write = json.len().min(4000);
                mem.write(&mut *caller, ptr as usize, &json.as_bytes()[..max_write]).unwrap();
                mem.write(&mut *caller, ptr as usize + max_write, &[0u8]).unwrap();
            } else {
                eprintln!("[asm_const 0] config ({}B): {}",
                    config_str.len(), &config_str[..config_str.len().min(200)]);
            }
            64 * 1024 * 1024
        }

        // [1] assetsInHeap = true
        1 => {
            eprintln!("[asm_const 1] assetsInHeap = true");
            0
        }

        // [2] Extract mrk/prompt markup — not needed
        2 => 0,

        // [3] In JS, setTimeout INTERRUPTS the current WASM call.
        //     We simulate this by trapping — the main loop catches it and continues.
        3 => {
            caller.data_mut().needs_more_audio = true;
            // Return a special value that won't be used (the trap below aborts)
            // We use Err to trigger a wasmtime trap, simulating JS setTimeout yield
            return -1; // Signal to the asm_const wrapper to trap
        }

        // [4] Set speech params — parse JSON and call ttsSetOneParam* exports
        4 => {
            let ptr = args.first().copied().unwrap_or(0) as u32;
            let mem = get_memory(caller);
            let json_str = read_cstring(&mem, &*caller, ptr);
            eprintln!("[asm_const 4] setSpeechParams: {}", &json_str[..json_str.len().min(120)]);

            if let Ok(params) = serde_json::from_str::<serde_json::Value>(&json_str) {
                let set_int = caller.get_export("_ttsSetOneParamInt")
                    .and_then(|e| e.into_func());
                let set_str = caller.get_export("_ttsSetOneParamStr")
                    .and_then(|e| e.into_func());

                if let Some(obj) = params.as_object() {
                    for (key, val) in obj {
                        // Integer params
                        let param_int = match key.as_str() {
                            "volume" => Some(8),
                            "speed" => Some(9),
                            "pitch" => Some(10),
                            "waitFactor" => Some(11),
                            _ => None,
                        };
                        if let (Some(id), Some(ref f)) = (param_int, &set_int) {
                            if let Some(v) = val.as_i64() {
                                f.call(&mut *caller, &[Val::I32(id), Val::I32(v as i32)], &mut [Val::I32(0)]).ok();
                            }
                        }
                        // String params
                        let param_str = match key.as_str() {
                            "language" => Some(1),
                            "name" => Some(2),
                            "vop" => Some(3),
                            _ => None,
                        };
                        if let (Some(id), Some(ref f)) = (param_str, &set_str) {
                            if let Some(s) = val.as_str() {
                                // Allocate string in WASM memory
                                let malloc = caller.get_export("_malloc")
                                    .and_then(|e| e.into_func());
                                if let Some(m) = malloc {
                                    let mut r = [Val::I32(0)];
                                    m.call(&mut *caller, &[Val::I32(s.len() as i32 + 1)], &mut r).ok();
                                    let sptr = r[0].unwrap_i32();
                                    let mem = get_memory(caller);
                                    mem.write(&mut *caller, sptr as usize, s.as_bytes()).ok();
                                    mem.write(&mut *caller, sptr as usize + s.len(), &[0u8]).ok();
                                    f.call(&mut *caller, &[Val::I32(id), Val::I32(sptr)], &mut [Val::I32(0)]).ok();
                                }
                            }
                        }
                    }
                }
            }
            0
        }

        // [5] Init/release completion notification
        5 => {
            let request = args.first().copied().unwrap_or(0);
            let state = args.get(1).copied().unwrap_or(0);
            let complete = args.get(2).copied().unwrap_or(0);
            let msg_ptr = args.get(3).copied().unwrap_or(0);
            eprintln!(
                "[asm_const 5] completion: request={:#x} state={:#x} complete={}",
                request as u32, state as u32, complete
            );
            if complete == 1 {
                caller.data_mut().init_complete = true;
            }
            0
        }

        // [6] Asset manager progress notification
        6 => {
            let task = args.get(2).copied().unwrap_or(0);
            eprintln!("[asm_const 6] asset progress: task={}", task);
            0
        }

        // [7] *** AUDIO BUFFER OUTPUT ***
        //     args: request, buf_ptr, buf_len, state, task, completeCode, msg, vrate
        7 => {
            let buf_byte_ptr = args.get(1).copied().unwrap_or(0) as usize;
            let buf_byte_len = args.get(2).copied().unwrap_or(0) as usize;
            let complete_code = args.get(5).copied().unwrap_or(0);
            let vrate = args.get(7).copied().unwrap_or(0);
            let num_samples = buf_byte_len / 2;

            if num_samples > 0 {
                let mem = get_memory(caller);
                let data = mem.data(&*caller);
                let samples: Vec<i16> = (0..num_samples)
                    .map(|i| {
                        let off = buf_byte_ptr + i * 2;
                        i16::from_le_bytes([data[off], data[off + 1]])
                    })
                    .collect();

                let total_before = caller.data().audio_samples.len();
                let nonzero = samples.iter().filter(|&&s| s != 0).count();
                let max_abs = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
                if total_before < 3 * 2048 {
                    // Also dump raw bytes to verify memory read
                    let raw = &data[buf_byte_ptr..buf_byte_ptr + 32.min(buf_byte_len)];
                    eprintln!("[asm_const 7] chunk#{} @{:#x} nonzero={}/{} max={} raw={:02x?}",
                        total_before / 2048, buf_byte_ptr, nonzero, num_samples, max_abs,
                        &raw[..16.min(raw.len())]);
                }

                caller.data_mut().audio_samples.extend_from_slice(&samples);
            }

            if complete_code == 1 {
                caller.data_mut().speak_complete = true;
                caller.data_mut().needs_more_audio = false;
                eprintln!("[asm_const 7] audio DONE: {} total samples",
                    caller.data().audio_samples.len());
            }
            0
        }

        // [8] JSON object response — includes voice list after init
        8 => {
            let complete = args.get(2).copied().unwrap_or(0);
            let obj_ptr = args.get(4).copied().unwrap_or(0) as u32;
            let mem = get_memory(caller);
            let obj_str = if obj_ptr > 0 { read_cstring(&mem, &*caller, obj_ptr) } else { String::new() };
            eprintln!("[asm_const 8] obj complete={}: {}", complete, &obj_str[..obj_str.len().min(500)]);
            if complete == 1 {
                caller.data_mut().init_complete = true;
            }
            0
        }

        // [9] Word/bookmark marker
        9 => 0,

        // [10] Lipsync data
        10 => 0,

        // [11] *** INITIALIZE ASSETS ***
        //      Load files into WASM heap, mount to /cfdir via MEMFS-style approach,
        //      then signal init complete.
        11 => {
            eprintln!("[asm_const 11] InitializeAssets");
            if let Err(e) = load_assets_into_heap(caller) {
                eprintln!("[asm_const 11] ERROR: {:#}", e);
                return -1;
            }
            0
        }

        // [12] Init step
        12 => {
            eprintln!("[asm_const 12] init step");
            0
        }

        // [13] Init step
        13 => {
            eprintln!("[asm_const 13] init step");
            0
        }

        // [14] *** FILE OPEN ***
        14 => {
            let path_ptr = args.first().copied().unwrap_or(0) as u32;
            let mem = get_memory(caller);
            let path = read_cstring(&mem, &*caller, path_ptr);
            eprintln!("[asm_const 14] open: {}", path);

            // Try the path as-is first (absolute paths from localroot),
            // then fall back to voice_dir/filename
            let file_path = if std::path::Path::new(&path).exists() {
                std::path::PathBuf::from(&path)
            } else {
                let filename = path.rsplit('/').next().unwrap_or(&path);
                caller.data().voice_dir.join(filename)
            };

            match std::fs::read(&file_path) {
                Ok(data) => {
                    let fd = caller.data().next_fd;
                    caller.data_mut().next_fd += 1;
                    caller.data_mut().files.insert(fd, VFile { data, position: 0 });
                    eprintln!("[asm_const 14] opened fd={}", fd);
                    fd
                }
                Err(e) => {
                    eprintln!("[asm_const 14] open failed: {} ({})", file_path.display(), e);
                    0
                }
            }
        }

        // [15] *** FILE CLOSE ***
        15 => {
            let fd = args.first().copied().unwrap_or(0);
            if fd != 0 {
                caller.data_mut().files.remove(&fd);
            }
            0
        }

        // [16] *** FILE READ ***
        // (fd, size, count, buf_ptr) -> bytes_read
        16 => {
            let fd = args.first().copied().unwrap_or(0);
            let size = args.get(1).copied().unwrap_or(0) as usize;
            let count = args.get(2).copied().unwrap_or(0) as usize;
            let buf_ptr = args.get(3).copied().unwrap_or(0) as u32;
            let total = size * count;

            // Read data from virtual file
            let file_data = {
                if let Some(file) = caller.data_mut().files.get_mut(&fd) {
                    let remaining = file.data.len() - file.position;
                    let to_read = total.min(remaining);
                    let chunk = file.data[file.position..file.position + to_read].to_vec();
                    file.position += to_read;
                    Some(chunk)
                } else {
                    None
                }
            };

            if let Some(data) = file_data {
                let nz = data.iter().filter(|&&b| b != 0).count();
                if data.len() > 100 {
                    eprintln!("[asm_const 16] read fd={} {}B → buf@{:#x} (nonzero: {}/{})",
                        fd, data.len(), buf_ptr, nz, data.len());
                }
                let mem = get_memory(caller);
                mem.write(&mut *caller, buf_ptr as usize, &data).unwrap();
                data.len() as i32
            } else {
                0
            }
        }

        // [17] *** FILE SEEK ***
        // (fd, offset, whence) -> result
        17 => {
            let fd = args.first().copied().unwrap_or(0);
            let offset = args.get(1).copied().unwrap_or(0) as i64;
            let whence = args.get(2).copied().unwrap_or(0);

            if let Some(file) = caller.data_mut().files.get_mut(&fd) {
                let new_pos = match whence {
                    0 => offset,                              // SEEK_SET
                    1 => file.position as i64 + offset,       // SEEK_CUR
                    2 => file.data.len() as i64 + offset,     // SEEK_END
                    _ => return 0x80000104u32 as i32,         // error
                };
                if new_pos < 0 {
                    return 0x80000104u32 as i32;
                }
                file.position = new_pos as usize;
                0
            } else {
                0x80000104u32 as i32
            }
        }

        // [18] *** FILE SIZE ***
        18 => {
            let fd = args.first().copied().unwrap_or(0);
            if let Some(file) = caller.data().files.get(&fd) {
                file.data.len() as i32
            } else {
                0
            }
        }

        // [19] *** GET ASSET LENGTH BY NAME ***
        19 => {
            let name_ptr = args.first().copied().unwrap_or(0) as u32;
            let mem = get_memory(caller);
            let name = read_cstring(&mem, &*caller, name_ptr);
            if let Some(&(_ptr, len)) = caller.data().assets.get(&name) {
                len as i32
            } else {
                eprintln!("[asm_const 19] asset not found: {}", name);
                0
            }
        }

        // [20] *** GET ASSET POINTER BY NAME ***
        20 => {
            let name_ptr = args.first().copied().unwrap_or(0) as u32;
            let mem = get_memory(caller);
            let name = read_cstring(&mem, &*caller, name_ptr);
            if let Some(&(ptr, _len)) = caller.data().assets.get(&name) {
                ptr as i32
            } else {
                eprintln!("[asm_const 20] asset not found: {}", name);
                0
            }
        }

        // [21] Look up file by name, write its local path to a buffer
        //      args: (outBufPtr, namePtr, outBufSize)
        21 => {
            let out_ptr = args.first().copied().unwrap_or(0) as u32;
            let name_ptr = args.get(1).copied().unwrap_or(0) as u32;
            let out_size = args.get(2).copied().unwrap_or(0) as usize;
            let mem = get_memory(caller);
            let name = read_cstring(&mem, &*caller, name_ptr);
            // Return /cfdir/<name> as the local path
            let local_path = format!("/cfdir/{}", name);
            eprintln!("[asm_const 21] lookup '{}' → '{}'", name, local_path);
            if out_ptr != 0 && out_size > 0 {
                let bytes = local_path.as_bytes();
                let write_len = bytes.len().min(out_size - 1);
                mem.write(&mut *caller, out_ptr as usize, &bytes[..write_len]).unwrap();
                mem.write(&mut *caller, out_ptr as usize + write_len, &[0u8]).unwrap();
            }
            0
        }

        // [22-24] Additional entries — stub
        _ => {
            eprintln!("[asm_const {}] UNHANDLED (args: {:?})", idx, args);
            0
        }
    }
}

/// Load all voice data files into WASM heap via _tts_malloc and notify engine.
fn load_assets_into_heap(caller: &mut Caller<'_, State>) -> Result<()> {
    let voice_dir = caller.data().voice_dir.clone();

    // Read all files from voice data directory
    let entries: Vec<(String, Vec<u8>)> = std::fs::read_dir(&voice_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let data = std::fs::read(e.path()).unwrap();
            (name, data)
        })
        .collect();

    eprintln!("[load_assets] {} files from {}", entries.len(), voice_dir.display());

    // Get _tts_malloc export for allocating heap space
    let tts_malloc = caller
        .get_export("_tts_malloc")
        .and_then(|e| e.into_func())
        .context("missing _tts_malloc export")?;

    let asset_notify = caller
        .get_export("_asset_manager_notify")
        .and_then(|e| e.into_func())
        .context("missing _asset_manager_notify export")?;

    // Pre-load ALL files into WASM heap and notify engine for each
    for (name, data) in &entries {
        let size = data.len() as i32;

        let mut results = [Val::I32(0)];
        tts_malloc.call(&mut *caller, &[Val::I32(size)], &mut results)?;
        let data_ptr = results[0].unwrap_i32();

        if data_ptr == 0 {
            eprintln!("[load_assets] tts_malloc({}) returned NULL for {}", size, name);
            continue;
        }

        let memory = get_memory(caller);
        memory.write(&mut *caller, data_ptr as usize, data)?;

        caller
            .data_mut()
            .assets
            .insert(name.clone(), (data_ptr as u32, size as u32));

        eprintln!("[load_assets] {} → heap@{:#x} ({}B)", name, data_ptr, size);

        // Allocate filename string in WASM memory
        let name_bytes = name.as_bytes();
        let mut name_results = [Val::I32(0)];
        tts_malloc.call(
            &mut *caller,
            &[Val::I32(name_bytes.len() as i32 + 1)],
            &mut name_results,
        )?;
        let name_ptr = name_results[0].unwrap_i32();
        let memory = get_memory(caller);
        memory.write(&mut *caller, name_ptr as usize, name_bytes)?;
        memory.write(&mut *caller, name_ptr as usize + name_bytes.len(), &[0u8])?;

        // Skip per-file notifications (they cause crashes).
        // Engine will find files via asm_const [19]/[20].
    }

    // Signal all assets are ready
    let notify_complete = caller
        .get_export("_asset_manager_notify_init_complete")
        .and_then(|e| e.into_func())
        .context("missing _asset_manager_notify_init_complete export")?;

    eprintln!("[load_assets] all files loaded, calling _asset_manager_notify_init_complete");
    notify_complete.call(&mut *caller, &[], &mut [])?;

    Ok(())
}

// ── Import Definitions ──────────────────────────────────────────────────────

fn define_imports(linker: &mut Linker<State>, engine: &Engine) -> Result<()> {
    // ── Math ────────────────────────────────────────────────────────────
    linker.func_wrap("global.Math", "exp", |_: Caller<'_, State>, x: f64| -> f64 {
        x.exp()
    })?;
    linker.func_wrap("global.Math", "log", |_: Caller<'_, State>, x: f64| -> f64 {
        x.ln()
    })?;
    linker.func_wrap(
        "global.Math",
        "pow",
        |_: Caller<'_, State>, b: f64, e: f64| -> f64 { b.powf(e) },
    )?;

    // ── LLVM math ───────────────────────────────────────────────────────
    linker.func_wrap("env", "_llvm_cos_f64", |_: Caller<'_, State>, x: f64| -> f64 {
        x.cos()
    })?;
    linker.func_wrap("env", "_llvm_sin_f64", |_: Caller<'_, State>, x: f64| -> f64 {
        x.sin()
    })?;

    // ── Abort / error ───────────────────────────────────────────────────
    linker.func_wrap("env", "abort", |_: Caller<'_, State>, code: i32| {
        eprintln!("[ABORT] code={}", code);
    })?;
    linker.func_wrap(
        "env",
        "abortOnCannotGrowMemory",
        |_: Caller<'_, State>| -> i32 {
            eprintln!("[ABORT] cannot grow memory");
            0
        },
    )?;

    // ── Temp return (setjmp/longjmp support) ────────────────────────────
    linker.func_wrap("env", "setTempRet0", |mut c: Caller<'_, State>, v: i32| {
        c.data_mut().temp_ret = v;
    })?;
    linker.func_wrap("env", "getTempRet0", |c: Caller<'_, State>| -> i32 {
        c.data().temp_ret
    })?;

    // ── longjmp ─────────────────────────────────────────────────────────
    linker.func_wrap(
        "env",
        "_longjmp",
        |_c: Caller<'_, State>, _env: i32, _val: i32| {
            // Signal longjmp via trap — invoke_vii will catch it
            // For now just log; we'll handle this if the engine actually uses it
            eprintln!("[longjmp] env={} val={}", _env, _val);
        },
    )?;

    // ── invoke_vii (indirect call with exception handling) ──────────────
    linker.func_wrap(
        "env",
        "invoke_vii",
        |mut caller: Caller<'_, State>, index: i32, a1: i32, a2: i32| {
            let table = caller
                .get_export("table")
                .unwrap()
                .into_table()
                .unwrap();
            let func_ref = table.get(&mut caller, index as u64);
            if let Some(val) = func_ref {
                if let Some(func) = val.unwrap_func() {
                    let typed = func.typed::<(i32, i32), ()>(&caller);
                    match typed {
                        Ok(f) => {
                            if let Err(e) = f.call(&mut caller, (a1, a2)) {
                                eprintln!("[invoke_vii] trap: {}", e);
                                caller.data_mut().temp_ret = 1;
                            }
                        }
                        Err(e) => eprintln!("[invoke_vii] type error: {}", e),
                    }
                }
            }
        },
    )?;

    // ── Locks (no-op, single threaded) ──────────────────────────────────
    linker.func_wrap("env", "___lock", |_: Caller<'_, State>, _: i32| {})?;
    linker.func_wrap("env", "___unlock", |_: Caller<'_, State>, _: i32| {})?;

    // ── errno ───────────────────────────────────────────────────────────
    linker.func_wrap("env", "___setErrNo", |_: Caller<'_, State>, _e: i32| {})?;

    // ── pthread stubs ───────────────────────────────────────────────────
    linker.func_wrap(
        "env",
        "_pthread_mutex_init",
        |_: Caller<'_, State>, _a: i32, _b: i32| -> i32 { 0 },
    )?;
    linker.func_wrap(
        "env",
        "_pthread_mutex_destroy",
        |_: Caller<'_, State>, _a: i32| -> i32 { 0 },
    )?;

    // ── Time ────────────────────────────────────────────────────────────
    linker.func_wrap(
        "env",
        "_times",
        |_: Caller<'_, State>, _buf: i32| -> i32 { 0 },
    )?;
    linker.func_wrap(
        "env",
        "_sysconf",
        |_: Caller<'_, State>, name: i32| -> i32 {
            match name {
                30 => 65536, // _SC_PAGESIZE
                _ => -1,
            }
        },
    )?;
    linker.func_wrap(
        "env",
        "_gettimeofday",
        |mut caller: Caller<'_, State>, tv: i32, _tz: i32| -> i32 {
            if tv != 0 {
                let mem = get_memory(&mut caller);
                write_i32(&mem, &mut caller, tv as u32, 0);
                write_i32(&mem, &mut caller, tv as u32 + 4, 0);
            }
            0
        },
    )?;
    linker.func_wrap(
        "env",
        "_ftime",
        |_: Caller<'_, State>, _tp: i32| -> i32 { 0 },
    )?;

    // ── Emscripten memory ───────────────────────────────────────────────
    linker.func_wrap(
        "env",
        "_emscripten_get_heap_size",
        |mut caller: Caller<'_, State>| -> i32 {
            let mem = get_memory(&mut caller);
            (mem.size(&caller) as i32) * 65536
        },
    )?;

    linker.func_wrap(
        "env",
        "_emscripten_resize_heap",
        |mut caller: Caller<'_, State>, requested: i32| -> i32 {
            let mem = get_memory(&mut caller);
            let current_pages = mem.size(&caller) as u32;
            let current_bytes = current_pages * 65536;
            if (requested as u32) <= current_bytes {
                return 1; // already large enough
            }
            let needed_pages =
                ((requested as u32 + 65535) / 65536).saturating_sub(current_pages);
            match mem.grow(&mut caller, needed_pages as u64) {
                Ok(_) => {
                    eprintln!(
                        "[resize_heap] grew by {} pages (now {})",
                        needed_pages,
                        current_pages + needed_pages
                    );
                    1
                }
                Err(e) => {
                    eprintln!("[resize_heap] FAILED: {}", e);
                    0
                }
            }
        },
    )?;

    linker.func_wrap(
        "env",
        "_emscripten_memcpy_big",
        |mut caller: Caller<'_, State>, dest: i32, src: i32, num: i32| -> i32 {
            let mem = get_memory(&mut caller);
            // Check if copying to audio buffer region (near 0x610000)
            if dest >= 0x610000 && dest < 0x620000 && num > 100 {
                let src_data = &mem.data(&caller)[src as usize..(src as usize + 32).min(mem.data_size(&caller))];
                let nonzero = src_data.iter().filter(|&&b| b != 0).count();
                eprintln!("[memcpy_big] dest={:#x} src={:#x} len={} src_nonzero={}/32",
                    dest, src, num, nonzero);
            }
            let data = mem.data(&caller)[src as usize..(src + num) as usize].to_vec();
            mem.write(&mut caller, dest as usize, &data).unwrap();
            dest
        },
    )?;

    // ── Emscripten async wget (stub) ────────────────────────────────────
    linker.func_new(
        "env",
        "_emscripten_async_wget2",
        FuncType::new(
            engine,
            vec![ValType::I32; 8],
            vec![ValType::I32],
        ),
        |_caller, _params, results| {
            eprintln!("[async_wget2] stub called");
            results[0] = Val::I32(0);
            Ok(())
        },
    )?;

    linker.func_wrap(
        "env",
        "_emscripten_async_wget2_abort",
        |_: Caller<'_, State>, _handle: i32| {},
    )?;

    // ── Config queries ──────────────────────────────────────────────────
    linker.func_wrap(
        "env",
        "_assetsLocal",
        |_: Caller<'_, State>| -> i32 {
            1 // Local mode: engine uses asm_const[14] for file I/O
        },
    )?;
    linker.func_wrap(
        "env",
        "_onlyPlayback",
        |_: Caller<'_, State>| -> i32 { 0 },
    )?;
    linker.func_wrap(
        "env",
        "_dataFileMapping",
        |_: Caller<'_, State>| -> i32 { 0 },
    )?;
    // _getLocalPipelineHeaders: Returns the CONTENT of all .hdr files
    // concatenated, allocated in WASM memory. This is what the JS does:
    //   z_ah += fs.readFileSync(hdrPath, {encoding: 'utf8'});
    linker.func_wrap(
        "env",
        "_getLocalPipelineHeaders",
        |mut caller: Caller<'_, State>| -> i32 {
            let voice_dir = caller.data().voice_dir.clone();
            let mut hdr_content = String::new();

            if let Ok(rd) = std::fs::read_dir(&voice_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    if entry
                        .path()
                        .extension()
                        .map(|x| x == "hdr")
                        .unwrap_or(false)
                    {
                        if let Ok(content) = std::fs::read_to_string(entry.path()) {
                            hdr_content.push_str(&content);
                        }
                    }
                }
            }

            if hdr_content.is_empty() {
                eprintln!("[getLocalPipelineHeaders] no .hdr content found");
                return 0;
            }

            eprintln!(
                "[getLocalPipelineHeaders] returning {}B of pipeline XML",
                hdr_content.len()
            );

            // Allocate in WASM memory
            let malloc_fn = caller
                .get_export("_malloc")
                .and_then(|e| e.into_func())
                .unwrap();
            let mut results = [Val::I32(0)];
            malloc_fn
                .call(
                    &mut caller,
                    &[Val::I32(hdr_content.len() as i32 + 1)],
                    &mut results,
                )
                .unwrap();
            let ptr = results[0].unwrap_i32();

            let mem = get_memory(&mut caller);
            mem.write(&mut caller, ptr as usize, hdr_content.as_bytes())
                .unwrap();
            mem.write(&mut caller, ptr as usize + hdr_content.len(), &[0u8])
                .unwrap();

            ptr
        },
    )?;

    // ── Syscalls ────────────────────────────────────────────────────────
    // writev (146) — capture stdout/stderr
    linker.func_wrap(
        "env",
        "___syscall146",
        |mut caller: Caller<'_, State>, _which: i32, varargs: i32| -> i32 {
            let mem = get_memory(&mut caller);
            let fd = read_i32(&mem, &caller, varargs as u32);
            let iov = read_i32(&mem, &caller, varargs as u32 + 4);
            let iovcnt = read_i32(&mem, &caller, varargs as u32 + 8);
            let mut total = 0i32;
            for i in 0..iovcnt {
                let base = read_i32(&mem, &caller, (iov + i * 8) as u32);
                let len = read_i32(&mem, &caller, (iov + i * 8 + 4) as u32);
                let text =
                    &mem.data(&caller)[base as usize..(base + len) as usize];
                if fd == 1 || fd == 2 {
                    eprint!("{}", String::from_utf8_lossy(text));
                }
                total += len;
            }
            total
        },
    )?;

    // ── syscall5 (open) ────────────────────────────────────────────────
    linker.func_wrap(
        "env",
        "___syscall5",
        |mut caller: Caller<'_, State>, _which: i32, varargs: i32| -> i32 {
            let mem = get_memory(&mut caller);
            let path_ptr = read_i32(&mem, &caller, varargs as u32) as u32;
            let _flags = read_i32(&mem, &caller, varargs as u32 + 4);
            let path = read_cstring(&mem, &caller, path_ptr);
            eprintln!("[syscall5 open] path={}", path);

            // /cfdir directory
            let trimmed = path.trim_end_matches('/');
            if trimmed == "/cfdir" {
                let fd = caller.data().next_fd;
                caller.data_mut().next_fd += 1;
                // position=999999 → getdents returns 0 immediately (skip dir listing)
                caller.data_mut().files.insert(fd, VFile { data: Vec::new(), position: 999999 });
                eprintln!("[syscall5 open] /cfdir → fd={}", fd);
                return fd;
            }

            // Strip /cfdir/ prefix and look up in voice dir
            let clean = path.trim_start_matches("/cfdir/")
                .trim_start_matches("/cfdir");
            let filename = clean.rsplit('/').next().unwrap_or(clean);
            let voice_dir = caller.data().voice_dir.clone();
            let file_path = voice_dir.join(filename);

            match std::fs::read(&file_path) {
                Ok(data) => {
                    let fd = caller.data().next_fd;
                    caller.data_mut().next_fd += 1;
                    caller.data_mut().files.insert(fd, VFile { data, position: 0 });
                    eprintln!("[syscall5 open] fd={} size={}", fd, caller.data().files[&fd].data.len());
                    fd
                }
                Err(_) => {
                    eprintln!("[syscall5 open] NOT FOUND: {}", file_path.display());
                    -1
                }
            }
        },
    )?;

    // ── syscall3 (read) ─────────────────────────────────────────────────
    linker.func_wrap(
        "env",
        "___syscall3",
        |mut caller: Caller<'_, State>, _which: i32, varargs: i32| -> i32 {
            let mem = get_memory(&mut caller);
            let fd = read_i32(&mem, &caller, varargs as u32);
            let buf = read_i32(&mem, &caller, varargs as u32 + 4) as u32;
            let count = read_i32(&mem, &caller, varargs as u32 + 8) as usize;

            let chunk = {
                if let Some(file) = caller.data_mut().files.get_mut(&fd) {
                    let remaining = file.data.len() - file.position;
                    let to_read = count.min(remaining);
                    let data = file.data[file.position..file.position + to_read].to_vec();
                    file.position += to_read;
                    Some(data)
                } else {
                    None
                }
            };

            if let Some(data) = chunk {
                let mem = get_memory(&mut caller);
                mem.write(&mut caller, buf as usize, &data).unwrap();
                data.len() as i32
            } else {
                -1
            }
        },
    )?;

    // ── syscall6 (close) ────────────────────────────────────────────────
    linker.func_wrap(
        "env",
        "___syscall6",
        |mut caller: Caller<'_, State>, _which: i32, varargs: i32| -> i32 {
            let mem = get_memory(&mut caller);
            let fd = read_i32(&mem, &caller, varargs as u32);
            caller.data_mut().files.remove(&fd);
            0
        },
    )?;

    // ── syscall140 (lseek) ──────────────────────────────────────────────
    linker.func_wrap(
        "env",
        "___syscall140",
        |mut caller: Caller<'_, State>, _which: i32, varargs: i32| -> i32 {
            let mem = get_memory(&mut caller);
            let fd = read_i32(&mem, &caller, varargs as u32);
            let offset_high = read_i32(&mem, &caller, varargs as u32 + 4);
            let offset_low = read_i32(&mem, &caller, varargs as u32 + 8);
            let result_ptr = read_i32(&mem, &caller, varargs as u32 + 12) as u32;
            let whence = read_i32(&mem, &caller, varargs as u32 + 16);
            let offset = ((offset_high as i64) << 32) | (offset_low as u32 as i64);

            let new_pos = {
                if let Some(file) = caller.data_mut().files.get_mut(&fd) {
                    let pos = match whence {
                        0 => offset,
                        1 => file.position as i64 + offset,
                        2 => file.data.len() as i64 + offset,
                        _ => return -1,
                    };
                    file.position = pos.max(0) as usize;
                    Some(file.position)
                } else {
                    None
                }
            };
            if let Some(pos) = new_pos {
                let mem = get_memory(&mut caller);
                write_i32(&mem, &mut caller, result_ptr, pos as i32);
                write_i32(&mem, &mut caller, result_ptr + 4, 0);
                0
            } else {
                -1
            }
        },
    )?;

    // ── syscall145 (readv) ──────────────────────────────────────────────
    linker.func_wrap(
        "env",
        "___syscall145",
        |mut caller: Caller<'_, State>, _which: i32, varargs: i32| -> i32 {
            let mem = get_memory(&mut caller);
            let fd = read_i32(&mem, &caller, varargs as u32);
            let iov = read_i32(&mem, &caller, varargs as u32 + 4);
            let iovcnt = read_i32(&mem, &caller, varargs as u32 + 8);
            let mut total = 0i32;

            for i in 0..iovcnt {
                let base = read_i32(&mem, &caller, (iov + i * 8) as u32) as u32;
                let len = read_i32(&mem, &caller, (iov + i * 8 + 4) as u32) as usize;

                let chunk = {
                    if let Some(file) = caller.data_mut().files.get_mut(&fd) {
                        let remaining = file.data.len() - file.position;
                        let to_read = len.min(remaining);
                        let data = file.data[file.position..file.position + to_read].to_vec();
                        file.position += to_read;
                        Some(data)
                    } else {
                        None
                    }
                };

                if let Some(data) = chunk {
                    let mem = get_memory(&mut caller);
                    mem.write(&mut caller, base as usize, &data).unwrap();
                    total += data.len() as i32;
                }
            }
            total
        },
    )?;

    // ── syscall195 (stat64) ─────────────────────────────────────────────
    linker.func_wrap(
        "env",
        "___syscall195",
        |mut caller: Caller<'_, State>, _which: i32, varargs: i32| -> i32 {
            let mem = get_memory(&mut caller);
            let path_ptr = read_i32(&mem, &caller, varargs as u32) as u32;
            let buf_ptr = read_i32(&mem, &caller, varargs as u32 + 4) as u32;
            let path = read_cstring(&mem, &caller, path_ptr);

            // Handle /cfdir as a directory
            let trimmed = path.trim_end_matches('/');
            if trimmed == "/cfdir" || trimmed.is_empty() {
                let mem = get_memory(&mut caller);
                let zeros = [0u8; 96];
                mem.write(&mut caller, buf_ptr as usize, &zeros).unwrap();
                // st_mode = directory (S_IFDIR | 0755)
                write_i32(&mem, &mut caller, buf_ptr + 8, 0o40755);
                return 0;
            }

            // Strip /cfdir/ prefix
            let clean = path.trim_start_matches("/cfdir/")
                .trim_start_matches("/cfdir");
            let filename = clean.rsplit('/').next().unwrap_or(clean);

            if filename.is_empty() {
                return -1;
            }

            let voice_dir = caller.data().voice_dir.clone();
            let file_path = voice_dir.join(filename);

            match std::fs::metadata(&file_path) {
                Ok(meta) => {
                    let mem = get_memory(&mut caller);
                    let zeros = [0u8; 96];
                    mem.write(&mut caller, buf_ptr as usize, &zeros).unwrap();
                    write_i32(&mem, &mut caller, buf_ptr + 40, meta.len() as i32);
                    write_i32(&mem, &mut caller, buf_ptr + 8, 0o100644);
                    0
                }
                Err(_) => {
                    eprintln!("[syscall195 stat] NOT FOUND: {}", path);
                    -1
                }
            }
        },
    )?;

    // ── syscall220 (getdents64) — list /cfdir directory ────────────────
    linker.func_wrap(
        "env",
        "___syscall220",
        |mut caller: Caller<'_, State>, _which: i32, varargs: i32| -> i32 {
            let mem = get_memory(&mut caller);
            let fd = read_i32(&mem, &caller, varargs as u32);
            let dirp = read_i32(&mem, &caller, varargs as u32 + 4) as u32;
            let count = read_i32(&mem, &caller, varargs as u32 + 8) as usize;

            // Use file.position as the directory entry offset
            let start_idx = caller
                .data()
                .files
                .get(&fd)
                .map(|f| f.position)
                .unwrap_or(0);

            let voice_dir = caller.data().voice_dir.clone();
            // Return all files including . and ..
            let mut entries: Vec<String> = vec![".".to_string(), "..".to_string()];
            if let Ok(rd) = std::fs::read_dir(&voice_dir) {
                for e in rd.filter_map(|e| e.ok()).filter(|e| e.path().is_file()) {
                    entries.push(e.file_name().to_string_lossy().to_string());
                }
            }

            if start_idx >= entries.len() {
                return 0; // end of directory
            }

            // Emscripten uses FIXED 280-byte records for getdents64
            const REC_LEN: usize = 280;
            let mut offset = 0usize;
            let mut entries_written = 0usize;

            for (i, name) in entries.iter().enumerate().skip(start_idx) {
                if offset + REC_LEN > count {
                    break;
                }

                let mem = get_memory(&mut caller);
                let base = dirp as usize + offset;
                let zeros = [0u8; REC_LEN];
                mem.write(&mut caller, base, &zeros).unwrap();
                mem.write(&mut caller, base, &(i as u64 + 1).to_le_bytes()).unwrap();
                mem.write(&mut caller, base + 8, &((i + 1) as u64).to_le_bytes())
                    .unwrap();
                mem.write(&mut caller, base + 16, &(REC_LEN as u16).to_le_bytes())
                    .unwrap();
                let d_type: u8 = if name == "." || name == ".." { 4 } else { 8 };
                mem.write(&mut caller, base + 18, &[d_type]).unwrap();
                let name_bytes = name.as_bytes();
                let name_len = name_bytes.len().min(255);
                mem.write(&mut caller, base + 19, &name_bytes[..name_len]).unwrap();

                offset += REC_LEN;
                entries_written += 1;
            }

            // Update directory position
            if let Some(file) = caller.data_mut().files.get_mut(&fd) {
                file.position = start_idx + entries_written;
            }

            eprintln!(
                "[syscall220 getdents] fd={} wrote {}/{} entries (from idx {}), {}B",
                fd, entries_written, entries.len(), start_idx, offset
            );
            offset as i32
        },
    )?;

    // Remaining syscalls — stub
    for (name, num) in [
        ("___syscall20", 20),
        ("___syscall54", 54),
        ("___syscall221", 221),
    ] {
        linker.func_wrap(
            "env",
            name,
            move |_: Caller<'_, State>, _which: i32, _varargs: i32| -> i32 {
                if num != 54 && num != 221 {
                    eprintln!("[syscall] {} ({})", name, num);
                }
                0
            },
        )?;
    }

    // ── asm_const variants ──────────────────────────────────────────────
    // Each variant has a different number of i32 params.
    // First param is always the dispatch index.
    let asm_const_variants: &[(&str, usize)] = &[
        ("_emscripten_asm_const_i", 1),
        ("_emscripten_asm_const_ii", 2),
        ("_emscripten_asm_const_iiii", 4),
        ("_emscripten_asm_const_iiiii", 5),
        ("_emscripten_asm_const_iiiiii", 6),
        ("_emscripten_asm_const_iiiiiiiii", 9),
        ("_emscripten_asm_const_iiiiiiiiiiii", 12),
        ("_emscripten_asm_const_iiiiiiiiiiiiii", 14),
        (
            "_emscripten_asm_const_iiiiiiiiiiiiiiiiiiiiii",
            22,
        ),
    ];

    for &(name, param_count) in asm_const_variants {
        linker.func_new(
            "env",
            name,
            FuncType::new(
                engine,
                vec![ValType::I32; param_count],
                vec![ValType::I32],
            ),
            |mut caller: Caller<'_, State>, params: &[Val], results: &mut [Val]| {
                let idx = params[0].unwrap_i32();
                let args: Vec<i32> =
                    params[1..].iter().map(|v| v.unwrap_i32()).collect();
                results[0] = Val::I32(asm_const_dispatch(&mut caller, idx, &args));
                Ok(())
            },
        )?;
    }

    Ok(())
}

// ── Module Instantiation ────────────────────────────────────────────────────

fn instantiate_module(
    engine: &Engine,
    module: &Module,
    state: State,
) -> Result<(Store<State>, Instance)> {
    let mut store = Store::new(engine, state);
    let mut linker = Linker::new(engine);

    define_imports(&mut linker, engine)?;

    // ── Globals ─────────────────────────────────────────────────────────
    let table_base = Global::new(
        &mut store,
        GlobalType::new(ValType::I32, Mutability::Const),
        Val::I32(0),
    )?;
    linker.define(&mut store, "env", "__table_base", table_base)?;

    let dynamictop_ptr = Global::new(
        &mut store,
        GlobalType::new(ValType::I32, Mutability::Const),
        Val::I32(0x0db8c0), // from JS analysis
    )?;
    linker.define(&mut store, "env", "DYNAMICTOP_PTR", dynamictop_ptr)?;

    let nan = Global::new(
        &mut store,
        GlobalType::new(ValType::F64, Mutability::Const),
        Val::F64(f64::NAN.to_bits()),
    )?;
    linker.define(&mut store, "global", "NaN", nan)?;

    let infinity = Global::new(
        &mut store,
        GlobalType::new(ValType::F64, Mutability::Const),
        Val::F64(f64::INFINITY.to_bits()),
    )?;
    linker.define(&mut store, "global", "Infinity", infinity)?;

    // ── Memory (1024 pages = 64 MB) ─────────────────────────────────────
    let memory = Memory::new(&mut store, MemoryType::new(1024, None))?;
    linker.define(&mut store, "env", "memory", memory)?;
    store.data_mut().memory = Some(memory);

    // ── Table (84096 funcref entries) ───────────────────────────────────
    let table = Table::new(
        &mut store,
        TableType::new(RefType::FUNCREF, 84096, Some(84096)),
        Ref::Func(None),
    )?;
    linker.define(&mut store, "env", "table", table)?;

    // ── tempDoublePtr global (used internally) ──────────────────────────
    let temp_double = Global::new(
        &mut store,
        GlobalType::new(ValType::I32, Mutability::Const),
        Val::I32(0x0db9b0), // from JS analysis
    )?;
    linker.define(&mut store, "env", "tempDoublePtr", temp_double)?;

    // ── Instantiate ─────────────────────────────────────────────────────
    let instance = linker
        .instantiate(&mut store, module)
        .context("WASM instantiation failed")?;

    // ── Initialize DYNAMICTOP_PTR in memory ─────────────────────────────
    // Write the initial dynamic base (0x5db9c0) at address 0xdb8c0
    let mem = store.data().memory.unwrap();
    write_i32(&mem, &mut store, 0x0db8c0, 0x5db9c0);
    eprintln!("[init] DYNAMICTOP_PTR@{:#x} = {:#x}", 0x0db8c0, 0x5db9c0);

    Ok((store, instance))
}

// ── TTS API ─────────────────────────────────────────────────────────────────

fn run_tts(store: &mut Store<State>, instance: &Instance, text: &str) -> Result<()> {
    // 1. Call _main()
    eprintln!("\n=== Calling _main() ===");
    let main_fn = instance.get_typed_func::<(), i32>(&mut *store, "_main")?;
    let ret = main_fn.call(&mut *store, ())?;
    eprintln!("[main] returned {}", ret);

    // 2. Build config JSON
    let voice_dir = store.data().voice_dir.clone();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&voice_dir)? {
        let entry = entry?;
        if entry.path().is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            let size = entry.metadata()?.len();
            files.push(serde_json::json!({
                "name": name,
                "size": size,
                "url": name,
            }));
        }
    }

    let config = serde_json::json!({
        "heapSize": 16 * 1024 * 1024,
        "files": files,
    });
    let config_str = config.to_string();
    eprintln!("\n=== Initializing TTS ===");
    eprintln!("[config] {} files, {}B JSON", files.len(), config_str.len());

    // 3. Build params JSON matching the JS SDK format.
    //    The 'metadata' field contains the file list (same format as files.metadata).
    let metadata = serde_json::json!({ "files": files });
    let metadata_str = metadata.to_string();

    let voice_dir_abs = std::fs::canonicalize(&voice_dir)
        .unwrap_or_else(|_| voice_dir.clone());
    let params = serde_json::json!({
        "env": "node",
        "data": "local",
        "cache": "none",
        "metadata": metadata_str,
        "localroot": voice_dir_abs.to_string_lossy()
    });
    let params_str = params.to_string();
    eprintln!("[config] params: {}", params_str);
    let params_ptr = alloc_string(store, instance, &params_str)?;

    // 4. Call _imp_ttsInitialize(ttsWorker=-1, paramsJsonPtr, requestId=1)
    let init_fn =
        instance.get_typed_func::<(i32, i32, i32), ()>(&mut *store, "_imp_ttsInitialize")?;
    eprintln!("[tts] _imp_ttsInitialize(-1, {:#x}, 1)", params_ptr);
    init_fn.call(&mut *store, (-1, params_ptr as i32, 1))?;

    // Check if our config was overwritten
    {
        let mem = store.data().memory.unwrap();
        let verify = read_cstring(&mem, &*store, params_ptr);
        eprintln!("[config] post-init at {:#x}: {}B, starts: '{}'",
            params_ptr, verify.len(), &verify[..verify.len().min(60)]);
    }
    eprintln!("[tts] init complete={}", store.data().init_complete);

    // 5. Set voice via _imp_ttsSetSpeechParams (matches ttsSetCurrentVoice in JS SDK)
    //    Signature: (i32, i32, i32) -> () = (requestId, paramsJsonPtr, ?)
    eprintln!("\n=== Setting voice ===");
    let set_str = instance.get_typed_func::<(i32, i32), i32>(&mut *store, "_ttsSetOneParamStr")?;
    let set_int = instance.get_typed_func::<(i32, i32), i32>(&mut *store, "_ttsSetOneParamInt")?;

    // Detect voice from pipeline header filename
    let voice_dir = store.data().voice_dir.clone();
    let hdr: Option<String> = std::fs::read_dir(&voice_dir).ok().and_then(|rd| {
        rd.filter_map(|e| e.ok())
            .find(|e| e.path().extension().map(|x| x == "hdr").unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().to_string())
    });

    // Parse voice info from header filename (e.g. ve_pipeline_enu_zoe_22_embedded-compact_2-2-1.hdr)
    let (lang, voice_name, vop) = if let Some(ref h) = hdr {
        let parts: Vec<&str> = h.trim_end_matches(".hdr").split('_').collect();
        // Format: ve_pipeline_{lang}_{voice}_{rate}_{vop}_{version}
        if parts.len() >= 6 {
            let lang = parts[2]; // e.g. "enu" or "rur"
            let voice = parts[3]; // e.g. "zoe" or "yuri"
            // Capitalize first letter
            let voice_cap = format!("{}{}", &voice[..1].to_uppercase(), &voice[1..]);
            // vop is everything between voice_rate_ and _version
            let vop = parts[5..parts.len()-1].join("-");
            (lang.to_string(), voice_cap, vop)
        } else {
            ("enu".to_string(), "Zoe".to_string(), "embedded-compact".to_string())
        }
    } else {
        ("enu".to_string(), "Zoe".to_string(), "embedded-compact".to_string())
    };

    // Map langcode to full language name (must match voice list exactly)
    let language = match lang.as_str() {
        "enu" => "American English",
        "rur" => "Russian",
        _ => "American English",
    };

    let lang_ptr = alloc_string(store, instance, language)?;
    let r = set_str.call(&mut *store, (1, lang_ptr as i32))?;
    eprintln!("[tts] set language={} → {}", language, r);
    let name_ptr = alloc_string(store, instance, &voice_name)?;
    let r = set_str.call(&mut *store, (2, name_ptr as i32))?;
    eprintln!("[tts] set name={} → {}", voice_name, r);
    let vop_ptr = alloc_string(store, instance, &vop)?;
    let r = set_str.call(&mut *store, (3, vop_ptr as i32))?;
    eprintln!("[tts] set vop={} → {}", vop, r);

    // 8=volume, 9=speed, 10=pitch
    set_int.call(&mut *store, (8, 80))?;
    set_int.call(&mut *store, (9, 100))?;
    set_int.call(&mut *store, (10, 100))?;
    eprintln!("[tts] speech params: vol=80 speed=100 pitch=100");

    // Call _imp_ttsSetSpeechParams(-1, voiceJsonPtr, requestId)
    // This triggers voice loading (opens voice data files via asm_const[14])
    let voice_json = serde_json::json!({
        "name": voice_name,
        "language": language,
        "vop": vop,
    });
    let voice_json_str = voice_json.to_string();
    let voice_json_ptr = alloc_string(store, instance, &voice_json_str)?;
    let set_speech = instance.get_typed_func::<(i32, i32, i32), ()>(
        &mut *store,
        "_imp_ttsSetSpeechParams",
    )?;
    eprintln!("[tts] _imp_ttsSetSpeechParams(-1, '{}', 2)", voice_json_str);
    set_speech.call(&mut *store, (-1, voice_json_ptr as i32, 2))?;

    // 6. Speak: _imp_ttsSpeak(-1, textPtr, requestId) then _worker_ttsSpeak(0,0) loop
    eprintln!("\n=== Speaking: \"{}\" ===", text);
    let text_ptr = alloc_string(store, instance, text)?;

    let imp_speak =
        instance.get_typed_func::<(i32, i32, i32), ()>(&mut *store, "_imp_ttsSpeak")?;
    let worker_speak =
        instance.get_typed_func::<(i32, i32), ()>(&mut *store, "_worker_ttsSpeak")?;

    // JS ccall does stackSave/stackRestore around every WASM call.
    // Without this, the C stack pointer drifts and synthesis corrupts.
    let stack_save = instance.get_typed_func::<(), i32>(&mut *store, "stackSave")?;
    let stack_restore = instance.get_typed_func::<i32, ()>(&mut *store, "stackRestore")?;

    store.data_mut().needs_more_audio = false;
    store.data_mut().speak_complete = false;

    // Allocate text on WASM stack (matching JS ccall's allocateUTF8 behavior)
    let stack_alloc = instance.get_typed_func::<i32, i32>(&mut *store, "stackAlloc")?;
    let stack_save = instance.get_typed_func::<(), i32>(&mut *store, "stackSave")?;
    let sp_before = stack_save.call(&mut *store, ())?;
    let stack_text_ptr = stack_alloc.call(&mut *store, text.len() as i32 + 1)?;
    {
        let mem = store.data().memory.unwrap();
        mem.write(&mut *store, stack_text_ptr as usize, text.as_bytes())?;
        mem.write(&mut *store, stack_text_ptr as usize + text.len(), &[0u8])?;
    }
    eprintln!("[tts] text on stack at {:#x} (sp was {:#x})", stack_text_ptr, sp_before);

    imp_speak.call(&mut *store, (-1, stack_text_ptr, 3))?;

    // Continuation loop — no stackSave/stackRestore to avoid invalidating engine state
    let mut iterations = 0;
    let mut no_progress = 0;
    let mut prev_len = store.data().audio_samples.len();
    loop {
        worker_speak.call(&mut *store, (0, 0))?;
        iterations += 1;

        let cur_len = store.data().audio_samples.len();
        if cur_len > prev_len {
            no_progress = 0;
        } else {
            no_progress += 1;
        }
        prev_len = cur_len;

        if no_progress >= 200 || iterations >= 5000
            || (cur_len as u32) >= 60 * 22050
        {
            break;
        }
    }

    eprintln!(
        "[tts] synthesis: {} iterations, {} samples ({:.1}s)",
        iterations,
        store.data().audio_samples.len(),
        store.data().audio_samples.len() as f64 / 22050.0
    );

    eprintln!(
        "[tts] speak done, captured {} audio samples",
        store.data().audio_samples.len()
    );

    Ok(())
}

// ── WAV Output ──────────────────────────────────────────────────────────────

fn write_wav(samples: &[i16], sample_rate: u32, path: &PathBuf) -> Result<()> {
    if samples.is_empty() {
        bail!("No audio samples captured");
    }

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = hound::WavWriter::create(path, spec)?;
    for &sample in samples {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;

    let duration = samples.len() as f64 / sample_rate as f64;
    eprintln!(
        "[wav] wrote {} samples ({:.1}s) to {}",
        samples.len(),
        duration,
        path.display()
    );
    Ok(())
}

// ── Main ────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    eprintln!("Yuri TTS — loading WASM engine");
    eprintln!("  wasm:  {}", cli.wasm.display());
    eprintln!("  voice: {}", cli.voice_dir.display());
    eprintln!("  text:  \"{}\"", cli.text);
    eprintln!("  out:   {}", cli.output.display());

    // Load WASM module with larger stack for deep Vocalizer call chains
    let mut config = Config::new();
    config.max_wasm_stack(16 * 1024 * 1024); // 16MB WASM stack
    let engine = Engine::new(&config)?;
    let module = Module::from_file(&engine, &cli.wasm).context("Failed to load webtts.wasm")?;
    eprintln!("[wasm] module loaded ({} imports, {} exports)",
        module.imports().len(), module.exports().len());

    // Instantiate with Emscripten runtime
    let state = State::new(cli.voice_dir.clone());
    let (mut store, instance) = instantiate_module(&engine, &module, state)?;
    eprintln!("[wasm] instantiated");

    // Run TTS
    run_tts(&mut store, &instance, &cli.text)?;

    // Write WAV output
    write_wav(&store.data().audio_samples, store.data().sample_rate, &cli.output)?;

    println!(
        "Done! {} samples written to {}",
        store.data().audio_samples.len(),
        cli.output.display()
    );
    Ok(())
}
