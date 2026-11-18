# Vocabulary: how others do it, and what Plaid does — 2026-11-08

## Survey (Nov 2026, vendor docs unless noted)

| Product | Mechanism | Limits | Guidance |
|---|---|---|---|
| AssemblyAI `keyterms_prompt` | term list fed to the model; `custom_spelling` = post-replace | streaming 100 terms; async up to 1000 | "Start with no keyterms"; too many or common terms cause overcorrection and hallucination |
| Deepgram `keyterm` (Nova-3) | unweighted terms, swappable mid-stream | 100 terms / 500 tokens | focus on the 20-50 most important terms |
| OpenAI | Whisper `prompt` (last 224 tokens); gpt-transcribe `keywords` | 224 tokens | prompting "not especially reliable" |
| Google | PhraseSet with boost 0-20 | 5000 phrases/request | higher boost = more false positives |
| Azure | phrase list, one global weight | ~200 recommended | longer list hurts quality and latency |
| Volcengine bigmodel | `context.hotwords` (no weights, goes into the prompt), console tables, replacement tables | streaming: hotwords + context share 100 tokens, only the first 100 are kept; second pass: 5000 words | 2-6 character terms; fewer is better; remove words it already gets right; use replacements when a hotword fails |

Dictation apps:
- Wispr Flow: auto-learns a word when the user re-spells it after a paste, skips common words, names, numbers, code; announces learned words in batches; entries can carry a literal whole-word correction.
- Typeless: corrections land in an "Auto-added" tab separate from "Manually-added"; bulk delete, no pruning.
- Superwhisper, MacWhisper, Aqua Voice: manual vocabulary + replacements; Superwhisper warns that more words confuse the model.
- 豆包输入法: "fix it once and it remembers" (press tests on names); no user-facing dictionary.
- 讯飞输入法: manual personal voice dictionary plus adaptive learning from edits.

Research: prompt-style biasing degrades as the list grows; per-utterance selection of the top terms recovers it.
LLM correction over-corrects toward frequent words unless it gets phonetic information.

## What Plaid adopted

| Takeaway | Status |
|---|---|
| Auto-learn from corrections, show what was learned, make it removable | Built: `Records.autoRules()` → `hotwords.auto.txt`, HUD notice "学会：…" on a new rule, list + 忽略 in Settings → 学习 (section 自动规则) |
| Hotword when the ASR is inconsistent, replacement when the same wrong form recurs | Built: only `heard -> term` replacements are learned; hotwords stay manual (100-token budget) |
| Prune by evidence | Partly: a rule is derived from records each time, so it disappears with its evidence; a manual edit back to the heard form vetoes it |
| Order inside the 100-token context | Fixed: `Hotwords.context()` now puts hotwords before dialog text (JSONSerialization had put `context_data` first). One long-context clip showed no difference in partials or final |
| Second pass with the larger hotword cap | Already on (`enable_nonstream`) |
| Per-utterance term selection | Not built: 21 terms fit the budget; revisit past ~50 |
| Whole-word replacement for CJK | Not built; learned heards must be 2+ CJK-containing characters |
| Phonetic candidates for the LLM | Not built; residual-error analysis showed it would only touch the Latin bucket |

## Learning rule
Sources: a refine rewrite (`llm == "ok"`, raw → text) and a manual edit (pasted → edited).
For every hotword term that occurs more often after than before, the before-span between the same neighbouring
characters is the heard form. Kept when the heard form is 2-12 characters, has no punctuation, isn't the term in
another case, contains CJK, and appears in at least 2 records.

All-ASCII heard forms are excluded: replacements are literal substring rewrites, and a dry run of refine over the
last 100 real Volc utterances proposed `Cloud -> Claude`, which would rewrite "iCloud" and "cloud".
The same dry run also proposed `然后一 two e -> e2e` (1 occurrence, below threshold), which is why new rules are
announced.

Terms come from the user's hotwords.txt only (`Hotwords.terms(auto: false)`), so a deleted term stops teaching itself.
With refine off, `autoLearn` (default on) runs the refine prompt in the background after each paste with a CJK raw, stores the result as `Record.checked`, and learns from it; the paste is unchanged. Endpoint: the refine settings if filled in, else DeepSeek (`deepseek-flash`) when its key is in the Keychain. Toggle in Settings → 学习.

## Sources
- AssemblyAI: https://www.assemblyai.com/docs/streaming/prompting-and-keyterms, https://www.assemblyai.com/docs/pre-recorded-audio/correct-spelling-of-terms
- Deepgram: https://developers.deepgram.com/docs/keyterm
- OpenAI: https://developers.openai.com/cookbook/examples/whisper_prompting_guide, https://developers.openai.com/api/docs/guides/transcription
- Google: https://cloud.google.com/speech-to-text/v2/docs/adaptation-model
- Azure: https://learn.microsoft.com/en-us/azure/ai-services/speech-service/improve-accuracy-phrase-list
- Volcengine: https://docs.volcengine.com/docs/doubao-voice/hot-words-and-context?lang=zh, https://docs.volcengine.com/docs/doubao-voice/bidirectional-streaming-automatic-speech-recognition-websocket
- Wispr Flow: https://docs.wisprflow.ai/articles/4052411709-teach-flow-your-words-with-the-dictionary, https://wisprflow.ai/post/technical-challenges
- Typeless: https://www.typeless.com/help/quickstart/history-and-dictionary
- Superwhisper: https://superwhisper.com/docs/get-started/interface-vocabulary
- MacWhisper: https://docs.macwhisper.com/article/37-find-and-replace-in-transcriptions
- Aqua Voice: https://aquavoice.com/guide/dictionary
- 豆包输入法 (third-party tests): https://blog.cold04.com/p/doubaoime_first_test/, https://www.tomyf.com/doubao-shurufa
- Papers: https://arxiv.org/html/2409.06062, https://arxiv.org/html/2505.17410, https://arxiv.org/html/2608.05759v1
