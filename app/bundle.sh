#!/bin/sh
# Builds the macOS app bundle: ./bundle.sh [dev|release]  (default dev → "Plaid Dev.app", com.neospaceindustries.plaid.dev).
# No hardened runtime: ad-hoc and Apple Development signatures both work without the audio-input entitlement.
# A stable signing identity keeps Microphone/Accessibility grants across rebuilds; ad-hoc builds lose them.
set -e
cd "$(dirname "$0")"
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
MODE=${1:-dev}
if [ "$MODE" = release ]; then NAME=Plaid; ID=com.neospaceindustries.plaid; else NAME="Plaid Dev"; ID=com.neospaceindustries.plaid.dev; fi
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
BUILD=${BUILD:-$(date +%Y%m%d%H%M)}

cargo build --release -p plaid
APP="target/$NAME.app"
rm -rf "$APP" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/plaid "$APP/Contents/MacOS/"
cp plaid/resources/icon.icns "$APP/Contents/Resources/"
sed -e "s/@ID@/$ID/" -e "s/@NAME@/$NAME/" -e "s/@VERSION@/$VERSION/" -e "s/@BUILD@/$BUILD/" plaid/resources/Info.plist > "$APP/Contents/Info.plist"
codesign --force -s "${SIGN_ID:-Apple Development}" -i "$ID" "$APP" 2>/dev/null || codesign --force -s - -i "$ID" "$APP"
echo "$APP"
