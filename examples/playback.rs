//! Play speech through the default audio device.
//!
//! Usage: cargo run --example playback

fn main() -> anyhow::Result<()> {
    let addon = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "assets/Vocalizer-Expressive2-voice-Russian-Yuri-EmbeddedHigh-V.2.0.1.nvda-addon".into());

    let engine = yuri::Engine::new()?;
    let params = yuri::SpeechParams {
        speed: 120,
        pitch: 100,
        volume: 80,
    };
    let mut voice = yuri::Voice::from_addon(&engine, addon.as_ref(), params)?;

    voice.speak_to_device("Привет мир")?;
    voice.speak_to_device("Hello world")?;

    Ok(())
}
