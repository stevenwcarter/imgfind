#!/bin/bash

# imgfind - CLIP-based Image Search CLI Installer
# This script installs imgfind to your local system

set -e

echo "🔍 imgfind Installation Script"
echo "================================"

# Get the current directory (assumes we're in the project root)
PROJECT_DIR=$(pwd)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Resolve where the prebuilt binaries live: a local release build, or — for the
# downloaded release tarball — right next to this script.
if [ -f "$PROJECT_DIR/target/release/imgfind" ]; then
    SRC_DIR="$PROJECT_DIR/target/release"
elif [ -f "$SCRIPT_DIR/imgfind" ]; then
    SRC_DIR="$SCRIPT_DIR"
else
    echo "❌ imgfind binary not found in $PROJECT_DIR/target/release or $SCRIPT_DIR"
    echo "Build first ('cargo build --release --workspace') or run this from an extracted release tarball."
    exit 1
fi

# Create local bin directory if it doesn't exist
LOCAL_BIN="$HOME/.local/bin"
mkdir -p "$LOCAL_BIN"

# Copy CLI binary to local bin
echo "📦 Installing imgfind to $LOCAL_BIN..."
cp "$SRC_DIR/imgfind" "$LOCAL_BIN/imgfind"
chmod +x "$LOCAL_BIN/imgfind"

# Copy GUI binary if present
GUI_BINARY="$SRC_DIR/imgfind-gui"
if [ -f "$GUI_BINARY" ]; then
    echo "📦 Installing imgfind-gui to $LOCAL_BIN..."
    cp "$GUI_BINARY" "$LOCAL_BIN/imgfind-gui"
    chmod +x "$LOCAL_BIN/imgfind-gui"
fi

# Copy launcher binary if present
LAUNCHER_BINARY="$SRC_DIR/imgfind-launcher"
if [ -f "$LAUNCHER_BINARY" ]; then
    echo "📦 Installing imgfind-launcher to $LOCAL_BIN..."
    cp "$LAUNCHER_BINARY" "$LOCAL_BIN/imgfind-launcher"
    chmod +x "$LOCAL_BIN/imgfind-launcher"

    # Install a desktop entry so it shows up in the OS application menu.
    # XDG .desktop entries are Linux-only; skip on macOS/other platforms.
    if [ "$(uname -s)" = "Linux" ]; then
        DESKTOP_DIR="$HOME/.local/share/applications"
        mkdir -p "$DESKTOP_DIR"
        DESKTOP_SRC="$SCRIPT_DIR/packaging/imgfind-launcher.desktop"
        if [ -f "$DESKTOP_SRC" ]; then
            # Install the app icon and point the desktop entry at it by absolute
            # path. An absolute Icon= is the most reliable choice for a ~/.local
            # install: it needs no icon-theme cache refresh or size-named theme
            # directory. If the icon isn't shipped, leave the themed name so a
            # system-wide icon (e.g. /usr/share/pixmaps/imgfind.png) can resolve.
            ICON_SRC="$SCRIPT_DIR/packaging/icon.png"
            ICON_LINE="Icon=imgfind"
            if [ -f "$ICON_SRC" ]; then
                ICON_DIR="$HOME/.local/share/icons"
                mkdir -p "$ICON_DIR"
                echo "🎨 Installing app icon to $ICON_DIR/imgfind.png..."
                cp "$ICON_SRC" "$ICON_DIR/imgfind.png"
                ICON_LINE="Icon=$ICON_DIR/imgfind.png"
            fi

            echo "🖥  Installing desktop entry to $DESKTOP_DIR..."
            sed -e "s|^Exec=imgfind-launcher\$|Exec=$LOCAL_BIN/imgfind-launcher|" \
                -e "s|^Icon=imgfind\$|$ICON_LINE|" \
                "$DESKTOP_SRC" > "$DESKTOP_DIR/imgfind-launcher.desktop"

            # Best-effort: refresh the desktop database so the entry appears
            # promptly. Harmless if the tool is absent.
            if command -v update-desktop-database >/dev/null 2>&1; then
                update-desktop-database "$DESKTOP_DIR" >/dev/null 2>&1 || true
            fi
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
