#!/bin/bash
set -e

# EmbeddedHigh Yuri (50MB) is included in the repo and works out of the box.
# This script sets up PremiumHigh Yuri (144MB synthesis database) for higher quality.

PREMIUM_URL="https://nvda-addons.ru/download.php?file=vocalizer_expressive_voice_yuri_Premium_High"
PREMIUM_FILE="assets/vocalizer-voice-yuri-PremiumHigh.nvda-addon"
VOICE_DIR="wasm/voicedata_yuri_full"
TARGET_FILE="$VOICE_DIR/synth_yuri_full_vssq5_f22.dat"

echo "Yuri voice setup"
echo "================"
echo ""
echo "EmbeddedHigh (50MB) is already included in the repo:"
echo "  cargo run --release -- --text 'Привет мир' --voice-dir wasm/voicedata_yuri_high -o privet.wav"
echo ""

if [ -f "$TARGET_FILE" ]; then
    echo "PremiumHigh is also ready: $TARGET_FILE"
    echo "  cargo run --release -- --text 'Привет мир' --voice-dir $VOICE_DIR -o privet.wav"
    exit 0
fi

echo "Setting up PremiumHigh quality (144MB synthesis database)..."
echo ""

# Download the addon if not present
if [ ! -f "$PREMIUM_FILE" ]; then
    echo "Downloading Yuri PremiumHigh voice (~134MB)..."
    mkdir -p assets
    curl -L "$PREMIUM_URL" -o "$PREMIUM_FILE"
    echo "Downloaded: $PREMIUM_FILE ($(du -h "$PREMIUM_FILE" | cut -f1))"
fi

echo "Extracting voice synthesis database..."
unzip -o "$PREMIUM_FILE" "rur/speech/components/synth_yuri_full_vssq5_f22.dat" -d /tmp/yuri_extract
mv /tmp/yuri_extract/rur/speech/components/synth_yuri_full_vssq5_f22.dat "$TARGET_FILE"
rm -rf /tmp/yuri_extract

echo ""
echo "Done! PremiumHigh Yuri ready ($(du -h "$TARGET_FILE" | cut -f1))"
echo "  cargo run --release -- --text 'Привет мир' --voice-dir $VOICE_DIR -o privet.wav"
