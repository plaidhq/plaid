# Plaid

Voice dictation for macOS and Windows. Hold a key, speak, release: the text is pasted into the focused input field.
Recognition uses Volcengine's Doubao streaming ASR (bigmodel). An optional LLM pass cleans up fillers, typos and
punctuation before pasting. The UI is in Simplified Chinese.

## Requirements

- macOS 14+ on Apple silicon, or Windows 11 x64.
- A Volcengine API key with Doubao streaming speech recognition (bigmodel) enabled. Plaid is bring-your-own-key.
- Optional: any OpenAI-compatible or Anthropic-compatible LLM endpoint for polishing.

## Install

Download the zip for your platform from [Releases](https://github.com/plaidhq/plaid/releases). The builds are unsigned.

- **macOS**: unzip, move `Plaid.app` to Applications and open it once. When macOS blocks it, go to System Settings ›
  Privacy & Security and choose Open Anyway. Grant Microphone and Accessibility when asked; Accessibility is used
  for the global hotkey and for pasting. Each update has a new ad-hoc signature, so macOS asks for Accessibility
  again.
- **Windows**: unzip and run `plaid.exe`. On the SmartScreen prompt choose More info › Run anyway.

Plaid checks GitHub for new releases every 6 hours and can update itself (设置 › 关于).

## Setup

1. Open 设置 › 服务 and paste the Volcengine API key. Keys are stored locally (Keychain on macOS, Credential
   Manager on Windows).
2. Optional: under LLM on the same page, pick a preset or enter an endpoint, model and key, then run 测试. Turn
   polishing on in 设置 › 润色 and choose a level (关 / 轻度 / 通顺), per app if you like.

## Use

- **Hold right ⌥ (right Alt on Windows)** to dictate; release to paste.
- **Tap it** for hands-free dictation; tap again or press ⏎ to finish, esc to cancel.
- Both keys can be changed or disabled in 设置 › 通用.

Other features:

- **词典**: hotwords sent to the ASR and the LLM, plus rewrite rules (heard → wanted). Plaid suggests rules from your
  manual edits after a paste and from LLM corrections; thresholds for auto-applying them are settable.
- **命令**: map a spoken phrase to a shell command. The rest of the sentence is in `$PLAID_ARGS`, the whole
  sentence in `$PLAID_TEXT`.
- **历史**: recent recordings with playback, re-polish, retry and export.
- **概览**: usage and latency numbers.

## Data

- Audio goes to Volcengine. With 发送上下文 on, a short piece of text before the cursor and the app name go too.
- With polishing or background checking on, the recognized text, app name and window title go to the LLM endpoint
  you configured.
- History, recordings and edit records stay on this machine. Keeping recordings can be turned off in 设置 › 数据, and
  entries can be deleted in 历史.

## Build

The app is Rust, using [GPUI](https://www.gpui.rs) through `gpui-kit`. It lives in `app/`.

```sh
cd app
cargo run -p plaid              # dev build; on macOS its default hotkey is right ⌘
cargo test --workspace
./bundle.sh release             # macOS: target/Plaid.app
```

Headless helpers:

- `plaid --file a.wav` replays a recording through the pipeline.
- `plaid --recover [at]` re-recognizes a saved recording.
- `plaid --learn-report` prints what the learner has collected.
- `plaid --update` updates from the command line.

CI builds and tests on Windows on every push to `main`. A `v*` tag builds both platforms and publishes a release.

## License

[Apache-2.0](LICENSE).
