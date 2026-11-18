// Driver for scripts/golden.sh; appended after excerpts of the app sources.
import AppKit

let outDir = CommandLine.arguments[1]
func writer(_ name: String) -> (String, Any, Any) -> Void {
    let path = outDir + "/" + name + ".jsonl"
    FileManager.default.createFile(atPath: path, contents: nil)
    let h = FileHandle(forWritingAtPath: path)!
    return { fn, input, output in
        let d = try! JSONSerialization.data(withJSONObject: ["fn": fn, "in": input, "out": output], options: [.sortedKeys, .withoutEscapingSlashes])
        h.write(d + Data([0x0a]))
    }
}

// MARK: synthetic (committed)
let pub = writer("synthetic")

// Phonetic: the cases from scripts/hotwords_check.swift.
let E = Phonetic.Entry.init
let list = [
    E("Claude Code", ["cloud code", "高 Code"], []), E("agent", [], []), E("Claude", ["克劳德", "cloud"], ["weather", "天气"]), E("keep-alive", [], []),
    E("WebSocket", [], []), E("GitHub", [], []), E("傅平", [], ["扶贫", "脱贫"]), E("Plaid", [], []), E("prompt", [], []),
    E("token", [], []), E("端到端", [], []), E("李娜", [], []), E("刘洋", [], []), E("王芳", [], []), E("Ghostty", [], []), E("PR", [], []), E("CI", ["ci"], []), E("worktree", ["work tree"], []), E("e2e", ["e to e"], []),
]
let checkLines = list.map { e in ([e.target] + e.aliases).joined(separator: " | ") + (e.block.isEmpty ? "" : " ~~~ " + e.block.joined(separator: " | ")) }
let phoneticInputs = ["用 cloud code 写", "问一下克劳德吧", "连接要 keep a life", "so the web socket reconnects", "push 到 github 上",
    "傅平说的", "付平来了", "扶贫办工作人员", "the cloud weather is bad", "改一下 prompts", "这些 tokens 很贵", "用 Claude Code 写",
    "我在 Claude 里", "打开 ghosty", "短到短的测试", "今天天气很好，我们去公园", "提个 pr", "", "在 work tree 里", "E to E 有覆盖",
    "跑一下 ci", "在你那种出示", "那里留言。", "中新网房产", "不如搞 code 的", "别的Agent去", "a Plaid app", "绿色的女孩说了行"]
for i in phoneticInputs { pub("phonetic", ["lines": checkLines, "text": i], Phonetic.apply(i, list)) }
let snapLines = ["Neo | Nio", "wrong -> right", "-> ignored", "# comment is filtered by Hotwords.lines, not Snapshot", "Plaid ~~~ 格子"]
let snap = Hotwords.Snapshot(lines: snapLines, threshold: 0.85)
for t in ["I am nio", "this is WRONG and wrong", "Plaid 格子衫", "plaid", "", "嗨 Neo"] {
    pub("snapshot.apply", ["lines": snapLines, "threshold": 0.85, "text": t], snap.apply(t))
}
pub("snapshot.terms", ["lines": snapLines], snap.terms)
for d in [[], ["你好"], ["a\"b", "c\\d"]] as [[String]] {
    pub("snapshot.context", ["lines": snapLines, "dialog": d], snap.context(dialog: d) as Any? ?? NSNull())
    pub("snapshot.context", ["lines": [] as [String], "dialog": d], Hotwords.Snapshot(lines: [], threshold: 0.85).context(dialog: d) as Any? ?? NSNull())
}
for t in ["另外一方面是 有一些，其实 如果 e to e 测试 OK。 好", "中文 English 中文", "你好 ，世界", "ａ　ｂ", "全角 （括号） 测试"] { pub("tidy", t, Volc.tidy(t)) }
for t in ["你好world 123 abc", "Claude Code 很好用", "𠀀𪚥 emoji 😀 ok", "x86_64-apple", "", "ＡＢＣ１２３", "é è ü"] { pub("chars", t, UsageG.chars(t)) }

let diffPairs: [(String, String)] = [
    ("连接要 keep a life 吗", "连接要 Keeplife 吗"), ("我是工成", "我是工程师"), ("王思邈来了", "王思淼来了"), ("hello world", "hello world!"),
    ("今天天气好", "今天天气好，我们出去玩吧"), ("嗯那个我们走", "我们走"), ("用 cloud code 写代码", "用 Claude Code 写代码"),
    ("a", "b"), ("", "x"), ("推送到 get hub", "推送到 GitHub"), ("这个 PR 要合并", "这个 MR 要合并"), ("克劳德 Code 很好", "Claude Code 很好"),
]
for (a, b) in diffPairs {
    pub("replacements", [a, b], Diff.replacements(a, b).map { [$0.from, $0.to] })
    pub("overlap", [a, b], Diff.overlap(a, b))
    if let s = Diff.insertedSpan(a, b) { pub("insertedSpan", [a, b], [s.start, s.end, s.text] as [Any]) } else { pub("insertedSpan", [a, b], NSNull()) }
}
for (base, s, e, now) in [("ab你好cd", 2, 4, "ab您好呀cd"), ("ab你好cd", 2, 4, "ab  cd"), ("ab你好cd", 2, 4, "xb你好cd"), ("😀x", 2, 3, "😀yz")] as [(String, Int, Int, String)] {
    pub("editedSpan", [base, s, e, now] as [Any], Diff.editedSpan(base, s, e, now) as Any? ?? NSNull())
}
let terms = ["Claude Code", "Claude", "GitHub", "Plaid"]
for (a, b) in [("用克劳德 Code 写", "用Claude Code 写"), ("推送到吉特哈布上", "推送到GitHub上"), ("打开 cloud", "打开 Claude"), ("普莱德很好", "Plaid很好"), ("普莱德", "Plaid")] {
    pub("rules", ["before": a, "after": b, "terms": terms], RecordsG.rules(a, b, terms).map { [$0.from, $0.to] })
}
let acts = [Action(name: "搜索", aliases: ["搜一下"], run: "x"), Action(name: "搜索文件", run: "x"), Action(name: "Git Push", run: "x"), Action(name: "空", run: "  ")]
func parsed(_ p: ActionsG.Parsed) -> Any {
    switch p {
    case .dictation: return ["dictation"]
    case .unknown(let s): return ["unknown", s]
    case .run(let a, let args): return ["run", a.name, args]
    }
}
let actNames = acts.map { ["name": $0.name, "aliases": $0.aliases, "run": $0.run] as [String: Any] }
for (t, w) in [("Plaid 搜索 天气", "Plaid"), ("嗯，plaid。搜索文件 报告。", "Plaid"), ("Plaids 搜索", "Plaid"), ("Plaid 打开", "Plaid"), ("你好", "Plaid"),
               ("搜一下 Rust", ""), ("git push", ""), ("ＰＬＡＩＤ　Git Push -f", "Plaid"), ("Plaid", "Plaid"), ("嘿 Plaid 搜索", "嘿 Plaid"), ("嘿，Plaid。搜索 x", "嘿 Plaid")] {
    pub("actions.parse", ["text": t, "wake": w, "actions": actNames] as [String: Any], parsed(ActionsG.parse(t, acts, wake: w)))
}
func hex(_ d: Data) -> String { d.map { String(format: "%02x", $0) }.joined() }
pub("volc.frame", ["type": 1, "last": false, "json": true, "payload": hex(Data("{}".utf8))], hex(Volc.frame(type: 1, json: true, payload: Data("{}".utf8))))
pub("volc.frame", ["type": 2, "last": true, "json": false, "payload": "010203"], hex(Volc.frame(type: 2, last: true, json: false, payload: Data([1, 2, 3]))))
func resp(_ r: Volc.Response?) -> Any { r.map { ["text": $0.text as Any? ?? NSNull(), "last": $0.last, "error": $0.error as Any? ?? NSNull()] as [String: Any] } ?? NSNull() }
let json = Data(#"{"result":{"text":"你好"}}"#.utf8)
var full = Data([0x11, 0x93, 0x10, 0x00, 0, 0, 0, 7]); withUnsafeBytes(of: UInt32(json.count).bigEndian) { full.append(contentsOf: $0) }; full += json
var errF = Data([0x11, 0xF0, 0x10, 0x00]); withUnsafeBytes(of: UInt32(45000001).bigEndian) { errF.append(contentsOf: $0) }
withUnsafeBytes(of: UInt32(3).bigEndian) { errF.append(contentsOf: $0) }; errF += Data("bad".utf8)
for f in [full, errF, Data([0x11, 0x90, 0x10, 0x00, 0, 0, 0, 0]), Data([0x11, 0x92, 0x10, 0x00, 0, 0, 0, 0]), Data([0x11, 0x90, 0x10, 0x00, 0, 0, 0, 2, 0x1f, 0x8b]), Data([0x11, 0x20]), Data([0x11, 0x90, 0x10, 0x00, 0, 0, 0, 9, 1])] {
    pub("volc.parse", hex(f), resp(Volc.parse(f)))
}

// MARK: private (from the user's records; gitignored)
let priv = writer("records.private")
let recText = (try? String(contentsOfFile: CommandLine.arguments[2], encoding: .utf8)) ?? ""
let recs = recText.split(separator: "\n").compactMap { try? JSONSerialization.jsonObject(with: Data($0.utf8)) as? [String: Any] }
let userLines = ((try? String(contentsOfFile: CommandLine.arguments[3], encoding: .utf8)) ?? "").split(separator: "\n")
    .map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty && !$0.hasPrefix("#") }
let userSnap = Hotwords.Snapshot(lines: userLines + checkLines, threshold: 0.85)
let userTerms = userSnap.terms
for r in recs {
    let raw = r["raw"] as? String ?? "", text = r["text"] as? String ?? ""
    priv("tidy", raw, Volc.tidy(raw))
    priv("chars", text, UsageG.chars(text))
    priv("snapshot.apply", ["lines": userLines + checkLines, "threshold": 0.85, "text": raw], userSnap.apply(raw))
    var pairs: [(String, String)] = []
    if raw != text { pairs.append((raw, text)) }
    if let c = r["checked"] as? String, c != raw { pairs.append((raw, c)) }
    if let e = r["edited"] as? String { pairs.append(((r["pasted"] as? String) ?? text, e)) }
    for (a, b) in pairs {
        priv("replacements", [a, b], Diff.replacements(a, b).map { [$0.from, $0.to] })
        priv("rules", ["before": a, "after": b, "terms": userTerms], RecordsG.rules(a, b, userTerms).map { [$0.from, $0.to] })
    }
}
print("golden: \(recs.count) records, \(userLines.count) hotword lines -> \(outDir)")
