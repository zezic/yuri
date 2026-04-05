//! Demonstrate speech parameter control (speed, pitch, volume).
//!
//! Usage: cargo run --example params

fn main() -> anyhow::Result<()> {
    let addon = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "assets/Vocalizer-Expressive2-voice-Russian-Yuri-EmbeddedHigh-V.2.0.1.nvda-addon".into());

    let engine = yuri::Engine::new()?;
    let mut voice = yuri::Voice::from_addon(&engine, addon.as_ref(), Default::default())?;

    println!("Normal speed:");
    voice.speak_to_device("This is normal speed")?;

    voice.set_params(yuri::SpeechParams { speed: 200, pitch: 100, volume: 80 })?;
    println!("Fast:");
    voice.speak_to_device("This is fast speech")?;

    voice.set_params(yuri::SpeechParams { speed: 70, pitch: 150, volume: 80 })?;
    println!("Slow and high pitched:");
    voice.speak_to_device("This is slow and high")?;

    Ok(())
}
