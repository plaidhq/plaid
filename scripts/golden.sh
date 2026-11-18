#!/bin/sh
# Golden fixtures for the Rust port, produced by the Swift implementation itself.
# Usage: scripts/golden.sh [records.jsonl] [hotwords.txt]  → app/core/tests/golden/*.jsonl
# Fixtures built from your own records go to *.private.jsonl (gitignored).
set -e
cd "$(dirname "$0")/.."
S=Sources/Plaid
D=app/core/tests/golden
mkdir -p "$D"
T=$(mktemp -d)
# Print a member from its first line to the closing brace at 4-space indent.
fn() { awk -v p="$1" 'index($0, p) { on = 1 } on { print } on && /^    }$/ { exit }' "$2"; }
{
  echo "import NaturalLanguage"
  cat $S/Phonetic.swift
  sed -n '1,180p' $S/Volc.swift
  sed -n '/^enum Diff {/,$p' $S/Field.swift
  echo "enum UsageG {"; fn 'static func chars(' $S/Records.swift; echo "}"
  echo "enum RecordsG {"; fn 'static func rules(' $S/Records.swift; echo "}"
  sed -n '/^struct Action: /,/^}/p' $S/Actions.swift
  echo "enum ActionsG {"
  fn 'enum Parsed: Equatable {' $S/Actions.swift
  for f in 'static func parse(_ text: String, _ actions' 'private static func isSep(' 'private static func strip(' 'private static func trim('; do fn "$f" $S/Actions.swift; done
  echo "}"
  cat scripts/golden_driver.swift
} > "$T/golden.swift"
swiftc -O -o "$T/golden" "$T/golden.swift" -framework AppKit
"$T/golden" "$D" "${1:-$HOME/Library/Application Support/Plaid/records.jsonl}" "${2:-$HOME/Library/Application Support/Plaid/hotwords.txt}"
rm -rf "$T"
