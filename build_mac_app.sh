#!/bin/bash
set -e

if [ ! -d "venv" ]; then
    echo "Setting up Python virtual environment..."
    python3 -m venv venv
    source venv/bin/activate
    echo "Installing Python dependencies..."
    pip install pyinstaller playwright gemini_webapi loguru pillow
else
    source venv/bin/activate
fi

# Rebuild python scripts if dist binaries don't exist or py files were modified
if [ ! -f "dist/gemini_auto" ] || [ "gemini_auto.py" -nt "dist/gemini_auto" ]; then
    echo "Bundling gemini_auto.py..."
    pyinstaller --onefile gemini_auto.py
fi

if [ ! -f "dist/gemini_download" ] || [ "gemini_download.py" -nt "dist/gemini_download" ]; then
    echo "Bundling gemini_download.py..."
    pyinstaller --onefile gemini_download.py
fi

echo "Building Rust app with cargo-bundle..."
if ! command -v cargo-bundle &> /dev/null; then
    echo "Installing cargo-bundle..."
    cargo install cargo-bundle
fi

cargo bundle --release

echo "Copying Python executables into the app bundle..."
APP_BUNDLE="target/release/bundle/osx/Gemini Native Client.app"
cp dist/gemini_auto "$APP_BUNDLE/Contents/MacOS/"
cp dist/gemini_download "$APP_BUNDLE/Contents/MacOS/"

echo "Build complete! The app is located at: $APP_BUNDLE"
echo "You can move it to your Applications folder by running:"
echo "cp -R \"$APP_BUNDLE\" /Applications/"
