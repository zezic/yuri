# Yuri — Local Vocalizer TTS via WASM

Run the Nuance Vocalizer Expressive TTS engine locally on any platform using a standalone WASM runtime. No browser required.

This is the same engine behind Apple's built-in "Yuri" voice on iOS/macOS, NVDA screen reader voices, and Code Factory's Vocalizer apps.

## Quick start

1. Download a voice addon (pick one):

   | Voice | Download |
   |-------|----------|
   | **Yuri** — Russian, Male, EmbeddedHigh | [Download .nvda-addon](https://nvda-addons.ru/get.php?file=vocalizer_expressive2_voice_Russian_Yuri_EmbeddedHigh) |
   | **Milena** — Russian, Female, EmbeddedHigh | [Download .nvda-addon](https://nvda-addons.ru/get.php?file=vocalizer_expressive2_voice_Russian_Milena_EmbeddedHigh) |
   | **Katya** — Russian, Female, EmbeddedPro | [Download .nvda-addon](https://nvda-addons.ru/get.php?file=vocalizer_expressive2_voice_Russian_Katya_EmbeddedPro) |
   | **Katya ML** — Russian, Female, EmbeddedPro | [Download .nvda-addon](https://nvda-addons.ru/get.php?file=vocalizer_expressive2_voice_Russian_Katya_ML_EmbeddedPro) |
   | **Yuri** — Russian, Male, EmbeddedPro | [Download .nvda-addon](https://nvda-addons.ru/get.php?file=vocalizer_expressive2_voice_Russian_Yuri_EmbeddedPro) |
   | **Milena** — Russian, Female, EmbeddedPro | [Download .nvda-addon](https://nvda-addons.ru/get.php?file=vocalizer_expressive2_voice_Russian_Milena_EmbeddedPro) |

2. Run:
   ```bash
   cargo run --release -- --text "Привет мир" --addon downloaded.nvda-addon
   ```

First run takes ~5 seconds (WASM compilation). Subsequent runs take ~80ms (cached).

## Library usage

Add `yuri` to your `Cargo.toml`. The WASM engine and common voice data are embedded — users only need a `.nvda-addon` voice file.

### Play through speakers

```rust
let engine = yuri::Engine::new()?;
let mut voice = yuri::Voice::from_addon(&engine, "voice.nvda-addon".as_ref(), Default::default())?;
voice.speak_to_device("Привет мир")?;
```

### Streaming (process chunks as they arrive)

```rust
voice.speak("Hello world", |event| {
    match event {
        yuri::SpeechEvent::Audio(chunk) => {
            // chunk.samples: Vec<i16>, chunk.sample_rate: 22050
            send_to_network(&chunk.samples)?;
        }
        yuri::SpeechEvent::Done => {}
    }
    Ok(())
})?;
```

### Collect all audio

```rust
let samples: Vec<i16> = voice.synthesize("Hello world")?;
```

### Fast startup with caching

```rust
use std::path::Path;

let engine = yuri::Engine::with_cache(Path::new("/tmp/yuri_cache"))?;
let mut voice = yuri::Voice::from_addon_cached(
    &engine,
    "voice.nvda-addon".as_ref(),
    Path::new("/tmp/yuri_cache/voice"),
    Default::default(),
)?;
// First run: ~5s. After that: ~80ms.
```

### Speech parameters

```rust
voice.set_params(yuri::SpeechParams { speed: 200, pitch: 100, volume: 80 })?;
voice.speak_to_device("Fast speech")?;
```

| Parameter | Range | Default |
|-----------|-------|---------|
| `speed` | 50–400 | 100 |
| `pitch` | 50–200 | 100 |
| `volume` | 0–100 | 80 |

### Inline control sequences

Control pitch, speed, volume, and pauses mid-sentence:

```rust
use yuri::control;

let text = format!(
    "Normal voice {} now high pitched {} now low {} back to normal",
    control::pitch(180), control::pitch(50), control::reset()
);
voice.speak_to_device(&text)?;
```

Available sequences:

| Function | Effect |
|----------|--------|
| `control::pause(ms)` | Insert silence (1–65535 ms) |
| `control::rate(pct)` | Change speed mid-text (50–400%) |
| `control::pitch(pct)` | Change pitch mid-text (50–200%) |
| `control::volume(lvl)` | Change volume mid-text (0–100) |
| `control::reset()` | Reset all params to defaults |

In the CLI, use backslash sequences directly:

```bash
# Pause between words
yuri --text 'Hello \pause=800\ world' --addon voice.nvda-addon

# Pitch changes mid-phrase
yuri --text 'Normal \pitch=180\ high \pitch=50\ low \rst\ normal' --addon voice.nvda-addon

# Speed up mid-sentence
yuri --text 'Regular speed \rate=250\ now fast \rst\ regular again' --addon voice.nvda-addon
```

See [`examples/`](examples/) for complete runnable examples.

## CLI usage

```bash
# Play through speakers
cargo run --release -- --text "Hello world" --addon voice.nvda-addon

# Save to WAV
cargo run --release -- --text "Hello world" --addon voice.nvda-addon -o hello.wav

# Read from stdin
echo "Привет мир" | cargo run --release -- --addon voice.nvda-addon

# Interactive mode
cargo run --release -- --addon voice.nvda-addon

# Speech parameters
cargo run --release -- --text "Hello" --addon voice.nvda-addon --speed 200 --pitch 60

# Use a pre-extracted voice directory
cargo run --release -- --text "Hello" --voice-dir wasm/voicedata_enu
```

## How it works

The Rust host loads `webtts.wasm` (a Nuance Vocalizer engine compiled to WebAssembly by Code Factory) via Wasmtime, and implements the Emscripten runtime it expects: 49 import functions and a 25-entry `asm_const` dispatch table bridging the engine's C code to host-side file I/O and audio capture.

Text is encoded as UTF-16 LE (the engine's native `wchar_t` format). Audio output is 16-bit PCM, 22050 Hz, mono.

## Performance

With cached WASM module (after first run):

| Operation | Time |
|-----------|------|
| Engine load (cached) | ~2ms |
| Voice init + speak "Hello world" | ~10ms |
| Full pipeline (cached engine + addon) | ~80ms |

Synthesis runs at 14–28x realtime depending on voice quality.

## Cargo features

| Feature | Default | Description |
|---------|---------|-------------|
| `playback` | yes | `Voice::speak_to_device()` via rodio |
| `cli` | yes | Binary with clap, hound, dirs |

To use as a library without audio playback:
```toml
[dependencies]
yuri = { version = "0.2", default-features = false }
```

## License

The Rust host code is original work. The `webtts.wasm` binary and voice data files are proprietary assets from Nuance/Code Factory subject to their respective licenses.
