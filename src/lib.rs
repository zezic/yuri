//! Yuri -- offline text-to-speech via a Vocalizer WASM engine.
//!
//! # Thread safety
//!
//! [`Engine`] is `Send + Sync` and can be shared across threads (e.g. via
//! `Arc<Engine>`). Each thread should create its own [`Voice`] from the
//! shared engine. [`Voice`] is **not** `Send` or `Sync` because it owns
//! mutable WASM instance state.

mod emscripten;
mod wasm;

use anyhow::{Context, Result};
use include_flate::flate;
use std::path::Path;
use wasmtime::{Config, Module, Store};

use emscripten::State;

flate!(static WEBTTS_WASM: [u8] from "wasm/webtts.wasm");
flate!(static SYSDCT_DAT: [u8] from "wasm/common/sysdct.dat");
flate!(static CLM_DAT: [u8] from "wasm/common/clm.dat");
flate!(static LID_DAT: [u8] from "wasm/common/lid.dat");
flate!(static SYNTH_MED_DAT: [u8] from "wasm/common/synth_med_fxd_bet3f22.dat");

/// Output sample rate of the TTS engine in Hz.
pub const SAMPLE_RATE: u32 = 22050;

const WASM_STACK_SIZE: usize = 16 * 1024 * 1024;

fn make_wasmtime_engine() -> Result<wasmtime::Engine> {
    let mut config = Config::new();
    config.max_wasm_stack(WASM_STACK_SIZE);
    Ok(wasmtime::Engine::new(&config)?)
}

/// Compiled WASM module, shareable across multiple voices.
///
/// `Engine` is `Send + Sync` -- it can be shared across threads via `Arc`.
///
/// Create once with [`Engine::new`] (embedded WASM) or [`Engine::from_file`]
/// (external `.wasm`), then pass to [`Voice::from_dir`] or [`Voice::from_addon`].
pub struct Engine {
    engine: wasmtime::Engine,
    module: Module,
}

/// Active TTS voice backed by its own WASM instance.
///
/// Each `Voice` owns isolated WASM state and can synthesize speech independently.
/// `Voice` is **not** `Send` or `Sync` -- each thread needs its own instance
/// created from a shared [`Engine`].
///
/// Create via [`Voice::from_dir`] or [`Voice::from_addon`].
pub struct Voice {
    store: Store<State>,
    instance: wasmtime::Instance,
    set_speech: wasmtime::TypedFunc<(i32, i32, i32), ()>,
    limits: SynthesisLimits,
    _tempdir: Option<tempfile::TempDir>,
}

/// Parameters controlling speech synthesis.
///
/// All values are percentages of the default rate. Out-of-range values
/// are clamped by the engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechParams {
    /// Speaking speed (50-400, default 100).
    pub speed: i32,
    /// Voice pitch (50-200, default 100).
    pub pitch: i32,
    /// Output volume (0-100, default 80).
    pub volume: i32,
}

impl Default for SpeechParams {
    fn default() -> Self {
        Self {
            speed: 100,
            pitch: 100,
            volume: 80,
        }
    }
}

/// A chunk of PCM audio samples delivered during streaming synthesis.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// Signed 16-bit PCM samples, mono.
    pub samples: Vec<i16>,
    /// Sample rate in Hz (always [`SAMPLE_RATE`]).
    pub sample_rate: u32,
}

/// Events delivered by [`Voice::speak`] during streaming synthesis.
#[derive(Debug, Clone)]
pub enum SpeechEvent {
    /// A chunk of audio samples (typically ~90ms of audio).
    Audio(AudioChunk),
    /// Synthesis of the utterance is complete.
    Done,
}

/// Limits for a single synthesis call, preventing runaway processing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynthesisLimits {
    /// Maximum audio duration in seconds (default 300 = 5 minutes).
    pub max_duration_secs: u32,
    /// Number of consecutive zero-output iterations before stopping (default 200).
    pub max_idle_iterations: u32,
}

impl Default for SynthesisLimits {
    fn default() -> Self {
        Self {
            max_duration_secs: 300,
            max_idle_iterations: 200,
        }
    }
}

impl Engine {
    /// Create an engine using the embedded WASM module.
    ///
    /// Compiles the WASM from scratch (~5s). Use [`Engine::with_cache`] for fast
    /// subsequent loads.
    pub fn new() -> Result<Self> {
        let engine = make_wasmtime_engine()?;

        let module = Module::new(&engine, &*WEBTTS_WASM)
            .context("Failed to compile embedded WASM module")?;

        Ok(Self { engine, module })
    }

    /// Create an engine with a cache directory for precompiled WASM.
    ///
    /// First call compiles and saves (~5s). Subsequent calls load from cache (~2ms).
    /// The cache directory is created if it doesn't exist.
    pub fn with_cache(cache_dir: &Path) -> Result<Self> {
        let engine = make_wasmtime_engine()?;

        std::fs::create_dir_all(cache_dir)
            .with_context(|| format!("Failed to create cache dir: {}", cache_dir.display()))?;

        let cache_path = cache_dir.join("webtts.cwasm");
        let module = if cache_path.exists() {
            match unsafe { Module::deserialize_file(&engine, &cache_path) } {
                Ok(m) => m,
                Err(_) => {
                    let _ = std::fs::remove_file(&cache_path);
                    let m = Module::new(&engine, &*WEBTTS_WASM)
                        .context("Failed to compile embedded WASM module")?;
                    if let Ok(bytes) = m.serialize() {
                        let _ = std::fs::write(&cache_path, bytes);
                    }
                    m
                }
            }
        } else {
            let m = Module::new(&engine, &*WEBTTS_WASM)
                .context("Failed to compile embedded WASM module")?;
            if let Ok(bytes) = m.serialize() {
                let _ = std::fs::write(&cache_path, bytes);
            }
            m
        };

        Ok(Self { engine, module })
    }

    /// Create an engine from a WASM file on disk.
    ///
    /// Uses a `.cwasm` cache beside the source file for faster subsequent loads.
    pub fn from_file(path: &Path) -> Result<Self> {
        let engine = make_wasmtime_engine()?;

        let cache_path = path.with_extension("cwasm");
        let module = if cache_path.exists() {
            match unsafe { Module::deserialize_file(&engine, &cache_path) } {
                Ok(m) => m,
                Err(_) => {
                    let _ = std::fs::remove_file(&cache_path);
                    let m = Module::from_file(&engine, path)
                        .context("Failed to load WASM file")?;
                    if let Ok(bytes) = m.serialize() {
                        let _ = std::fs::write(&cache_path, bytes);
                    }
                    m
                }
            }
        } else {
            let m = Module::from_file(&engine, path).context("Failed to load WASM file")?;
            if let Ok(bytes) = m.serialize() {
                let _ = std::fs::write(&cache_path, bytes);
            }
            m
        };

        Ok(Self { engine, module })
    }
}

impl Voice {
    /// Create a voice from a directory containing voice data files.
    pub fn from_dir(engine: &Engine, voice_dir: &Path, params: SpeechParams) -> Result<Self> {
        let state = State::new(voice_dir.to_path_buf());
        let (mut store, instance) =
            wasm::instantiate_module(&engine.engine, &engine.module, state)?;
        wasm::init_tts(&mut store, &instance, params.speed, params.pitch, params.volume)?;

        let set_speech = instance.get_typed_func::<(i32, i32, i32), ()>(&mut store, "_imp_ttsSetSpeechParams")?;

        Ok(Self {
            store,
            instance,
            set_speech,
            limits: SynthesisLimits::default(),
            _tempdir: None,
        })
    }

    /// Create a voice from an `.nvda-addon` file (ZIP archive).
    ///
    /// Extracts voice data into a temporary directory that is cleaned up when
    /// the `Voice` is dropped. Use [`Voice::from_addon_cached`] to persist the
    /// extracted files for faster subsequent loads.
    pub fn from_addon(engine: &Engine, addon_path: &Path, params: SpeechParams) -> Result<Self> {
        let tempdir = tempfile::tempdir().context("Failed to create temp directory")?;
        let voice_dir = tempdir.path().to_path_buf();
        Self::extract_addon(addon_path, &voice_dir)?;
        let state = State::new(voice_dir);
        let (mut store, instance) =
            wasm::instantiate_module(&engine.engine, &engine.module, state)?;
        wasm::init_tts(&mut store, &instance, params.speed, params.pitch, params.volume)?;
        let set_speech = instance.get_typed_func::<(i32, i32, i32), ()>(&mut store, "_imp_ttsSetSpeechParams")?;
        Ok(Self { store, instance, set_speech, limits: SynthesisLimits::default(), _tempdir: Some(tempdir) })
    }

    /// Create a voice from an `.nvda-addon` file with persistent cache.
    ///
    /// Extracts voice data into `cache_dir` on first call. Subsequent calls
    /// skip extraction if the files already exist. The cache directory is
    /// created if it doesn't exist.
    pub fn from_addon_cached(
        engine: &Engine,
        addon_path: &Path,
        cache_dir: &Path,
        params: SpeechParams,
    ) -> Result<Self> {
        std::fs::create_dir_all(cache_dir)
            .with_context(|| format!("Failed to create cache dir: {}", cache_dir.display()))?;

        // Check if already extracted (look for any .hdr file)
        let has_hdr = std::fs::read_dir(cache_dir)
            .map(|rd| rd.filter_map(|e| e.ok()).any(|e| {
                e.path().extension().map(|x| x == "hdr").unwrap_or(false)
            }))
            .unwrap_or(false);

        if !has_hdr {
            Self::extract_addon(addon_path, cache_dir)?;
        }

        Self::from_dir(engine, cache_dir, params)
    }

    fn extract_addon(addon_path: &Path, dest: &Path) -> Result<()> {
        let file = std::fs::File::open(addon_path)
            .with_context(|| format!("Failed to open addon: {}", addon_path.display()))?;
        let mut archive = zip::ZipArchive::new(file)
            .context("Failed to read addon as ZIP")?;

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            let entry_name = entry.name().to_string();

            if entry_name.ends_with(".dat") || entry_name.ends_with(".hdr") {
                let filename = entry_name.rsplit('/').next().unwrap_or(&entry_name);
                match filename {
                    "sysdct.dat" | "clm.dat" | "lid.dat" | "synth_med_fxd_bet3f22.dat" => continue,
                    _ => {}
                }
                let out_path = dest.join(filename);
                let mut out_file = std::fs::File::create(&out_path)
                    .with_context(|| format!("Failed to create {}", out_path.display()))?;
                std::io::copy(&mut entry, &mut out_file)?;
            }
        }

        std::fs::write(dest.join("sysdct.dat"), &*SYSDCT_DAT)?;
        std::fs::write(dest.join("clm.dat"), &*CLM_DAT)?;
        std::fs::write(dest.join("lid.dat"), &*LID_DAT)?;
        std::fs::write(dest.join("synth_med_fxd_bet3f22.dat"), &*SYNTH_MED_DAT)?;
        Ok(())
    }

    /// Synthesize speech, delivering audio chunks via a callback as they are produced.
    ///
    /// The callback receives [`SpeechEvent::Audio`] for each chunk and
    /// [`SpeechEvent::Done`] when synthesis is complete.
    pub fn speak(
        &mut self,
        text: &str,
        mut callback: impl FnMut(SpeechEvent) -> Result<()>,
    ) -> Result<()> {
        let mut cb_err: Option<anyhow::Error> = None;
        wasm::speak_text_streaming(
            &mut self.store,
            &self.instance,
            text,
            &self.limits,
            &mut |samples: &[i16]| {
                if cb_err.is_some() {
                    return;
                }
                if let Err(e) = callback(SpeechEvent::Audio(AudioChunk {
                    samples: samples.to_vec(),
                    sample_rate: SAMPLE_RATE,
                })) {
                    cb_err = Some(e);
                }
            },
        )?;
        if let Some(e) = cb_err {
            return Err(e);
        }
        callback(SpeechEvent::Done)?;
        Ok(())
    }

    /// Synthesize text and return all audio samples at once.
    pub fn synthesize(&mut self, text: &str) -> Result<Vec<i16>> {
        let mut all_samples = Vec::new();
        wasm::speak_text_streaming(
            &mut self.store,
            &self.instance,
            text,
            &self.limits,
            &mut |samples: &[i16]| {
                all_samples.extend_from_slice(samples);
            },
        )?;
        Ok(all_samples)
    }

    /// Update speech parameters on an already-initialized voice.
    pub fn set_params(&mut self, params: SpeechParams) -> Result<()> {
        let params_json = serde_json::json!({
            "speed": params.speed,
            "pitch": params.pitch,
            "volume": params.volume,
        });
        let params_json_str = params_json.to_string();
        let params_json_ptr =
            emscripten::alloc_string(&mut self.store, &self.instance, &params_json_str)?;
        self.set_speech
            .call(&mut self.store, (-1, params_json_ptr as i32, 4))?;

        // Free the allocated string to prevent WASM heap leak
        let free_fn = self.instance.get_typed_func::<i32, ()>(&mut self.store, "_free")?;
        free_fn.call(&mut self.store, params_json_ptr as i32)?;
        Ok(())
    }

    /// Synthesize speech and play it through the default audio output device.
    ///
    /// Audio is streamed to the device as chunks are produced, so playback
    /// begins almost immediately. Blocks until playback is complete.
    ///
    /// Requires the `playback` feature (enabled by default).
    #[cfg(feature = "playback")]
    pub fn speak_to_device(&mut self, text: &str) -> Result<()> {
        use rodio::{buffer::SamplesBuffer, OutputStream, Sink};

        let (_stream, handle) = OutputStream::try_default()
            .context("Failed to open audio output device")?;
        let sink = Sink::try_new(&handle)?;

        self.speak(text, |event| {
            if let SpeechEvent::Audio(chunk) = event {
                let source = SamplesBuffer::new(1, chunk.sample_rate, chunk.samples);
                sink.append(source);
            }
            Ok(())
        })?;

        sink.sleep_until_end();
        Ok(())
    }

    /// Override synthesis limits (max duration, idle iterations).
    pub fn set_limits(&mut self, limits: SynthesisLimits) {
        self.limits = limits;
    }
}
