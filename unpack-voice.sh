#!/bin/bash
set -e

if [ -z "$1" ]; then
    echo "Usage: $0 <addon-file> [output-dir]"
    echo ""
    echo "Extracts voice data from a Vocalizer NVDA addon for use with Yuri TTS."
    echo ""
    echo "Examples:"
    echo "  $0 Vocalizer-Expressive2-voice-Russian-Yuri-EmbeddedHigh.nvda-addon"
    echo "  $0 addon.nvda-addon wasm/voicedata_milena"
    exit 1
fi

ADDON="$1"
OUTDIR="${2:-wasm/voicedata_custom}"
COMMON_DIR="wasm/voicedata_enu"
TMPDIR=$(mktemp -d)

if [ ! -f "$ADDON" ]; then
    echo "Error: file not found: $ADDON"
    exit 1
fi

echo "Extracting voice from: $(basename "$ADDON")"

# Extract all .dat and .hdr files
unzip -o "$ADDON" "*.dat" "*.hdr" -d "$TMPDIR" 2>/dev/null

# Find the voice-specific files (the .hdr tells us the voice)
HDR=$(find "$TMPDIR" -name "ve_pipeline_*.hdr" -type f | head -1)
if [ -z "$HDR" ]; then
    echo "Error: no pipeline header (.hdr) found in addon"
    rm -rf "$TMPDIR"
    exit 1
fi

HDR_NAME=$(basename "$HDR")
echo "Found pipeline: $HDR_NAME"

# Auto-name output dir from the pipeline header if using default
if [ "$OUTDIR" = "wasm/voicedata_custom" ]; then
    # Extract voice name from: ve_pipeline_rur_yuri_22_embedded-high_2-0-1.hdr
    VOICE=$(echo "$HDR_NAME" | sed 's/ve_pipeline_//;s/_22_.*//;s/_/-/g')
    OUTDIR="wasm/voicedata_${VOICE}"
fi

mkdir -p "$OUTDIR"

# Copy all extracted .dat and .hdr to output
find "$TMPDIR" -name "*.dat" -type f -exec cp {} "$OUTDIR/" \;
find "$TMPDIR" -name "*.hdr" -type f -exec cp {} "$OUTDIR/" \;
rm -rf "$TMPDIR"

# Add common files from English voice data
if [ -d "$COMMON_DIR" ]; then
    for f in sysdct.dat clm.dat lid.dat synth_med_fxd_bet3f22.dat; do
        if [ -f "$COMMON_DIR/$f" ] && [ ! -f "$OUTDIR/$f" ]; then
            cp "$COMMON_DIR/$f" "$OUTDIR/$f"
        fi
    done
else
    echo "Warning: $COMMON_DIR not found. Common files (sysdct.dat, clm.dat, lid.dat) must be added manually."
fi

echo ""
echo "Voice data ready in: $OUTDIR"
ls -lh "$OUTDIR/"
echo ""
echo "Test with:"
echo "  cargo run --release -- --text 'Привет мир' --voice-dir $OUTDIR -o output.wav"
