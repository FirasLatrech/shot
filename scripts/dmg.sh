#!/bin/sh
# Packages target/release/Shot.app into target/release/Shot-<version>.dmg
# with the usual "drag to Applications" layout. Run scripts/bundle.sh first.
set -e
cd "$(dirname "$0")/.."

APP=target/release/Shot.app
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
DMG=target/release/Shot-$VERSION.dmg
STAGE=$(mktemp -d)
trap 'rm -rf "$STAGE"' EXIT

[ -d "$APP" ] || { echo "Missing $APP; run scripts/bundle.sh first" >&2; exit 1; }
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -quiet -volname "Shot $VERSION" -srcfolder "$STAGE" -fs HFS+ -format UDZO "$DMG"
echo "Built $DMG"
