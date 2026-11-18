# Performance and recovery pass — v0.9.13

## Behavior

- Results are queued for persistence before automatic delivery. Delivery validates the original app, window and text element when Accessibility exposes them, without activating the target. A changed/unverifiable target or a newer utterance leaves the result in History.
- Clipboard restoration checks ownership with `changeCount`; a newer user copy is never overwritten. Overlapping Plaid pastes restore the original clipboard rather than an earlier dictation result.
- Recognition errors retain safe partial text and, when enabled, audio. History replay updates the original record; it never pastes, refines or executes a voice command. Cloud replay requires confirmation.
- Escape cancels waiting phases. Starting another dictation while finalizing preserves the interrupted attempt; starting while refining saves the raw result and cancels the abandoned HTTP task.
- Each ASR session holds an immutable hotword snapshot; cloud response processing runs off-main and deduplicates raw responses first.
- Records, audio, logs, hotword writes and exports use an ordered background I/O queue. History retains the newest 1000 records consistently. Clearing history invalidates derived rules while retaining aggregate usage.
- Background learning is serialized, has a bounded backlog, and defers starting jobs/result processing during live dictation. Skipping foreground refinement does not start a duplicate learner request.
- Dictionary editing coalesces writes and flushes when leaving/closing. Settings use device identity rather than display names, observe default-input changes while visible, and label controls for accessibility.

## Measurement semantics

`triggerMs` records physical hotkey press to application start; `micStartMs` records application start to successful audio-engine start. Existing `firstTextMs` still starts at application start, not speech onset. `totalMs` includes delivery validation and ends when the paste key is posted, not when another app confirms insertion. Saved/failed records do not enter latency aggregates.

## Validation

Build and application self-test:

```sh
./build.sh
CFFIXED_USER_HOME="$(mktemp -d)" PLAID_AUTO_LEARN=0 \
  build/Plaid.app/Contents/MacOS/Plaid --selftest
```

Isolated checks (no user clipboard, model loading or network calls):

```sh
swiftc Sources/Plaid/Hotkeys.swift scripts/hotkeys_check.swift -o /tmp/plaid-hotkeys-check
/tmp/plaid-hotkeys-check
swiftc Sources/Plaid/Records.swift Sources/Plaid/Opus.swift scripts/records_check.swift -o /tmp/plaid-records-check
/tmp/plaid-records-check
swiftc Sources/Plaid/Recovery.swift scripts/recovery_check.swift -o /tmp/plaid-recovery-check
/tmp/plaid-recovery-check
```

A timestamped, PID-targeted modifier driver and local WebSocket fixture exercised the actual GUI without sending keystrokes to the foreground app. Checks covered chord ordering, late-chord cancellation, continuous mode, preserving partial/audio on disconnect, and taking over an unfinished final. Exported bundles were inspected. History, General and Dictionary were rendered with isolated data and visually inspected; cached captures do not draw the sidebar material.

Six real recordings (1.2–78.8 seconds) were replayed before/after at 4x feed speed using the same 0.6B model and hotwords. All six final strings matched exactly. This is a regression sample, not a broad accuracy evaluation or real-time latency benchmark.

One 25.9-second recording was also replayed at real-time speed:

| Metric | Before | After |
| --- | ---: | ---: |
| Partial inference count | 25 | 20 |
| Cumulative partial inference time | 4364 ms | 3410 ms |
| Release to final | 698 ms | 697 ms |
| Final text | identical | identical |

The single-clip reduction in repeated inference work is not a measured reduction in whole-app CPU, GPU energy, or end-to-end latency. VoiceOver reading order, physical default-device switching, and destructive model removal were not exercised end-to-end.
