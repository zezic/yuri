#!/bin/bash
set -e

ADDON_URL="https://nvda-addons.ru/download.php?file=vocalizer_expressive_voice_yuri_Premium_High"
ADDON_FILE="assets/vocalizer-voice-yuri-PremiumHigh.nvda-addon"
VOICE_DIR="wasm/voicedata_yuri_full"
TARGET_FILE="$VOICE_DIR/synth_yuri_full_vssq5_f22.dat"

if [ -f "$TARGET_FILE" ]; then
    echo "Voice data already set up: $TARGET_FILE"
    echo ""
    echo "Run with:"
    echo "  cargo run --release -- --text 'Привет мир' --voice-dir $VOICE_DIR -o privet.wav"
    exit 0
fi

# Download the addon if not present
if [ ! -f "$ADDON_FILE" ]; then
    echo "Downloading Yuri PremiumHigh voice (~134MB)..."
    mkdir -p assets
    curl -L "$ADDON_URL" -o "$ADDON_FILE"
    echo "Downloaded: $ADDON_FILE ($(du -h "$ADDON_FILE" | cut -f1))"
fi

echo "Extracting voice synthesis database..."
unzip -o "$ADDON_FILE" "rur/speech/components/synth_yuri_full_vssq5_f22.dat" -d /tmp/yuri_extract
mv /tmp/yuri_extract/rur/speech/components/synth_yuri_full_vssq5_f22.dat "$TARGET_FILE"
rm -rf /tmp/yuri_extract

echo ""
echo "Done! Yuri PremiumHigh voice ready ($(du -h "$TARGET_FILE" | cut -f1))"
echo ""
echo "Test with:"
echo "  cargo run --release -- --text 'Привет мир' --voice-dir $VOICE_DIR -o privet.wav"
