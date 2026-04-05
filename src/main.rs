use anyhow::{bail, Result};
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "yuri", about = "Local Vocalizer TTS via WASM")]
struct Cli {
    /// Text to synthesize (reads from stdin if omitted)
    #[arg(short, long)]
    text: Option<String>,

    /// Output WAV file (plays audio directly if omitted)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Path to voice data directory
    #[arg(long)]
    voice_dir: Option<PathBuf>,

    /// Path to .nvda-addon file
    #[arg(long)]
    addon: Option<PathBuf>,

    /// Path to webtts.wasm (uses embedded WASM if omitted)
    #[arg(long)]
    wasm: Option<PathBuf>,

    /// Cache directory for compiled WASM and extracted voice data.
    /// Defaults to ~/.cache/yuri. Use --no-cache to disable.
    #[arg(long)]
    cache_dir: Option<PathBuf>,

    /// Disable caching (recompile WASM and re-extract addon every run)
    #[arg(long, default_value = "false")]
    no_cache: bool,

    /// Speaking speed (50-400, default 100)
    #[arg(long, default_value = "100")]
    speed: i32,

    /// Pitch (50-200, default 100)
    #[arg(long, default_value = "100")]
    pitch: i32,

    /// Volume (0-100, default 80)
    #[arg(long, default_value = "80")]
    volume: i32,
}

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
        "Wrote {} samples ({:.1}s) to {}",
        samples.len(),
        duration,
        path.display()
    );
    Ok(())
}

/// Process backslash-escaped control sequences in text for CLI convenience.
/// Converts `\pause=500\` to `\x1B\pause=500\` etc.
fn process_escapes(text: &str) -> String {
    let sequences = [
        "\\pause=", "\\rate=", "\\pitch=", "\\vol=", "\\voice=",
        "\\lang=", "\\readmode=", "\\mrk=", "\\rst\\",
    ];
    let mut result = text.to_string();
    for seq in &sequences {
        result = result.replace(seq, &format!("\x1B{seq}"));
    }
    result
}

fn speak_and_output(voice: &mut yuri::Voice, text: &str, cli: &Cli) -> Result<()> {
    let t0 = std::time::Instant::now();

    if let Some(ref path) = cli.output {
        let samples = voice.synthesize(text)?;
        let audio_dur = samples.len() as f64 / yuri::SAMPLE_RATE as f64;
        write_wav(&samples, yuri::SAMPLE_RATE, path)?;
        eprintln!("{:.0}ms synth, {:.1}s audio", t0.elapsed().as_secs_f64() * 1000.0, audio_dur);
    } else {
        voice.speak_to_device(text)?;
        eprintln!("{:.0}ms", t0.elapsed().as_secs_f64() * 1000.0);
    }

    Ok(())
}

fn default_cache_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|d| d.join("yuri"))
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let cache_dir = if cli.no_cache {
        None
    } else if let Some(ref dir) = cli.cache_dir {
        Some(dir.clone())
    } else {
        default_cache_dir()
    };

    let engine = if let Some(ref wasm_path) = cli.wasm {
        yuri::Engine::from_file(wasm_path)?
    } else if let Some(ref cache) = cache_dir {
        yuri::Engine::with_cache(cache)?
    } else {
        yuri::Engine::new()?
    };

    let params = yuri::SpeechParams {
        speed: cli.speed,
        pitch: cli.pitch,
        volume: cli.volume,
    };

    let mut voice = if let Some(ref addon_path) = cli.addon {
        if let Some(ref cache) = cache_dir {
            let addon_stem = addon_path.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "default".into());
            let voice_cache = cache.join("voices").join(addon_stem);
            yuri::Voice::from_addon_cached(&engine, addon_path, &voice_cache, params)?
        } else {
            yuri::Voice::from_addon(&engine, addon_path, params)?
        }
    } else if let Some(ref voice_dir) = cli.voice_dir {
        yuri::Voice::from_dir(&engine, voice_dir, params)?
    } else {
        bail!("Provide --voice-dir or --addon to select a voice");
    };

    use std::io::IsTerminal;

    if let Some(ref text) = cli.text {
        let text = process_escapes(text);
        speak_and_output(&mut voice, &text, &cli)?;
    } else if !std::io::stdin().is_terminal() {
        use std::io::Read;
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        let text = String::from_utf8_lossy(&buf).into_owned();
        let text = process_escapes(text.trim());
        if text.is_empty() {
            bail!("No text provided");
        }
        speak_and_output(&mut voice, &text, &cli)?;
    } else {
        use std::io::Read;
        eprintln!("Interactive mode -- type text and press Enter (Ctrl+D to quit)");
        let stdin = std::io::stdin();
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            buf.clear();
            loop {
                match stdin.lock().read(&mut byte) {
                    Ok(0) => {
                        if buf.is_empty() { return Ok(()); }
                        break;
                    }
                    Ok(_) => {
                        if byte[0] == b'\n' { break; }
                        buf.push(byte[0]);
                    }
                    Err(_) => break,
                }
            }
            let text = String::from_utf8_lossy(&buf).into_owned();
            let text = process_escapes(text.trim());
            if text.is_empty() {
                continue;
            }
            speak_and_output(&mut voice, &text, &cli)?;
        }
    }

    Ok(())
}
