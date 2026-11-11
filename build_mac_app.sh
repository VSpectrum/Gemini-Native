#!/bin/bash
set -euo pipefail

cd "$(dirname "$0")"

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

# cargo-bundle 0.12 pulls in sigstore, aws-lc, and rav1e (hundreds of crates)
# only to emit a macOS .app. The icon already exists, so wrap the binary here.
# Thin LTO and more codegen units keep this finishable on older CPUs.
# Cargo.toml's release profile is unchanged for a plain `cargo build --release`.
echo "Building release binary..."
cargo build --release --bin gemini_native_client \
    --config 'profile.release.lto="thin"' \
    --config 'profile.release.codegen-units=8'

APP_BUNDLE="target/release/bundle/osx/Gemini Native Client.app"
MACOS_DIR="$APP_BUNDLE/Contents/MacOS"
RESOURCES_DIR="$APP_BUNDLE/Contents/Resources"

rm -rf "$APP_BUNDLE"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"

cp "target/release/gemini_native_client" "$MACOS_DIR/"
cp "dist/gemini_auto" "$MACOS_DIR/"
cp "dist/gemini_download" "$MACOS_DIR/"
cp "assets/icon.icns" "$RESOURCES_DIR/icon.icns"
chmod +x "$MACOS_DIR/gemini_native_client" "$MACOS_DIR/gemini_auto" "$MACOS_DIR/gemini_download"

cat > "$APP_BUNDLE/Contents/Info.plist" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>English</string>
    <key>CFBundleDisplayName</key>
    <string>Gemini Native Client</string>
    <key>CFBundleExecutable</key>
    <string>gemini_native_client</string>
    <key>CFBundleIconFile</key>
    <string>icon</string>
    <key>CFBundleIdentifier</key>
    <string>com.vspectrum.gemini-native-client</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>Gemini Native Client</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>10.13</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
EOF

echo "Build complete! The app is located at: $APP_BUNDLE"
echo "You can move it to your Applications folder by running:"
echo "cp -R \"$APP_BUNDLE\" /Applications/"
