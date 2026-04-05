use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, warn};
use wasmtime::*;

pub(crate) struct State {
    pub(crate) files: HashMap<i32, VFile>,
    pub(crate) next_fd: i32,
    pub(crate) voice_dir: PathBuf,
    pub(crate) assets: HashMap<String, (u32, u32)>, // name -> (heap_ptr, byte_len)
    pub(crate) pending_samples: Vec<i16>,
    pub(crate) temp_ret: i32,
    pub(crate) init_complete: bool,
    pub(crate) needs_more_audio: bool,
    pub(crate) memory: Option<Memory>,
}

pub(crate) struct VFile {
    pub(crate) data: Vec<u8>,
    pub(crate) position: usize,
}

impl State {
    pub(crate) fn new(voice_dir: PathBuf) -> Self {
        Self {
            files: HashMap::new(),
            next_fd: 10,
            voice_dir,
            assets: HashMap::new(),
            pending_samples: Vec::new(),
            temp_ret: 0,
            init_complete: false,
            needs_more_audio: false,
            memory: None,
        }
    }
}

/// Returns the WASM linear memory.
///
/// # Safety invariant
/// `memory` is always `Some` after `instantiate_module` sets it during WASM
/// instantiation. Every caller of `get_memory` runs inside a WASM callback
/// that can only fire after instantiation, so the unwrap is safe.
pub(crate) fn get_memory(caller: &mut Caller<'_, State>) -> Memory {
    caller.data().memory.unwrap()
}

pub(crate) fn read_cstring(memory: &Memory, store: &impl AsContext<Data = State>, ptr: u32) -> String {
    let data = memory.data(store);
    let mut end = ptr as usize;
    while end < data.len() && data[end] != 0 {
        end += 1;
    }
    String::from_utf8_lossy(&data[ptr as usize..end]).into_owned()
}

pub(crate) fn read_i32(memory: &Memory, store: &impl AsContext<Data = State>, addr: u32) -> i32 {
    let data = memory.data(store);
    let off = addr as usize;
    if off + 3 >= data.len() {
        return 0;
    }
    i32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

pub(crate) fn write_i32(memory: &Memory, store: &mut impl AsContextMut<Data = State>, addr: u32, val: i32) {
    memory
        .write(store, addr as usize, &val.to_le_bytes())
        .ok();
}

/// Write a null-terminated UTF-8 string into WASM memory via `_malloc`.
/// Returns the pointer in WASM linear memory.
pub(crate) fn alloc_string(store: &mut Store<State>, instance: &Instance, s: &str) -> Result<u32> {
    let malloc = instance
        .get_typed_func::<i32, i32>(&mut *store, "_malloc")
        .context("missing _malloc")?;
    let len = s.len() as i32 + 1;
    let ptr = malloc.call(&mut *store, len)?;
    let memory = store.data().memory.context("missing memory")?;
    memory.write(&mut *store, ptr as usize, s.as_bytes())?;
    memory.write(&mut *store, ptr as usize + s.len(), &[0u8])?;
    Ok(ptr as u32)
}

// asm_const dispatch table
//
// The WASM module calls _emscripten_asm_const_*(idx, ...) where idx selects
// a JS function from a dispatch table. We reimplement the entries in Rust.

pub(crate) fn asm_const_dispatch(caller: &mut Caller<'_, State>, idx: i32, args: &[i32]) -> i32 {
    match idx {
        // [0] Parse config JSON, return heapSize
        0 => {
            let ptr = args.first().copied().unwrap_or(0) as u32;
            let mem = get_memory(caller);
            let config_str = read_cstring(&mem, &*caller, ptr);

            if config_str.is_empty() || config_str.len() < 10 {
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
                let max_write = json.len().min(4000);
                mem.write(&mut *caller, ptr as usize, &json.as_bytes()[..max_write]).ok();
                mem.write(&mut *caller, ptr as usize + max_write, &[0u8]).ok();
            }
            64 * 1024 * 1024
        }

        // [1] assetsInHeap = true
        1 => 0,

        // [2] Extract mrk/prompt markup -- not needed
        2 => 0,

        // [3] Yield: set flag so main loop calls _worker_ttsSpeak(0,0)
        3 => {
            caller.data_mut().needs_more_audio = true;
            0
        }

        // [4] Set speech params from JSON
        4 => {
            let ptr = args.first().copied().unwrap_or(0) as u32;
            let mem = get_memory(caller);
            let json_str = read_cstring(&mem, &*caller, ptr);

            if let Ok(params) = serde_json::from_str::<serde_json::Value>(&json_str) {
                let set_int = caller.get_export("_ttsSetOneParamInt")
                    .and_then(|e| e.into_func());
                let set_str = caller.get_export("_ttsSetOneParamStr")
                    .and_then(|e| e.into_func());

                if let Some(obj) = params.as_object() {
                    for (key, val) in obj {
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
                        let param_str = match key.as_str() {
                            "language" => Some(1),
                            "name" => Some(2),
                            "vop" => Some(3),
                            _ => None,
                        };
                        if let (Some(id), Some(ref f)) = (param_str, &set_str) {
                            if let Some(s) = val.as_str() {
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
            let complete = args.get(2).copied().unwrap_or(0);
            if complete == 1 {
                caller.data_mut().init_complete = true;
            }
            0
        }

        // [6] Asset manager progress notification
        6 => 0,

        // [7] Audio buffer output
        7 => {
            let buf_byte_ptr = args.get(1).copied().unwrap_or(0) as usize;
            let buf_byte_len = args.get(2).copied().unwrap_or(0) as usize;
            let complete_code = args.get(5).copied().unwrap_or(0);
            let num_samples = buf_byte_len / 2;

            if num_samples > 0 {
                let mem = get_memory(caller);
                let data = mem.data(&*caller);
                let end = buf_byte_ptr + num_samples * 2;
                if end <= data.len() {
                    let samples: Vec<i16> = (0..num_samples)
                        .map(|i| {
                            let off = buf_byte_ptr + i * 2;
                            i16::from_le_bytes([data[off], data[off + 1]])
                        })
                        .collect();

                    caller.data_mut().pending_samples.extend_from_slice(&samples);
                }
            }

            if complete_code == 1 {
                caller.data_mut().needs_more_audio = false;
            }
            0
        }

        // [8] JSON object response (includes voice list after init)
        8 => {
            let complete = args.get(2).copied().unwrap_or(0);
            if complete == 1 {
                caller.data_mut().init_complete = true;
            }
            0
        }

        // [9] Word/bookmark marker
        9 => 0,

        // [10] Lipsync data
        10 => 0,

        // [11] Initialize assets -- signal ready for local mode
        11 => {
            let notify = caller
                .get_export("_asset_manager_notify_init_complete")
                .and_then(|e| e.into_func());
            if let Some(func) = notify {
                func.call(&mut *caller, &[], &mut []).ok();
            }
            0
        }

        // [12-13] Init steps
        12 | 13 => 0,

        // [14] File open
        14 => {
            let path_ptr = args.first().copied().unwrap_or(0) as u32;
            let mem = get_memory(caller);
            let path = read_cstring(&mem, &*caller, path_ptr);

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
                    fd
                }
                Err(e) => {
                    let name = file_path.display().to_string();
                    if name.contains("userdct") {
                        debug!(path = %name, "optional user dictionary not found");
                    } else {
                        warn!(path = %name, error = %e, "file open failed");
                    }
                    0
                }
            }
        }

        // [15] File close
        15 => {
            let fd = args.first().copied().unwrap_or(0);
            if fd != 0 {
                caller.data_mut().files.remove(&fd);
            }
            0
        }

        // [16] File read: (fd, size, count, buf_ptr) -> bytes_read
        16 => {
            let fd = args.first().copied().unwrap_or(0);
            let size = args.get(1).copied().unwrap_or(0) as usize;
            let count = args.get(2).copied().unwrap_or(0) as usize;
            let buf_ptr = args.get(3).copied().unwrap_or(0) as u32;
            let total = size * count;

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
                let mem = get_memory(caller);
                mem.write(&mut *caller, buf_ptr as usize, &data).ok();
                data.len() as i32
            } else {
                0
            }
        }

        // [17] File seek: (fd, offset, whence) -> result
        17 => {
            let fd = args.first().copied().unwrap_or(0);
            let offset = args.get(1).copied().unwrap_or(0) as i64;
            let whence = args.get(2).copied().unwrap_or(0);

            if let Some(file) = caller.data_mut().files.get_mut(&fd) {
                let new_pos = match whence {
                    0 => offset,                              // SEEK_SET
                    1 => file.position as i64 + offset,       // SEEK_CUR
                    2 => file.data.len() as i64 + offset,     // SEEK_END
                    _ => return 0x80000104u32 as i32,
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

        // [18] File size
        18 => {
            let fd = args.first().copied().unwrap_or(0);
            if let Some(file) = caller.data().files.get(&fd) {
                file.data.len() as i32
            } else {
                0
            }
        }

        // [19] Get asset length by name
        19 => {
            let name_ptr = args.first().copied().unwrap_or(0) as u32;
            let mem = get_memory(caller);
            let name = read_cstring(&mem, &*caller, name_ptr);
            if let Some(&(_ptr, len)) = caller.data().assets.get(&name) {
                len as i32
            } else {
                0
            }
        }

        // [20] Get asset pointer by name
        20 => {
            let name_ptr = args.first().copied().unwrap_or(0) as u32;
            let mem = get_memory(caller);
            let name = read_cstring(&mem, &*caller, name_ptr);
            if let Some(&(ptr, _len)) = caller.data().assets.get(&name) {
                ptr as i32
            } else {
                0
            }
        }

        // [21] Look up file by name, write its local path to a buffer
        21 => {
            let out_ptr = args.first().copied().unwrap_or(0) as u32;
            let name_ptr = args.get(1).copied().unwrap_or(0) as u32;
            let out_size = args.get(2).copied().unwrap_or(0) as usize;
            let mem = get_memory(caller);
            let name = read_cstring(&mem, &*caller, name_ptr);
            let local_path = format!("/cfdir/{}", name);
            if out_ptr != 0 && out_size > 0 {
                let bytes = local_path.as_bytes();
                let write_len = bytes.len().min(out_size - 1);
                mem.write(&mut *caller, out_ptr as usize, &bytes[..write_len]).ok();
                mem.write(&mut *caller, out_ptr as usize + write_len, &[0u8]).ok();
            }
            0
        }

        _ => 0,
    }
}

pub(crate) fn define_imports(linker: &mut Linker<State>, engine: &wasmtime::Engine) -> Result<()> {
    // Math builtins
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

    // LLVM math intrinsics
    linker.func_wrap("env", "_llvm_cos_f64", |_: Caller<'_, State>, x: f64| -> f64 {
        x.cos()
    })?;
    linker.func_wrap("env", "_llvm_sin_f64", |_: Caller<'_, State>, x: f64| -> f64 {
        x.sin()
    })?;

    // Abort / error
    linker.func_wrap("env", "abort", |_: Caller<'_, State>, code: i32| {
        warn!(code, "WASM abort");
    })?;
    linker.func_wrap(
        "env",
        "abortOnCannotGrowMemory",
        |_: Caller<'_, State>| -> i32 {
            warn!("cannot grow WASM memory");
            0
        },
    )?;

    // Temp return (setjmp/longjmp support)
    linker.func_wrap("env", "setTempRet0", |mut c: Caller<'_, State>, v: i32| {
        c.data_mut().temp_ret = v;
    })?;
    linker.func_wrap("env", "getTempRet0", |c: Caller<'_, State>| -> i32 {
        c.data().temp_ret
    })?;

    // longjmp stub
    linker.func_wrap(
        "env",
        "_longjmp",
        |_: Caller<'_, State>, _env: i32, _val: i32| {},
    )?;

    // invoke_vii (indirect call with exception handling)
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
                            if let Err(_e) = f.call(&mut caller, (a1, a2)) {
                                caller.data_mut().temp_ret = 1;
                            }
                        }
                        Err(e) => warn!(error = %e, "invoke_vii type error"),
                    }
                }
            }
        },
    )?;

    // Locks (no-op, single threaded)
    linker.func_wrap("env", "___lock", |_: Caller<'_, State>, _: i32| {})?;
    linker.func_wrap("env", "___unlock", |_: Caller<'_, State>, _: i32| {})?;

    // errno
    linker.func_wrap("env", "___setErrNo", |_: Caller<'_, State>, _e: i32| {})?;

    // pthread stubs
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

    // Time
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

    // Emscripten memory management
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
                return 1;
            }
            let needed_pages =
                ((requested as u32 + 65535) / 65536).saturating_sub(current_pages);
            match mem.grow(&mut caller, needed_pages as u64) {
                Ok(_) => 1,
                Err(e) => {
                    warn!(error = %e, "resize_heap failed");
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
            let data = mem.data(&caller)[src as usize..(src + num) as usize].to_vec();
            mem.write(&mut caller, dest as usize, &data).ok();
            dest
        },
    )?;

    // Emscripten async wget (stub)
    linker.func_new(
        "env",
        "_emscripten_async_wget2",
        FuncType::new(
            engine,
            vec![ValType::I32; 8],
            vec![ValType::I32],
        ),
        |_caller, _params, results| {
            results[0] = Val::I32(0);
            Ok(())
        },
    )?;

    linker.func_wrap(
        "env",
        "_emscripten_async_wget2_abort",
        |_: Caller<'_, State>, _handle: i32| {},
    )?;

    // Config queries
    linker.func_wrap(
        "env",
        "_assetsLocal",
        |_: Caller<'_, State>| -> i32 { 1 },
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

    // _getLocalPipelineHeaders: returns concatenated .hdr file contents
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
                return 0;
            }

            let malloc_fn = match caller
                .get_export("_malloc")
                .and_then(|e| e.into_func())
            {
                Some(f) => f,
                None => return 0,
            };
            let mut results = [Val::I32(0)];
            if malloc_fn
                .call(
                    &mut caller,
                    &[Val::I32(hdr_content.len() as i32 + 1)],
                    &mut results,
                )
                .is_err()
            {
                return 0;
            }
            let ptr = results[0].unwrap_i32();

            let mem = get_memory(&mut caller);
            mem.write(&mut caller, ptr as usize, hdr_content.as_bytes())
                .ok();
            mem.write(&mut caller, ptr as usize + hdr_content.len(), &[0u8])
                .ok();

            ptr
        },
    )?;

    // Syscalls

    // writev (146) -- capture stdout/stderr
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

    // syscall5 (open)
    linker.func_wrap(
        "env",
        "___syscall5",
        |mut caller: Caller<'_, State>, _which: i32, varargs: i32| -> i32 {
            let mem = get_memory(&mut caller);
            let path_ptr = read_i32(&mem, &caller, varargs as u32) as u32;
            let _flags = read_i32(&mem, &caller, varargs as u32 + 4);
            let path = read_cstring(&mem, &caller, path_ptr);

            let trimmed = path.trim_end_matches('/');
            if trimmed == "/cfdir" {
                let fd = caller.data().next_fd;
                caller.data_mut().next_fd += 1;
                // position=999999 so getdents returns 0 immediately
                caller.data_mut().files.insert(fd, VFile { data: Vec::new(), position: 999999 });
                return fd;
            }

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
                    fd
                }
                Err(e) => {
                    let name = file_path.display().to_string();
                    if name.contains("userdct") {
                        debug!(path = %name, "optional user dictionary not found");
                    } else {
                        warn!(path = %name, error = %e, "file open failed");
                    }
                    -1
                }
            }
        },
    )?;

    // syscall3 (read)
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
                mem.write(&mut caller, buf as usize, &data).ok();
                data.len() as i32
            } else {
                -1
            }
        },
    )?;

    // syscall6 (close)
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

    // syscall140 (lseek)
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

    // syscall145 (readv)
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
                    mem.write(&mut caller, base as usize, &data).ok();
                    total += data.len() as i32;
                }
            }
            total
        },
    )?;

    // syscall195 (stat64)
    linker.func_wrap(
        "env",
        "___syscall195",
        |mut caller: Caller<'_, State>, _which: i32, varargs: i32| -> i32 {
            let mem = get_memory(&mut caller);
            let path_ptr = read_i32(&mem, &caller, varargs as u32) as u32;
            let buf_ptr = read_i32(&mem, &caller, varargs as u32 + 4) as u32;
            let path = read_cstring(&mem, &caller, path_ptr);

            let trimmed = path.trim_end_matches('/');
            if trimmed == "/cfdir" || trimmed.is_empty() {
                let mem = get_memory(&mut caller);
                let zeros = [0u8; 96];
                mem.write(&mut caller, buf_ptr as usize, &zeros).ok();
                // st_mode = directory (S_IFDIR | 0755)
                write_i32(&mem, &mut caller, buf_ptr + 8, 0o40755);
                return 0;
            }

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
                    mem.write(&mut caller, buf_ptr as usize, &zeros).ok();
                    write_i32(&mem, &mut caller, buf_ptr + 40, meta.len() as i32);
                    write_i32(&mem, &mut caller, buf_ptr + 8, 0o100644);
                    0
                }
                Err(_) => -1,
            }
        },
    )?;

    // syscall220 (getdents64) -- list /cfdir directory
    linker.func_wrap(
        "env",
        "___syscall220",
        |mut caller: Caller<'_, State>, _which: i32, varargs: i32| -> i32 {
            let mem = get_memory(&mut caller);
            let fd = read_i32(&mem, &caller, varargs as u32);
            let dirp = read_i32(&mem, &caller, varargs as u32 + 4) as u32;
            let count = read_i32(&mem, &caller, varargs as u32 + 8) as usize;

            let start_idx = caller
                .data()
                .files
                .get(&fd)
                .map(|f| f.position)
                .unwrap_or(0);

            let voice_dir = caller.data().voice_dir.clone();
            let mut entries: Vec<String> = vec![".".to_string(), "..".to_string()];
            if let Ok(rd) = std::fs::read_dir(&voice_dir) {
                for e in rd.filter_map(|e| e.ok()).filter(|e| e.path().is_file()) {
                    entries.push(e.file_name().to_string_lossy().to_string());
                }
            }

            if start_idx >= entries.len() {
                return 0;
            }

            // Emscripten uses fixed 280-byte records for getdents64
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
                mem.write(&mut caller, base, &zeros).ok();
                mem.write(&mut caller, base, &(i as u64 + 1).to_le_bytes()).ok();
                mem.write(&mut caller, base + 8, &((i + 1) as u64).to_le_bytes())
                    .ok();
                mem.write(&mut caller, base + 16, &(REC_LEN as u16).to_le_bytes())
                    .ok();
                let d_type: u8 = if name == "." || name == ".." { 4 } else { 8 };
                mem.write(&mut caller, base + 18, &[d_type]).ok();
                let name_bytes = name.as_bytes();
                let name_len = name_bytes.len().min(255);
                mem.write(&mut caller, base + 19, &name_bytes[..name_len]).ok();

                offset += REC_LEN;
                entries_written += 1;
            }

            if let Some(file) = caller.data_mut().files.get_mut(&fd) {
                file.position = start_idx + entries_written;
            }

            offset as i32
        },
    )?;

    // Remaining syscalls -- stub
    for name in [
        "___syscall20",
        "___syscall54",
        "___syscall221",
    ] {
        linker.func_wrap(
            "env",
            name,
            move |_: Caller<'_, State>, _which: i32, _varargs: i32| -> i32 { 0 },
        )?;
    }

    // asm_const variants (each has a different number of i32 params)
    let asm_const_variants: &[(&str, usize)] = &[
        ("_emscripten_asm_const_i", 1),
        ("_emscripten_asm_const_ii", 2),
        ("_emscripten_asm_const_iiii", 4),
        ("_emscripten_asm_const_iiiii", 5),
        ("_emscripten_asm_const_iiiiii", 6),
        ("_emscripten_asm_const_iiiiiiiii", 9),
        ("_emscripten_asm_const_iiiiiiiiiiii", 12),
        ("_emscripten_asm_const_iiiiiiiiiiiiii", 14),
        ("_emscripten_asm_const_iiiiiiiiiiiiiiiiiiiiii", 22),
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
                let ret = asm_const_dispatch(&mut caller, idx, &args);
                results[0] = Val::I32(ret);
                Ok(())
            },
        )?;
    }

    Ok(())
}
