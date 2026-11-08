#!/bin/sh
# Builds FlowGauge.app into ~/Applications with an ad-hoc signature.
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
cargo build --release -p gauge-app -p gauge-fand -p gauge
app="$HOME/Applications/FlowGauge.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/gauge-app target/release/gauge-fand "$app/Contents/MacOS/"
cp scripts/install-helper.sh scripts/uninstall-helper.sh "$app/Contents/Resources/"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>FlowGauge</string>
  <key>CFBundleDisplayName</key><string>FlowGauge</string>
  <key>CFBundleIdentifier</key><string>com.flowgauge.app</string>
  <key>CFBundleExecutable</key><string>gauge-app</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>LSUIElement</key><true/>
  <key>NSLocationUsageDescription</key><string>macOS treats your Wi-Fi network name as location data. FlowGauge only shows the name in its Network panel.</string>
  <key>NSLocationWhenInUseUsageDescription</key><string>macOS treats your Wi-Fi network name as location data. FlowGauge only shows the name in its Network panel.</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
</dict>
</plist>
PLIST
codesign --force --deep --sign - "$app"
echo "$app"
