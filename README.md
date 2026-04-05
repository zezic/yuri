# Yuri — Local Vocalizer TTS via WASM

Run the Nuance Vocalizer Expressive TTS engine locally on any platform using a standalone WASM runtime. No browser required.

This is the same engine behind Apple's built-in "Yuri" voice on iOS/macOS, NVDA screen reader voices, and Code Factory's Vocalizer apps.

## Library usage

Add the crate and provide a voice file — everything else is embedded:

```rust
let engine = yuri::Engine::new()?;
let mut voice = yuri::Voice::from_addon(&engine, "yuri.nvda-addon", Default::default())?;

// Streaming: receive audio chunks as they're produced (~90ms each)
voice.speak("Привет мир", |event| {
    match event {
        yuri::SpeechEvent::Audio(chunk) => play(&chunk.samples),
        yuri::SpeechEvent::Done => {},
    }
    Ok(())
})?;

// Or collect all audio at once
let samples: Vec<i16> = voice.synthesize("Hello world")?;
```

The WASM engine and common voice data files are compressed and embedded in the library. Users only need an `.nvda-addon` voice file.

Audio format: 16-bit PCM, 22050 Hz, mono.

## CLI usage

```bash
# Play directly through speakers
cargo run --release -- --text "Hello world" --voice-dir wasm/voicedata_enu

# Russian Yuri (included in repo)
cargo run --release -- --text "Привет мир" --voice-dir wasm/voicedata_yuri_high

# Load voice from an .nvda-addon file
cargo run --release -- --text "Привет мир" --addon yuri.nvda-addon

# Save to file
cargo run --release -- --text "Hello world" --voice-dir wasm/voicedata_enu -o hello.wav

# Read from stdin
echo "Привет мир" | cargo run --release -- --voice-dir wasm/voicedata_yuri_high

# Interactive mode (type lines, press Enter to hear them)
cargo run --release -- --voice-dir wasm/voicedata_yuri_high
```

### Speech parameters

```bash
cargo run --release -- --text "Hello" --voice-dir wasm/voicedata_enu --speed 200
cargo run --release -- --text "Hello" --voice-dir wasm/voicedata_enu --pitch 60
```

| Parameter | Range | Default | Description |
|-----------|-------|---------|-------------|
| `--speed` | 50-400 | 100 | Speaking rate (%) |
| `--pitch` | 50-200 | 100 | Voice pitch (%) |
| `--volume` | 0-100 | 80 | Output volume |

## Voices

### Included in repo

**English — Zoe** (compact, 19 MB). Voice data from the Code Factory WebApps SDK demo CDN.

**Russian — Yuri EmbeddedHigh** (55 MB). From the [Vocalizer Expressive 2 NVDA addon](https://nvda-addons.ru/get.php?file=vocalizer_expressive2_voice_Russian_Yuri_EmbeddedHigh).

### Optional: Yuri PremiumHigh (160 MB)

Same voice Apple ships on iOS/macOS. Run `./setup.sh` to download and extract:

```bash
./setup.sh
cargo run --release -- --text "Привет мир" --voice-dir wasm/voicedata_yuri_full
```

### Other compatible voices

These Vocalizer Expressive 2 NVDA addons work with `--addon` or `./unpack-voice.sh`:

| Voice | Quality | Link |
|-------|---------|------|
| Yuri (Male) | EmbeddedPro | [Download](https://nvda-addons.ru/get.php?file=vocalizer_expressive2_voice_Russian_Yuri_EmbeddedPro) |
| Milena (Female) | EmbeddedHigh | [Download](https://nvda-addons.ru/get.php?file=vocalizer_expressive2_voice_Russian_Milena_EmbeddedHigh) |
| Milena (Female) | EmbeddedPro | [Download](https://nvda-addons.ru/get.php?file=vocalizer_expressive2_voice_Russian_Milena_EmbeddedPro) |
| Katya (Female) | EmbeddedPro | [Download](https://nvda-addons.ru/get.php?file=vocalizer_expressive2_voice_Russian_Katya_EmbeddedPro) |
| Katya ML (Female) | EmbeddedPro | [Download](https://nvda-addons.ru/get.php?file=vocalizer_expressive2_voice_Russian_Katya_ML_EmbeddedPro) |

To use with `--addon`:
```bash
cargo run --release -- --text "Привет" --addon downloaded-addon.nvda-addon
```

To extract for `--voice-dir`:
```bash
./unpack-voice.sh downloaded-addon.nvda-addon
cargo run --release -- --text "Привет" --voice-dir wasm/voicedata_rur-milena
```

## How it works

The Rust host loads `webtts.wasm` (a Nuance Vocalizer engine compiled to WebAssembly by Code Factory) via Wasmtime, and implements the Emscripten runtime it expects: 49 import functions and a 25-entry `asm_const` dispatch table bridging the engine's C code to host-side file I/O and audio capture.

Key implementation details:
- Text is encoded as UTF-16 LE (the engine's native `wchar_t` format)
- Synthesis uses `_imp_ttsSpeak` + `_worker_ttsSpeak(0, 0)` continuation loop
- Each continuation must run with a clean WASM stack (no recursion from `asm_const[3]`)
- Pipeline headers are returned as file content via `_getLocalPipelineHeaders`
- WASM module is precompiled and cached (~5s first run, ~2ms after)

## Performance

With cached WASM module (after first run):

| Voice | Synth time | Audio | Realtime factor |
|-------|-----------|-------|-----------------|
| English Zoe (compact) | ~10ms | 1.4s | 28x |
| Yuri EmbeddedHigh | ~10ms | 1.9s | 28x |
| Yuri PremiumHigh | ~140ms | 1.9s | 14x |

## Dependencies

Library (always linked):
- [wasmtime](https://docs.rs/wasmtime) — WebAssembly runtime
- [include-flate](https://docs.rs/include-flate) — embedded compressed assets
- [zip](https://docs.rs/zip) — .nvda-addon extraction
- [tempfile](https://docs.rs/tempfile) — temporary voice data directories

CLI only (behind `cli` feature, default on):
- [clap](https://docs.rs/clap) — argument parsing
- [hound](https://docs.rs/hound) — WAV file output
- [rodio](https://docs.rs/rodio) — audio playback

## License

The Rust host code is original work. The `webtts.wasm` binary and voice data files are proprietary assets from Nuance/Code Factory subject to their respective licenses.
