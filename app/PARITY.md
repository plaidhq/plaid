# Parity with the Swift app

State of the GPUI app against the Swift Volc-only app it replaced (v0.10.0 build 31, bundle id dev.neo.plaid.volc).
The Swift sources were removed after commit 71d69dc; read them there. Bundle ids are now com.neospaceindustries.plaid
(release) and com.neospaceindustries.plaid.dev. The one-time settings/Keychain import ran on 2026-11-18 and was
removed afterwards (migrate.rs, commit fa9d596).

## Ported

| Swift | Rust | Notes |
|---|---|---|
| main.swift Phase machine | core/dictation.rs `Controller` | Pure; driver does the I/O. Replay: `plaid --file a.wav`. |
| Hotkeys.swift | core/gesture.rs, plaid/hotkey.rs | macOS CGEventTap (hotkey/cgtap.rs), Windows WH_KEYBOARD_LL (hotkey/llhook.rs). |
| Volc.swift | core/session.rs, core/volc.rs | |
| Phonetic.swift, hotwords | core/phonetic.rs, core/hotwords.rs | |
| Records.swift | core/store.rs | records.jsonl, usage.json and audio/*.wav are file-compatible. |
| Refine.swift | core/llm.rs + controller `Phase::Refining` | Fails open. |
| Learner.swift, Field.swift (EditWatch) | controller Learner, driver `Watch` | |
| Actions.swift | core/actions.rs, plaid/run.rs | Same env vars, including PLAID_BUNDLE. |
| Paste.swift | plaid/paste.rs, paste_win.rs, driver `verify_target` | The clipboard is restored on quit as well. |
| Mic.swift `micDevice` | audio.rs `input_device`, 声音 page picker | Same Core Audio UID; falls back to the default input. |
| Recovery.swift | controller `recover`, driver `recognize`, 历史 重试 | Also `plaid --recover [at]`; status goes to the HUD. |
| Settings.swift and its pages | plaid/ui/*.rs | Redesigned dark-only "Edge Light" look drawn in GPUI (ui/mav.rs); see **Settings window** below. |
| HUD | plaid/hud | Same states; restyled as a 10.9 HUD panel. |
| Onboarding | ui/onboarding.rs | Opens at launch until a Volc key is saved and accessibility is granted. |

## Behaviour differences

- **Volc ASR WebSocket connects direct.** It ignores both env and system proxies. Volc is a domestic endpoint.
  Swift's URLSession went through the system proxy.
- **LLM HTTP proxy order:**
  1. Loopback goes direct.
  2. Otherwise `HTTP(S)_PROXY` env vars.
  3. Otherwise the system proxy:
     - macOS: SCDynamicStoreCopyProxies (HTTP/HTTPS, ExceptionsList, ExcludeSimpleHostnames);
     - Windows: WinHttpGetIEProxyConfigForCurrentUser.

  PAC files and SOCKS-only setups are not followed.
- **Commands get the login shell's PATH.** It is resolved once (`$SHELL -lic`). It is prewarmed at launch when
  actions.json has entries.
- **Commands on Windows** run through `powershell -NoProfile -NonInteractive -Command`. A timeout kills only
  powershell, not its children (unix kills the process group). Not run on a real machine yet.
- **Recovery cannot be cancelled.** It runs from 历史 重试 (or `--recover`); the updated record shows in History,
  progress and failures only on the HUD. Dictation is refused while
  it runs, as in Swift. Like Swift it sends the whole recording at once; in 1 of 4 replays of a 17 s recording the
  server dropped part of the middle (the other 3 matched the live transcript).
- **A stalled connect ends at the finish deadline.** Commands queue while the WebSocket connects; key-up arms the
  5 s final deadline even before the socket is up (as Swift did), and dropping the session cancels at once.
- **Resampling** uses a windowed sinc filter (Swift used AVAudioConverter): -7 dB at 7.4 kHz, -35 dB at 8.2 kHz,
  under -48 dB from 8.4 kHz; output does not depend on the callback's buffer size; 2.6 ms CPU per second of 48 kHz
  input. 48 kHz and 44.1 kHz conversions of a recording replay to the same transcript as the 16 kHz original.
- **Recovered text keeps the rules.** It goes through the session's tidy and hotword rules, same as live text.

## Not ported

- Opus upload (Volc.swift `codec`, Swift default `opus`, 32 kbps). The port always streams PCM, about 8x the
  traffic; 设置 has no 上传格式 picker.
- `--export` CLI flag (历史 导出 writes the same feedback bundle) and `--ogg` (debug only).
- Windows focused-field text (needs UI Automation). Without it Windows has no PLAID_BEFORE, no field context
  for the ASR and no edit learning.

## New, not yet built

- Launch at login (macOS and Windows). The Swift app never had it.

## Settings window

Top tabs 今天, 报告, 历史 | 声音, 文字, 词典, 命令 | 设置. Space Grotesk and JetBrains Mono are bundled
(resources/fonts); CJK falls back to the system font. Tray 历史/设置 open the window on that page.
今天/报告/历史/词典/命令 carry over the Swift pages (概览, 性能, 历史, 词典, 命令). 声音 holds the shortcut,
microphone list with a live meter, recognition options, HUD states and HUD options (text on/off, reveal speed
跟着说话/快/立即, linger 0.4/0.7/1.5 s, start/paste chime, follow the mouse screen; chime and mouse screen are
macOS-only, no-ops on Windows); 设置 holds the Volc key and data folder.
文字 adds six local text rules (core/rules.rs: fillers, repeats, corrections, final 。, CJK–Latin spacing, Chinese
numerals), all off by default, applied before the LLM pass and without it, so short sentences get them too; then
the LLM level and its threshold (`llmMinChars`): 关 (rules only, `llmEnabled` 0), 轻度 (`llmLevel` light: a
punctuation-and-typos prompt, `llmPromptLight`) or 通顺 (the Swift prompt). 按应用 (`llmApps`, `app<TAB>level`
lines, matched case-insensitively on the frontmost app name) overrides the level per app, including 原文 (no rules,
no LLM). Drawn menus occlude, so a click on an entry never reaches the control under it.
Checked offscreen only: `plaid --ui-check <dir>` (snapshot feature, fixture data, no window shown, no device or
key access) clicks through every page and writes PNGs. Differences from Swift:

- 设置: no 测试 button for the Volc key; no launch-at-login switch.
- 历史: a list beside a detail panel (always shows the selected or newest record): waveform with playback, the
  recognised text with the rules' deletions struck through, what was pasted with the LLM's insertions marked, and
  the user's later edit; the waveform fills as the recording plays and the play button stops it. Records can be
  deleted (detail panel, right-click, ⌫; through the controller, refused while that record is being retried;
  usage is kept). ↑↓ move the selection, ⌘F focuses the search. Filters add 润色 and 改过. 重新润色 runs the LLM (the app's level,
  at least 15 s timeout, no history context) on the recognised text again and overwrites the record; it never pastes.
  重试 and 重新润色 cannot be cancelled and their failures show on the HUD only;
  export does not flush an unsaved 词典 draft first.
- 报告: new summary cards above the Swift 性能 diagnostics: half a year of daily characters as a heatmap and
  speaking speed (both from usage.json, so they survive 清空历史), a release → paste histogram split into direct and
  LLM, and how many records the LLM (punctuation vs other characters) and each local rule changed.
- 词典: no tabs or table. Hotwords are a wall of pills sized by how often the last 30 days of records used them
  (tinted when a rewrite rule targets them); the field filters the wall and adds a word on ⏎; clicking a pill opens
  the edit sheet, which also deletes. Aliases and replacements list in 改写规则; learned rules in 最近学会 with 保留
  (adds a 替换 line and drops the learned rule) or 撤销. No 来源 or 添加于. The tab badge counts pending suggestions.
- 命令: each command folds to one row (「name」, aliases, first shell line, output, ⏎ when it confirms); clicking a
  row expands its editor, one at a time, and a newly added command opens. An actions.json parse error locks only
  the editors and the add menu (as in the Swift source).
- 报告: the trend axis labels every second day.
- No tooltips anywhere.

## Windows

Built and tested on GitHub Actions (`.github/workflows/app.yml`, windows-latest): unit tests and the
`--file` replay against a mock Volcengine endpoint (plaid/tests/replay.rs). Never run on a desktop: no hotkey,
mic or paste has been exercised. Same settings and data layout as macOS; hotkey settings keep macOS key codes,
mapped from virtual keys in hotkey/vk.rs.

- **Hotkey**: right Alt (61) by default, also for dev builds. Win and Alt bindings send the unassigned vkE8
  around the press so releasing them doesn't open Start or the menu bar. Injected events are ignored, which
  includes keys produced by remappers (PowerToys, AutoHotkey): a remapped key cannot be the hotkey. The fake left
  Ctrl of AltGr layouts is skipped, so right Alt on those layouts still works as a hold key.
- **Paste**: snapshots every memory-backed clipboard format, writes CF_UNICODETEXT marked out of clipboard
  monitors, Win+V history and cloud clipboard, sends Ctrl+V, restores after 0.5 s unless the clipboard sequence
  number moved. If the clipboard stays busy (10 tries over 0.1 s) or the write fails, nothing is pasted and the
  dictation counts as not pasted.
- **Target**: foreground window plus its keyboard-focus window; app name from the exe's FileDescription;
  PLAID_BUNDLE is the exe file name (`msedge.exe`); window title for refine. UWP apps all report as
  `ApplicationFrameHost.exe` / "Application Frame Host".
- **Limits**:
  - nothing works while an elevated window has focus (UIPI blocks both the hook and SendInput);
  - a key-up missed while the secure desktop (UAC, Ctrl+Alt+Del) was up can leave a hold stuck until that key is
    pressed and released again;
  - Windows drops a low-level hook whose callback exceeds LowLevelHooksTimeout; the callback only recognizes and
    posts, but a stalled machine could still lose it (no reinstall yet);
  - the snapshot reads every format, which makes apps that render on demand (a large Excel or Office copy) produce
    them; that paste can take seconds.

First run on a Windows machine, in order:

1. key hold and tap, including in Notepad, a browser and Word; Start menu/menu bar must not open on release;
2. AltGr layout (e.g. German) with right Alt bound;
3. mic and resampler (44.1/48 kHz devices), `micDevice`;
4. paste into Notepad, a browser text area and a terminal; clipboard (text and an image) comes back. If nothing
   is written, suspect the NULL clipboard owner first (`OpenClipboard(NULL)` + `EmptyClipboard`);
5. ⏎/esc confirm for voice commands; a command that outlives its timeout;
6. Credential Manager key read, system proxy.

## Verified only by tests or replays

Not yet exercised in a live run of the GPUI app:

- key tap (its code also moved to hotkey/cgtap.rs when the Windows hook was added; the move was checked by diff
  only, so it needs a regression run);
- mic, including `micDevice` selection;
- paste;
- ⏎/esc interception for command confirm;
- EditWatch;
- window title;
- Keychain LLM key;
- clipboard restore on quit;
- GUI memory and idle CPU;
- settings window, HUD and onboarding on a real screen: window activation, hidden macOS traffic lights, focus
  rings, Windows window chrome, fonts and HUD transparency;
- 设置 shortcut recorder (needs the live key hook and accessibility);
- onboarding accessibility prompt and its try-it paste; mic permission state;
- history copy, audio playback, 重试 and 重新润色 round trips and the export save dialog; Finder reveal;
- 优化 LLM test against a real endpoint (only the error path is snapshotted) and 命令 trial run of a real command;
- 性能 memory figures on Windows; empty states with no records.
