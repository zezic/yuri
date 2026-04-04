# Yuri - Local Vocalizer TTS via WASM

Run the Nuance Vocalizer Expressive TTS engine locally on any platform using a standalone WASM runtime. No browser required.

This is the same engine behind Apple's built-in "Yuri" voice on iOS/macOS, NVDA screen reader voices, and Code Factory's Vocalizer apps.

## How it works

```
Rust Host (Wasmtime) --> webtts.wasm (Emscripten/Vocalizer)
     |                        |
     | 49 Emscripten imports  | Nuance Vocalizer Embedded
     | asm_const dispatch     | compiled to WebAssembly
     | file I/O bridge        | by Code Factory
     |                        |
     v                        v
Voice Data Files         PCM Audio Output
(CLC format)             (16-bit, 22050 Hz)
```

The Rust host implements the Emscripten runtime that the WASM module expects: memory management, syscalls, math functions, and a 25-entry `asm_const` dispatch table that bridges the engine's C code to host-side file I/O, audio capture, and asset management.

## Quick Start

Both voices work out of the box:

```bash
# English (Zoe, compact quality)
cargo run --release -- --text "Hello world" --voice-dir wasm/voicedata_enu -o hello.wav

# Russian (Yuri, EmbeddedHigh quality)
cargo run --release -- --text "Привет мир" --voice-dir wasm/voicedata_yuri_high -o privet.wav
```

For PremiumHigh quality Yuri (144MB synthesis database), run `./setup.sh` first:

```bash
./setup.sh  # downloads and extracts the NVDA addon
cargo run --release -- --text "Привет мир" --voice-dir wasm/voicedata_yuri_full -o privet.wav
```

Text is passed to the engine as UTF-16 LE (the engine's native character format).

## Voices

### English - Zoe (compact, 19 MB)

Included in repo. Voice data from the Code Factory WebApps SDK demo CDN.

### Russian - Yuri EmbeddedHigh (55 MB)

Included in repo. Single-file embedded voice from the [Vocalizer Expressive 2 NVDA addon](https://nvda-addons.ru/get.php?file=vocalizer_expressive2_voice_Russian_Yuri_EmbeddedHigh).

### Russian - Yuri PremiumHigh (160 MB)

Same voice as Apple ships on iOS/macOS. Requires setup due to the 144MB synthesis database:

```bash
./setup.sh
```

This downloads the [NVDA Vocalizer Yuri PremiumHigh addon](https://nvda-addons.ru/download.php?file=vocalizer_expressive_voice_yuri_Premium_High) and extracts the voice data.

### WASM Engine

The `webtts.wasm` binary (5.2 MB) is included in the repo. Original source: Code Factory's Vocalizer for WebApps SDK demo.

## Architecture

The host implements these Emscripten interfaces:

- **49 import functions**: math (`sin`, `cos`, `exp`, `log`, `pow`), memory (`sbrk`, `resize_heap`, `memcpy_big`), syscalls (`open`, `read`, `close`, `lseek`, `writev`, `stat`, `getdents`), threading stubs, time functions, and the `_emscripten_asm_const_*` dispatch family
- **25 asm_const entries**: config parsing `[0]`, asset management `[11,19,20]`, file I/O `[14-18]`, audio output `[7]`, synthesis continuation `[3]`, speech params `[4]`, voice lookup `[21]`, completion callbacks `[5,8]`, and event forwarding `[6,9,10]`
- **Voice file serving**: local file access via `asm_const[14]` (open), `[16]` (read), `[17]` (seek), `[18]` (size), mapped through `asm_const[21]` path resolution

Key implementation details:
- Text must be encoded as **UTF-16 LE** (the engine's native `wchar_t` format)
- Initialization uses `_imp_ttsInitialize(-1, paramsJsonPtr, requestId)` with `data: "local"` mode
- Voice selection via `_imp_ttsSetSpeechParams(-1, voiceJsonPtr, requestId)`
- Synthesis via `_imp_ttsSpeak(-1, utf16TextPtr, requestId)` followed by `_worker_ttsSpeak(0, 0)` continuation loop
- The continuation loop must run with a **clean WASM stack** (no recursion from `asm_const[3]`)
- Pipeline headers are returned as file **content** (not filenames) via `_getLocalPipelineHeaders`
- WASM stack size must be at least 16 MB (`Config::max_wasm_stack`)

## Dependencies

- [wasmtime](https://docs.rs/wasmtime) - WebAssembly runtime
- [hound](https://docs.rs/hound) - WAV file output
- [clap](https://docs.rs/clap) - CLI argument parsing
- [serde_json](https://docs.rs/serde_json) - JSON handling

## Status

Working:
- English Zoe (compact quality) - full sentences
- Russian Yuri (EmbeddedHigh quality) - full sentences
- Russian Yuri (PremiumHigh quality via `./setup.sh`) - full sentences
- WAV file output (16-bit PCM, 22050 Hz, mono)

Not yet implemented:
- Real-time audio playback (via `rodio`/`cpal`)
- Voice parameter control (speed, pitch, volume)
- Multiple sequential speak calls

## License

The Rust host code is original work. The `webtts.wasm` binary and voice data files are proprietary assets from Nuance/Code Factory subject to their respective licenses.
