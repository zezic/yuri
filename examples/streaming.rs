//! Streaming synthesis — process audio chunks as they arrive.
//!
//! Usage: cargo run --example streaming

fn main() -> anyhow::Result<()> {
    let addon = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "assets/Vocalizer-Expressive2-voice-Russian-Yuri-EmbeddedHigh-V.2.0.1.nvda-addon".into());

    let engine = yuri::Engine::new()?;
    let mut voice = yuri::Voice::from_addon(&engine, addon.as_ref(), Default::default())?;

    let mut chunk_count = 0;
    let mut total_samples = 0;

    voice.speak("The quick brown fox jumps over the lazy dog", |event| {
        match event {
            yuri::SpeechEvent::Audio(chunk) => {
                chunk_count += 1;
                total_samples += chunk.samples.len();
                println!(
                    "Chunk {}: {} samples ({:.0}ms)",
                    chunk_count,
                    chunk.samples.len(),
                    chunk.samples.len() as f64 / yuri::SAMPLE_RATE as f64 * 1000.0,
                );
            }
            yuri::SpeechEvent::Done => {
                println!("Done! {} chunks, {} total samples", chunk_count, total_samples);
            }
        }
        Ok(())
    })?;

    Ok(())
}
