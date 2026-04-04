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

## Usage

```bash
# English (Zoe, compact quality)
cargo run --release -- --text "Hello world" --voice-dir wasm/voicedata_enu -o hello.wav

# Russian (Yuri, PremiumHigh quality - same as Apple's Yuri)
cargo run --release -- --text "Привет мир" --voice-dir wasm/voicedata_yuri_full -o privet.wav
```

Text is passed to the engine as UTF-16 LE (the engine's native character format).

## Voice Data Setup

### English (Zoe, compact)

The demo voice data is available from the Code Factory CDN:

```bash
mkdir -p wasm/voicedata_enu
cd wasm/voicedata_enu
for f in sysdct.dat clm.dat synth_med_fxd_bet3f22.dat lid.dat; do
  curl -LO "https://www.codefactoryglobal.com/downloads/webassembly/voicedata/common/$f"
done
curl -LO "https://www.codefactoryglobal.com/downloads/webassembly/voicedata/languages/enu/speech/ve/ve_pipeline_enu_zoe_22_embedded-compact_2-2-1.hdr"
curl -LO "https://www.codefactoryglobal.com/downloads/webassembly/voicedata/languages/enu/speech/components/enu_zoe_embedded-compact_2-2-1.dat"
```

### Russian (Yuri, PremiumHigh)

Extract from the NVDA Vocalizer addon (rename `.nvda-addon` to `.zip`):

```bash
mkdir -p wasm/voicedata_yuri_full
# From the extracted NVDA addon:
cp rur/speech/components/*.dat wasm/voicedata_yuri_full/
cp rur/speech/ve/*.hdr wasm/voicedata_yuri_full/
# Common files from the English demo CDN:
cp wasm/voicedata_enu/{sysdct,clm,lid,synth_med_fxd_bet3f22}.dat wasm/voicedata_yuri_full/
```

### WASM Engine

The `webtts.wasm` binary (5.2 MB) is from Code Factory's Vocalizer for WebApps SDK:

```bash
curl -L "https://codefactoryglobal.com/webassembly/demo/webtts.wasm" -o wasm/webtts.wasm
```

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
- Russian Yuri (PremiumHigh quality from NVDA addon) - full sentences
- WAV file output (16-bit PCM, 22050 Hz, mono)

Not yet implemented:
- Real-time audio playback (via `rodio`/`cpal`)
- Voice parameter control (speed, pitch, volume)
- Multiple sequential speak calls
- Compact Russian voice (encrypted format not supported by this WASM build)

## License

The Rust host code is original work. The `webtts.wasm` binary and voice data files are proprietary assets from Nuance/Code Factory subject to their respective licenses.
