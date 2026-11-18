#!/bin/sh
# Builds Plaid.app (arm64) and packs it with README.txt into build/Plaid-<version>.dmg.
# Signed with Apple Development, not notarized: recipients allow it once in Privacy & Security (see README).
set -e
cd "$(dirname "$0")/.."
./build.sh
VER=$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' Info.plist)
STAGE=build/dmg
rm -rf "$STAGE" && mkdir -p "$STAGE"
cp -R build/Plaid.app "$STAGE/"
cp dist-assets/README.txt "$STAGE/使用说明.txt"
ln -s /Applications "$STAGE/Applications"
OUT=build/Plaid-$VER.dmg
rm -f "$OUT"
hdiutil create -volname "Plaid $VER" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$OUT" >/dev/null
codesign --force -s "${SIGN_ID:-Apple Development}" "$OUT" 2>/dev/null || true
echo "$OUT"
