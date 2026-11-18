# UI pass — v0.9.14

## Changes

- Dictation HUD uses explicit state labels, stable transcript geometry and opacity transitions instead of spring-resizing on each partial. Long previews select the newest text before layout; full text remains available to accessibility and is not modified for recognition or delivery.
- The HUD retains its initial screen anchor throughout the utterance. Saved-but-undelivered feedback lasts eight seconds and points to History; the menu bar now has a direct History entry. The overlay remains nonactivating and click-through.
- General settings show everyday controls first. Microphone processing, cloud transport/second-pass settings and tail tuning are grouped under Advanced. Model status includes disk usage, and installed model management expands alongside model selection. Existing configuration values and defaults are unchanged.
- Setting rows fall back to stacked labels/controls when horizontal space is insufficient. The settings window supports a 900-point minimum width, down from 940.
- History filters all, undelivered and failed records. Selection and hover reveal copy/retry actions; diagnostic details expand separately. Active recovery identifies its original record so only that row shows progress. Retry remains recognition-only, with explicit cloud-upload confirmation.
- Dictionary entries are searchable and individually editable. Forms distinguish hotwords, literal replacements and advanced single-line syntax. Alias/exclusion entries retain their original syntax; unrelated lines and comments are preserved. Suggestions support batch acceptance and confirmed dismissal. Raw editing remains available.

## Validation

The actual application was driven through PID-scoped Accessibility and keyboard events, using isolated `CFFIXED_USER_HOME` fixtures. Tests did not type into other applications, paste into the foreground app, play audio, call external services, or delete real models.

Verified interactions:

- Advanced settings collapse/expand and installed-model inventory discovery.
- History filtering, search, selected-row actions, arrow-key selection and cloud replay confirmation/cancellation.
- Dictionary hotword creation, literal replacement creation/editing, preserving comments and aliases/exclusions, and searching editable rules.
- Application self-test and recovery coordinator checks, including active-record identity cleanup.
- Actual HUD state cycling and rendered light/dark window captures at 900-point width, inspected visually. Cached window captures do not render the native sidebar material.

Physical multi-display switching and VoiceOver speech output were not exercised. No recognition/model algorithms or dictionary parser semantics changed.
