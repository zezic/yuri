//! Use cached engine and addon extraction for fast startup.
//!
//! First run: ~5s (compiles WASM + extracts addon)
//! Subsequent runs: ~80ms
//!
//! Usage: cargo run --example cached

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let addon = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "assets/Vocalizer-Expressive2-voice-Russian-Yuri-EmbeddedHigh-V.2.0.1.nvda-addon".into());

    let cache_dir = Path::new("/tmp/yuri_example_cache");
    let voice_cache = cache_dir.join("voice");

    let t0 = std::time::Instant::now();
    let engine = yuri::Engine::with_cache(cache_dir)?;
    let mut voice = yuri::Voice::from_addon_cached(&engine, addon.as_ref(), &voice_cache, Default::default())?;
    eprintln!("Engine ready in {:.0}ms", t0.elapsed().as_secs_f64() * 1000.0);

    voice.speak_to_device("Ready to speak")?;

    Ok(())
}
