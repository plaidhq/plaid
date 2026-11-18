// Run: cat Sources/Plaid/Phonetic.swift scripts/hotwords_check.swift | swift -
let E = Phonetic.Entry.init
let list = [
    E("Claude Code", ["cloud code", "高 Code"], []), E("agent", [], []), E("Claude", ["克劳德", "cloud"], ["weather", "天气"]), E("keep-alive", [], []),
    E("WebSocket", [], []), E("GitHub", [], []), E("傅平", [], ["扶贫", "脱贫"]), E("Plaid", [], []), E("prompt", [], []),
    E("token", [], []), E("端到端", [], []), E("李娜", [], []), E("刘洋", [], []), E("王芳", [], []), E("Ghostty", [], []), E("PR", [], []), E("CI", ["ci"], []), E("worktree", ["work tree"], []), E("e2e", ["e to e"], []),
]
let cases: [(String, String)] = [
    ("用 cloud code 写", "用 Claude Code 写"), ("问一下克劳德吧", "问一下Claude吧"), ("连接要 keep a life", "连接要 keep-alive"),
    ("so the web socket reconnects", "so the WebSocket reconnects"), ("push 到 github 上", "push 到 GitHub 上"),
    ("傅平说的", "傅平说的"), ("付平来了", "傅平来了"), ("扶贫办工作人员", "扶贫办工作人员"),
    ("the cloud weather is bad", "the cloud weather is bad"), ("改一下 prompts", "改一下 prompts"), ("这些 tokens 很贵", "这些 tokens 很贵"),
    ("用 Claude Code 写", "用 Claude Code 写"), ("我在 Claude 里", "我在 Claude 里"), ("打开 ghosty", "打开 Ghostty"),
    ("短到短的测试", "端到端的测试"), ("今天天气很好，我们去公园", "今天天气很好，我们去公园"), ("提个 pr", "提个 pr"),
    ("", ""), ("在 work tree 里", "在 worktree 里"), ("E to E 有覆盖", "e2e 有覆盖"), ("跑一下 ci", "跑一下 CI"), ("在你那种出示", "在你那种出示"), ("那里留言。", "那里留言。"), ("中新网房产", "中新网房产"), ("不如搞 code 的", "不如Claude Code 的"), ("别的Agent去", "别的Agent去"), ("a Plaid app", "a Plaid app"),
]
var bad = 0
for (i, want) in cases { let got = Phonetic.apply(i, list); if got != want { bad += 1; print("FAIL \(i) → \(got), want \(want)") } }
print(bad == 0 ? "ok \(cases.count)" : "\(bad) failed"); if bad > 0 { exit(1) }
