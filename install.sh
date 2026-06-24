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
    echo "Please run 'cargo build --release --workspace' first."
    exit 1
fi

# Create local bin directory if it doesn't exist
LOCAL_BIN="$HOME/.local/bin"
mkdir -p "$LOCAL_BIN"

# Copy CLI binary to local bin
echo "📦 Installing imgfind to $LOCAL_BIN..."
cp "$BINARY_PATH" "$LOCAL_BIN/imgfind"
chmod +x "$LOCAL_BIN/imgfind"

# Copy GUI binary if present
GUI_BINARY="$PROJECT_DIR/target/release/imgfind-gui"
if [ -f "$GUI_BINARY" ]; then
    echo "📦 Installing imgfind-gui to $LOCAL_BIN..."
    cp "$GUI_BINARY" "$LOCAL_BIN/imgfind-gui"
    chmod +x "$LOCAL_BIN/imgfind-gui"
fi

# Copy launcher binary if present
LAUNCHER_BINARY="$PROJECT_DIR/target/release/imgfind-launcher"
if [ -f "$LAUNCHER_BINARY" ]; then
    echo "📦 Installing imgfind-launcher to $LOCAL_BIN..."
    cp "$LAUNCHER_BINARY" "$LOCAL_BIN/imgfind-launcher"
    chmod +x "$LOCAL_BIN/imgfind-launcher"

    # Install a desktop entry so it shows up in the OS application menu.
    # XDG .desktop entries are Linux-only; skip on macOS/other platforms.
    if [ "$(uname -s)" = "Linux" ]; then
        DESKTOP_DIR="$HOME/.local/share/applications"
        mkdir -p "$DESKTOP_DIR"
        DESKTOP_SRC="$PROJECT_DIR/packaging/imgfind-launcher.desktop"
        if [ -f "$DESKTOP_SRC" ]; then
            echo "🖥  Installing desktop entry to $DESKTOP_DIR..."
            sed "s|^Exec=imgfind-launcher\$|Exec=$LOCAL_BIN/imgfind-launcher|" \
                "$DESKTOP_SRC" > "$DESKTOP_DIR/imgfind-launcher.desktop"
        fi
    fi
fi

echo "✅ Installation complete!"
echo ""
echo "To use imgfind, make sure $LOCAL_BIN is in your PATH."
echo "Add this line to your ~/.zshrc or ~/.bashrc:"
echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
echo ""
echo "🚀 Quick start:"
echo "    imgfind index --dir ~/Pictures"
echo "    imgfind search \"a beautiful sunset\""
echo "    imgfind-gui --dir ~/Pictures"
echo "    imgfind-launcher        # graphical launcher (or find 'imgfind' in your app menu)"
echo "    imgfind status"
echo ""
echo "📚 For more information, see USAGE.md"
