#!/bin/sh
# Replays a feedback bundle exported from Settings › Records through the ASR and prints the user's result next to today's.
# usage: scripts/replay.sh plaid-feedback-xxx.zip   (needs the API key in the Keychain or PLAID_API_KEY; jq ships with macOS)
set -e
BIN="$(dirname "$0")/../build/Plaid.app/Contents/MacOS/Plaid"
DIR=$(mktemp -d)
ditto -x -k "$1" "$DIR"
ROOT=$(dirname "$(find "$DIR" -name records.jsonl | head -1)")
cat "$ROOT/info.json"; echo
jq -c 'select(.audio != null)' "$ROOT/records.jsonl" | while read -r r; do
  a=$(echo "$r" | jq -r .audio)
  echo "== $a  noise $(echo "$r" | jq -r .noiseRMS) speech $(echo "$r" | jq -r .speechRMS)"
  echo "then:   $(echo "$r" | jq -r .raw)"
  [ "$(echo "$r" | jq -r '.edited // empty')" ] && echo "edited: $(echo "$r" | jq -r .edited)"
  ctx=$(echo "$r" | jq -c '.ctx // empty')
  "$BIN" --file "$ROOT/audio/$a" ${ctx:+--ctx "$ctx"} < /dev/null | grep '^final' | sed 's/^/now:    /'
done
