#!/bin/sh
# Builds build/Plaid.app. A stable signing identity keeps mic/Accessibility grants across rebuilds.
set -e
cd "$(dirname "$0")"
FLAGS="-c release"
swift build $FLAGS

APP=build/Plaid.app
rm -rf "$APP" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$(swift build $FLAGS --show-bin-path)/Plaid" "$APP/Contents/MacOS/"
cp Info.plist "$APP/Contents/"
codesign --force -s "${SIGN_ID:-Apple Development}" "$APP" 2>/dev/null || codesign --force -s - "$APP"
echo "$APP"
