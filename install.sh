#!/bin/bash

# imgfind - CLIP-based Image Search CLI Installer
# This script installs imgfind to your local system

set -e

echo "🔍 imgfind Installation Script"
echo "================================"

# Get the current directory (assumes we're in the project root)
PROJECT_DIR=$(pwd)
BINARY_PATH="$PROJECT_DIR/target/release/imgfind"

# Check if release binary exists
if [ ! -f "$BINARY_PATH" ]; then
    echo "❌ Release binary not found at $BINARY_PATH"
    echo "Please run 'cargo build --release' first."
    exit 1
fi

# Create local bin directory if it doesn't exist
LOCAL_BIN="$HOME/.local/bin"
mkdir -p "$LOCAL_BIN"

# Copy binary to local bin
echo "📦 Installing imgfind to $LOCAL_BIN..."
cp "$BINARY_PATH" "$LOCAL_BIN/imgfind"
chmod +x "$LOCAL_BIN/imgfind"

echo "✅ Installation complete!"
echo ""
echo "To use imgfind, make sure $LOCAL_BIN is in your PATH."
echo "Add this line to your ~/.zshrc or ~/.bashrc:"
echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
echo ""
echo "🚀 Quick start:"
echo "    imgfind index --dir ~/Pictures"
echo "    imgfind search \"a beautiful sunset\""
echo "    imgfind status"
echo ""
echo "📚 For more information, see USAGE.md"
