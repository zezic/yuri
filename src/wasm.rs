use anyhow::{Context, Result};
use wasmtime::*;

use crate::emscripten::{alloc_string, define_imports, write_i32, State};
use crate::SynthesisLimits;

pub(crate) fn instantiate_module(
    engine: &Engine,
    module: &Module,
    state: State,
) -> Result<(Store<State>, Instance)> {
    let mut store = Store::new(engine, state);
    let mut linker = Linker::new(engine);

    define_imports(&mut linker, engine)?;

    // Globals
    let table_base = Global::new(
        &mut store,
        GlobalType::new(ValType::I32, Mutability::Const),
        Val::I32(0),
    )?;
    linker.define(&mut store, "env", "__table_base", table_base)?;

    let dynamictop_ptr = Global::new(
        &mut store,
        GlobalType::new(ValType::I32, Mutability::Const),
        Val::I32(0x0db8c0),
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

    // Memory (4096 pages = 256 MB)
    let memory = Memory::new(&mut store, MemoryType::new(4096, None))?;
    linker.define(&mut store, "env", "memory", memory)?;
    store.data_mut().memory = Some(memory);

    // Table (84096 funcref entries)
    let table = Table::new(
        &mut store,
        TableType::new(RefType::FUNCREF, 84096, Some(84096)),
        Ref::Func(None),
    )?;
    linker.define(&mut store, "env", "table", table)?;

    // tempDoublePtr global
    let temp_double = Global::new(
        &mut store,
        GlobalType::new(ValType::I32, Mutability::Const),
        Val::I32(0x0db9b0),
    )?;
    linker.define(&mut store, "env", "tempDoublePtr", temp_double)?;

    let instance = linker
        .instantiate(&mut store, module)
        .context("WASM instantiation failed")?;

    // Initialize DYNAMICTOP_PTR in memory
    let mem = store.data().memory.unwrap();
    write_i32(&mem, &mut store, 0x0db8c0, 0x5db9c0);

    Ok((store, instance))
}

pub(crate) fn init_tts(
    store: &mut Store<State>,
    instance: &Instance,
    speed: i32,
    pitch: i32,
    volume: i32,
) -> Result<()> {
    // 1. Call _main()
    let main_fn = instance.get_typed_func::<(), i32>(&mut *store, "_main")?;
    main_fn.call(&mut *store, ())?;

    // 2. Build config JSON from voice directory listing
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

    // 3. Build params JSON matching the JS SDK format
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
    let params_ptr = alloc_string(store, instance, &params_str)?;

    // 4. Call _imp_ttsInitialize(ttsWorker=-1, paramsJsonPtr, requestId=1)
    let init_fn =
        instance.get_typed_func::<(i32, i32, i32), ()>(&mut *store, "_imp_ttsInitialize")?;
    init_fn.call(&mut *store, (-1, params_ptr as i32, 1))?;

    // 5. Set voice via individual parameter exports
    let set_str = instance.get_typed_func::<(i32, i32), i32>(&mut *store, "_ttsSetOneParamStr")?;
    let set_int = instance.get_typed_func::<(i32, i32), i32>(&mut *store, "_ttsSetOneParamInt")?;

    // Detect voice from pipeline header filename
    // (e.g. ve_pipeline_enu_zoe_22_embedded-compact_2-2-1.hdr)
    let voice_dir = store.data().voice_dir.clone();
    let hdr: Option<String> = std::fs::read_dir(&voice_dir).ok().and_then(|rd| {
        rd.filter_map(|e| e.ok())
            .find(|e| e.path().extension().map(|x| x == "hdr").unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().to_string())
    });

    let (lang, voice_name, vop) = if let Some(ref h) = hdr {
        let parts: Vec<&str> = h.trim_end_matches(".hdr").split('_').collect();
        // Format: ve_pipeline_{lang}_{voice}_{rate}_{vop}_{version}
        if parts.len() >= 6 {
            let lang = parts[2];
            let voice = parts[3];
            let voice_cap = format!("{}{}", &voice[..1].to_uppercase(), &voice[1..]);
            let vop_str = parts[5..parts.len()-1].join("-");
            let vop = if vop_str.contains("embedded") { vop_str } else { String::new() };
            (lang.to_string(), voice_cap, vop)
        } else {
            ("enu".to_string(), "Zoe".to_string(), "embedded-compact".to_string())
        }
    } else {
        ("enu".to_string(), "Zoe".to_string(), "embedded-compact".to_string())
    };

    let language = match lang.as_str() {
        "enu" => "American English",
        "rur" => "Russian",
        _ => "American English",
    };

    let lang_ptr = alloc_string(store, instance, language)?;
    set_str.call(&mut *store, (1, lang_ptr as i32))?;
    let name_ptr = alloc_string(store, instance, &voice_name)?;
    set_str.call(&mut *store, (2, name_ptr as i32))?;
    let vop_ptr = alloc_string(store, instance, &vop)?;
    set_str.call(&mut *store, (3, vop_ptr as i32))?;

    // 8=volume, 9=speed, 10=pitch
    set_int.call(&mut *store, (8, volume))?;
    set_int.call(&mut *store, (9, speed))?;
    set_int.call(&mut *store, (10, pitch))?;

    // Trigger voice loading via _imp_ttsSetSpeechParams
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
    set_speech.call(&mut *store, (-1, voice_json_ptr as i32, 2))?;

    // Re-apply speech params after voice selection (voice selection resets them)
    let params_json = serde_json::json!({
        "speed": speed,
        "pitch": pitch,
        "volume": volume,
    });
    let params_json_str = params_json.to_string();
    let params_json_ptr = alloc_string(store, instance, &params_json_str)?;
    set_speech.call(&mut *store, (-1, params_json_ptr as i32, 4))?;

    // Free all malloc'd strings to prevent WASM heap leaks
    let free_fn = instance.get_typed_func::<i32, ()>(&mut *store, "_free")?;
    free_fn.call(&mut *store, params_ptr as i32)?;
    free_fn.call(&mut *store, lang_ptr as i32)?;
    free_fn.call(&mut *store, name_ptr as i32)?;
    free_fn.call(&mut *store, vop_ptr as i32)?;
    free_fn.call(&mut *store, voice_json_ptr as i32)?;
    free_fn.call(&mut *store, params_json_ptr as i32)?;

    Ok(())
}

pub(crate) fn speak_text_streaming(
    store: &mut Store<State>,
    instance: &Instance,
    text: &str,
    limits: &SynthesisLimits,
    on_samples: &mut dyn FnMut(&[i16]),
) -> Result<()> {
    let imp_speak =
        instance.get_typed_func::<(i32, i32, i32), ()>(&mut *store, "_imp_ttsSpeak")?;
    let worker_speak =
        instance.get_typed_func::<(i32, i32), ()>(&mut *store, "_worker_ttsSpeak")?;

    store.data_mut().needs_more_audio = false;
    store.data_mut().pending_samples.clear();

    // Encode text as UTF-16 LE (the engine's native encoding)
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let utf16_bytes = utf16.len() * 2 + 2;

    let stack_save_fn = instance.get_typed_func::<(), i32>(&mut *store, "stackSave")?;
    let stack_alloc_fn = instance.get_typed_func::<i32, i32>(&mut *store, "stackAlloc")?;
    let stack_restore_fn = instance.get_typed_func::<i32, ()>(&mut *store, "stackRestore")?;

    let sp = stack_save_fn.call(&mut *store, ())?;
    let text_buf = stack_alloc_fn.call(&mut *store, utf16_bytes as i32)?;
    {
        let mem = store.data().memory.unwrap();
        let mut bytes = Vec::with_capacity(utf16_bytes);
        for &c in &utf16 {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        bytes.extend_from_slice(&[0u8, 0u8]); // UTF-16 null terminator
        mem.write(&mut *store, text_buf as usize, &bytes)?;
    }
    imp_speak.call(&mut *store, (-1, text_buf, 3))?;
    stack_restore_fn.call(&mut *store, sp)?;

    let max_samples = limits.max_duration_secs as usize * crate::SAMPLE_RATE as usize;
    let max_idle = limits.max_idle_iterations;

    // Drain samples produced by the initial _imp_ttsSpeak call
    let initial = std::mem::take(&mut store.data_mut().pending_samples);
    if !initial.is_empty() {
        on_samples(&initial);
    }

    // Continuation loop
    let mut no_progress: u32 = 0;
    let mut total_samples: usize = initial.len();
    loop {
        if !store.data().needs_more_audio { break; }
        store.data_mut().needs_more_audio = false;
        worker_speak.call(&mut *store, (0, 0))?;

        let chunk = std::mem::take(&mut store.data_mut().pending_samples);
        if !chunk.is_empty() {
            total_samples += chunk.len();
            on_samples(&chunk);
            no_progress = 0;
        } else {
            no_progress += 1;
        }

        if no_progress >= max_idle || total_samples >= max_samples {
            break;
        }
    }

    // Drain any remaining samples
    let remaining = std::mem::take(&mut store.data_mut().pending_samples);
    if !remaining.is_empty() {
        on_samples(&remaining);
    }

    Ok(())
}
