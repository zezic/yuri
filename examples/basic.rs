//! Basic TTS synthesis — collect all audio and save to WAV.
//!
//! Usage: cargo run --example basic

fn main() -> anyhow::Result<()> {
    let addon = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "assets/Vocalizer-Expressive2-voice-Russian-Yuri-EmbeddedHigh-V.2.0.1.nvda-addon".into());

    let engine = yuri::Engine::new()?;
    let mut voice = yuri::Voice::from_addon(&engine, addon.as_ref(), Default::default())?;

    let samples = voice.synthesize("Hello world")?;
    println!("{} samples ({:.1}s)", samples.len(), samples.len() as f64 / yuri::SAMPLE_RATE as f64);

    Ok(())
}
