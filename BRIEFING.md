# Project Briefing: Local Vocalizer TTS via WASM

## Goal

Run the Nuance Vocalizer Expressive TTS engine locally on any platform (Linux, macOS, Windows) **without a browser**, using a standalone WASM runtime. The target voice is **Yuri PremiumHigh** (Russian male), which is the same voice Apple ships as "Yuri" / "Yuri (Enhanced)" in AVSpeechSynthesizer on iOS/macOS.

## What We Have

### 1. Voice Data File
- **File:** `assets/vocalizer-voice-yuri-PremiumHigh.nvda-addon`
- **Format:** This is just a **renamed ZIP archive**. Rename to `.zip` and extract.
- **Contents:** Inside you'll find a folder structure with the voice data files (language model, audio data, etc.) for the Russian `ru-RU` locale. The voice data is **platform-independent** — the same data works with Windows DLLs, Linux .so, Android .so, or WASM engine.
- **Key folder to look for:** A folder named with a language code like `ru` or `ru-RU` containing `.dat`, `.lic`, or similar Vocalizer data files.

### 2. Rust Starter Template
- Located in the working directory
- We'll use Rust because it has excellent WASM runtime support via **Wasmtime** or **Wasmer** crates

## Background: The Vocalizer Engine

### What is Vocalizer?
Nuance Vocalizer Expressive is a commercial TTS engine originally by Nuance Communications (now owned by Microsoft). It's the same engine behind:
- Apple's built-in TTS voices (Yuri, Samantha, Daniel, etc.)
- NVDA screen reader Vocalizer add-ons
- Code Factory's Android Vocalizer TTS app
- Various automotive and enterprise deployments

### Engine Variants
The engine exists as native binaries for each platform:
- **Windows:** `.dll` files (found inside NVDA add-ons as "VE Core")
- **Linux:** `.so` files (enterprise SDK only)
- **Android:** `.so` files (ARM, inside APKs)
- **iOS/macOS:** Bundled into the OS by Apple
- **WebAssembly:** `webtts.wasm` — compiled by Code Factory using Emscripten

### The WASM Version
Code Factory (codefactoryglobal.com) created **Vocalizer for WebApps**, which compiles the Vocalizer Embedded C engine to WebAssembly using Emscripten. Their SDK includes:
- `webtts.wasm` — the compiled TTS engine
- `webtts.js` — Emscripten-generated JS glue code
- A JavaScript API for interacting with the engine
- Voice data files served from a web server

Documentation: https://codefactoryglobal.com/webassembly/docs/

Their SDK is proprietary/commercial, but the architecture tells us:
1. The WASM module is a standard Emscripten-compiled C program
2. It expects to access voice data files (can be via filesystem or fetch)
3. It produces PCM audio buffers as output
4. The C API underneath uses functions like `ve_ttsOpen()`, `ve_ttsProcessText2()`, etc.

## Architecture Plan

```
┌─────────────────────────────────────────────┐
│              Rust Host Application           │
│                                              │
│  1. Load webtts.wasm via Wasmtime/Wasmer     │
│  2. Provide WASI filesystem access to        │
│     voice data directory                     │
│  3. Call TTS init / process / close exports   │
│  4. Receive PCM audio buffers                │
│  5. Play audio via cpal/rodio or save to WAV │
└──────────────┬──────────────────────────────┘
               │
               ▼
┌─────────────────────────────────────────────┐
│           webtts.wasm (Emscripten)           │
│                                              │
│  - Nuance Vocalizer Embedded engine          │
│  - Compiled from C to WASM                   │
│  - Expects: voice data files on filesystem   │
│  - Produces: PCM audio samples               │
│  - May need Emscripten runtime imports       │
└──────────────┬──────────────────────────────┘
               │
               ▼
┌─────────────────────────────────────────────┐
│         Voice Data (extracted from           │
│     vocalizer-voice-yuri-PremiumHigh)        │
│                                              │
│  - Language rules, phoneme data              │
│  - Voice model / audio segments              │
│  - Platform-independent format               │
└─────────────────────────────────────────────┘
```

## Implementation Steps

### Phase 1: Investigate
1. **Extract the .nvda-addon** — rename to .zip, unpack, document the file structure
2. **Obtain webtts.wasm** — this is the hard part. Options:
   - Check if Code Factory has a public demo at codefactoryglobal.com that loads the WASM (inspect network tab)
   - Look for any publicly accessible CDN or demo deployment
   - Check npm packages or GitHub for any related code
   - The JS glue file (`webtts.js`) would reveal the expected WASM imports/exports
3. **Analyze the WASM module** — use `wasm-tools`, `wasm2wat`, or `wasmtime explore` to understand:
   - What functions are exported?
   - What imports does it expect (memory, filesystem, env functions)?
   - Is it WASI-compatible or does it need Emscripten-specific imports?

### Phase 2: Build the Host
1. **Set up Rust project** with `wasmtime` (or `wasmer`) as the WASM runtime
2. **Implement host functions** that the WASM module imports:
   - Filesystem access (to read voice data)
   - Memory allocation
   - Any Emscripten-specific imports (`emscripten_memcpy_js`, `__syscall_*`, etc.)
3. **Create a CLI interface:** `vocalizer-local --voice ./yuri-data/ --text "Привет мир" --output hello.wav`

### Phase 3: Audio Output
1. Capture PCM buffers from the WASM engine
2. Either save as WAV file or play directly using `rodio`/`cpal`

## Key Technical Challenges

### 1. Obtaining the WASM binary
The `webtts.wasm` is commercial software. It may be loadable from a public demo page. The JS glue code is equally important for understanding the expected interface.

### 2. Emscripten vs WASI
If the WASM was compiled with Emscripten (likely), it won't use standard WASI interfaces. Instead it will import Emscripten-specific functions like:
- `emscripten_resize_heap`
- `fd_write`, `fd_read` (WASI-like but possibly Emscripten's version)
- `__syscall_openat`, `__syscall_stat64`, etc.
- Memory management functions

You'll need to implement these host-side. Wasmtime has some Emscripten compatibility, or you may need to shim them.

### 3. Voice Data Path
The engine needs to find voice data files. You'll need to either:
- Mount the extracted voice data directory via WASI preopened dirs
- Intercept filesystem calls and serve files from the host

### 4. Licensing
The WASM engine checks for licensing. You may encounter license validation that needs to be handled or bypassed. Code Factory's web SDK likely uses some form of API key or domain-based licensing.

## Fallback Approaches

If the WASM approach proves too difficult, consider:

### Alternative A: Windows DLL via Wine on Linux
Extract VE Core DLLs from an NVDA Vocalizer add-on, use Wine or `pe-loader` to call them on Linux.

### Alternative B: Android .so via extraction
Extract the ARM `.so` from Code Factory's Android APK, run on ARM Linux (Raspberry Pi) or via QEMU user-mode emulation on x86.

### Alternative C: Python + ctypes with Windows DLL
On Windows, extract the DLLs and call them directly via Python ctypes — the simplest path but Windows-only.

## Useful Links

- Code Factory WebApps SDK docs: https://codefactoryglobal.com/webassembly/docs/
- Nuance Vocalizer Enterprise install guide (has API info): https://www.auser.lombardia.it/upload/installing_vocalizer_7.6.4(9).pdf
- WorldVoice NVDA add-on (open source driver, shows how VE Core is called): https://github.com/tsengwoody/WorldVoice
- NVDA source (screen reader framework): https://github.com/nvaccess/nvda
- Wasmtime Rust crate: https://docs.rs/wasmtime
- Wasmer Rust crate: https://docs.rs/wasmer

## Voice Data Format Notes

When you extract the .nvda-addon, you'll likely see:
```
ru/                          # Language code folder
  ├── *.dat                  # Voice model data
  ├── *.ztl                  # Language rules
  └── ...                    # Various data files
```

The Vocalizer engine expects a root data directory containing language folders. The engine is told where this root is during initialization.

## Summary

**The core idea:** The same Nuance Vocalizer engine that runs on iOS, Android, and Windows has been compiled to WebAssembly by Code Factory. WASM can run outside browsers via Wasmtime/Wasmer. If we can get the WASM binary and understand its interface (from the JS glue code), we can build a Rust host that loads it, feeds it voice data from the extracted NVDA add-on, and produces audio — all locally, on any platform.

This would be the first truly cross-platform, offline Vocalizer runner outside of Apple/Windows/Android ecosystems.
