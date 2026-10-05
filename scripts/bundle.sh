#!/bin/sh
# Builds Shot.app (a menu-bar app, no Dock icon) into target/release/.
set -e
cd "$(dirname "$0")/.."
cargo build --release -p shot

APP=target/release/Shot.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/shot "$APP/Contents/MacOS/Shot"
# Regenerate with: cargo run -p shot-core --example icon -- icon.png, then iconutil.
cp assets/AppIcon.icns "$APP/Contents/Resources/"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Shot</string>
    <key>CFBundleDisplayName</key><string>Shot</string>
    <key>CFBundleIdentifier</key><string>dev.shot.Shot</string>
    <key>CFBundleExecutable</key><string>Shot</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
# A stable signing identity keeps the Screen Recording permission across
# rebuilds; ad-hoc signing ("-") works too but macOS forgets the grant each build.
IDENTITY=${SHOT_SIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk -F'"' 'NR==1 {print $2}')}
codesign --force --sign "${IDENTITY:--}" "$APP"
echo "Built $APP"
