// Runs the saved recordings (records.jsonl + audio/) through `Plaid --file` and scores them.
// usage: swift scripts/eval.swift [out.json]      env is passed through: PLAID_FEED_SPEED, EVAL_LIMIT=n
// EVAL_MANIFEST=x.tsv scores a public test set instead: one `id<TAB>wav<TAB>reference` per line (16 kHz mono s16le WAV).
// EVAL_RESCORE=old.json re-scores a previous run's hypotheses (e.g. after a normalizer change) without running the engine.
// Reference per clip: `edited` if the user corrected the paste, else `raw` (the live ASR text, accepted unedited).
// CER after stripping punctuation/whitespace and case-folding; "itn" also maps Chinese numerals to digits on both sides,
// so 三百 vs 300 shows up as an ITN-only difference instead of an error.
import Foundation

let support = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/Application Support/Plaid")
let bin = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent().appendingPathComponent("../build/Plaid.app/Contents/MacOS/Plaid").standardized.path
let env = ProcessInfo.processInfo.environment

struct Rec: Decodable { let audio: String?; let raw: String; let edited: String?; let ctx: [String]?; let audioMs: Int }
struct Row: Codable { let audio: String; let sec: Double; let ref: String; let hyp: String; let cer: Double; let cerItn: Double
    let refLen: Int; let edits: Int; let editsItn: Int; let volcEditsItn: Int; let finalMs: Int; let firstPartial: Double?; let ticks: [Int]
    var sub = 0, del = 0, ins = 0  // ITN-normalized
    var attempts = 1 }

/// Drops corpus markup ([+], [LAUGHTER], <noise>) and reads 百分之N as N, since "%" is stripped as a symbol.
func norm(_ s: String) -> [Character] {
    Array(s.replacingOccurrences(of: "\\[[^\\]]*\\]|<[^>]*>", with: "", options: .regularExpression)
        .replacingOccurrences(of: "百\\s*分\\s*之", with: "", options: .regularExpression)
        .precomposedStringWithCompatibilityMapping.lowercased().unicodeScalars
        .filter { !CharacterSet.punctuationCharacters.union(.symbols).union(.whitespacesAndNewlines).contains($0) }
        .map(Character.init))
}

let digits: [Character: Int] = ["零": 0, "〇": 0, "一": 1, "二": 2, "两": 2, "三": 3, "四": 4, "五": 5, "六": 6, "七": 7, "八": 8, "九": 9, "幺": 1]
/// 万/亿 stay literal so 五千亿 and 5000亿 both read 5000亿 (hypotheses mix Arabic digits with these units).
let units: [Character: Int] = ["十": 10, "百": 100, "千": 1000]

func cnInt(_ s: Substring) -> String {
    if !s.contains(where: { units[$0] != nil }) { return String(s.compactMap { digits[$0].map(String.init) }.joined()) }
    var section = 0, num = 0
    for c in s {
        if let d = digits[c] { num = d }
        else if let u = units[c] { section += (num == 0 && u == 10 ? 1 : num) * u; num = 0 }
    }
    return String(section + num)
}

/// Runs of Chinese numerals → Arabic, e.g. 三百 → 300, 二点五 → 2.5, 一下 → 1下 (consistent on both sides).
func itn(_ s: [Character]) -> [Character] {
    var out: [Character] = [], run = ""
    func flush() {
        guard !run.isEmpty else { return }
        let p = run.split(separator: "点", maxSplits: 1, omittingEmptySubsequences: false)
        var v = p[0].isEmpty ? "" : cnInt(p[0])
        if p.count > 1 { v += "." + cnInt(p[1]) }  // ponytail: fraction digit-by-digit only
        out += Array(v)
        run = ""
    }
    for c in s {
        if digits[c] != nil || units[c] != nil || (c == "点" && !run.isEmpty) { run.append(c) } else { flush(); out.append(c) }
    }
    flush()
    return out
}

/// Edit distance from reference `a` to hypothesis `b`, split into substitutions, deletions, insertions.
func align(_ a: [Character], _ b: [Character]) -> (s: Int, d: Int, i: Int) {
    var m = [[Int]](repeating: [Int](repeating: 0, count: b.count + 1), count: a.count + 1)
    for i in 0...a.count { m[i][0] = i }
    for j in 0...b.count { m[0][j] = j }
    for i in stride(from: 1, through: a.count, by: 1) {
        for j in stride(from: 1, through: b.count, by: 1) { m[i][j] = min(m[i - 1][j] + 1, m[i][j - 1] + 1, m[i - 1][j - 1] + (a[i - 1] == b[j - 1] ? 0 : 1)) }
    }
    var i = a.count, j = b.count, s = 0, d = 0, n = 0
    while i > 0 || j > 0 {
        if i > 0, j > 0, m[i][j] == m[i - 1][j - 1] + (a[i - 1] == b[j - 1] ? 0 : 1) { if a[i - 1] != b[j - 1] { s += 1 }; i -= 1; j -= 1 }
        else if i > 0, m[i][j] == m[i - 1][j] + 1 { d += 1; i -= 1 }
        else { n += 1; j -= 1 }
    }
    return (s, d, n)
}

func lev(_ a: [Character], _ b: [Character]) -> Int { let o = align(a, b); return o.s + o.d + o.i }

func run(_ wav: String, ctx: [String]?) -> (hyp: String, finalMs: Int, first: Double?, ticks: [Int]) {
    let p = Process()
    p.executableURL = URL(fileURLWithPath: bin)
    p.arguments = ["--file", wav] + (ctx.flatMap { c in (try? JSONEncoder().encode(c)).map { ["--ctx", String(decoding: $0, as: UTF8.self)] } } ?? [])
    var e = env
    e["PLAID_LLM_ENABLED"] = "0"
    p.environment = e
    let pipe = Pipe()
    p.standardOutput = pipe
    p.standardError = FileHandle.nullDevice
    try! p.run()
    let out = String(decoding: pipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
    p.waitUntilExit()
    var hyp = "", ms = -1, first: Double?, ticks: [Int] = []
    for line in out.split(separator: "\n") {
        if line.hasPrefix("final ("), let r = line.range(of: " ms after release): ") {
            ms = Int(line[line.index(line.startIndex, offsetBy: 7)..<r.lowerBound]) ?? -1
            hyp = String(line[r.upperBound...])
        } else if line.hasPrefix("ticks ms: ") {
            ticks = line.dropFirst(10).trimmingCharacters(in: CharacterSet(charactersIn: "[]")).split(separator: ",").compactMap { Int($0.trimmingCharacters(in: .whitespaces)) }
        } else if first == nil, let s = line.split(separator: "s", maxSplits: 1).first, let t = Double(s.trimmingCharacters(in: .whitespaces)) {
            first = t
        }
    }
    return (hyp, ms, first, ticks)
}

/// One clip to score: `raw` is the live Volc text (records mode) or nil (manifest mode).
struct Clip { let id: String; let wav: String; let sec: Double; let ref: String; let raw: String?; let ctx: [String]? }

let manifest = env["EVAL_MANIFEST"]
var recs: [Rec] = []
var clips: [Clip]
if let manifest {
    clips = try! String(contentsOfFile: manifest, encoding: .utf8).split(separator: "\n").compactMap { line in
        let f = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
        guard f.count >= 3 else { return nil }
        let bytes = (try? FileManager.default.attributesOfItem(atPath: f[1])[.size] as? Int) ?? 44
        return Clip(id: f[0], wav: f[1], sec: Double(bytes - 44) / 32000, ref: f[2], raw: nil, ctx: nil)
    }
} else {
    let dec = JSONDecoder()
    let lines = try! String(contentsOf: support.appendingPathComponent("records.jsonl"), encoding: .utf8).split(separator: "\n")
    recs = lines.compactMap { try? dec.decode(Rec.self, from: Data($0.utf8)) }
        .filter { $0.audio.map { FileManager.default.fileExists(atPath: support.appendingPathComponent("audio/" + $0).path) } ?? false }
    clips = recs.map { Clip(id: $0.audio!, wav: support.appendingPathComponent("audio/" + $0.audio!).path, sec: Double($0.audioMs) / 1000,
                            ref: $0.edited ?? $0.raw, raw: $0.raw, ctx: $0.ctx) }
}
if let n = env["EVAL_LIMIT"].flatMap(Int.init) { clips = Array(clips.prefix(n)) }

let previous = env["EVAL_RESCORE"].map { f in Dictionary(try! JSONDecoder().decode([Row].self, from: Data(contentsOf: URL(fileURLWithPath: f))).map { ($0.audio, $0) }) { a, _ in a } }
var rows: [Row] = []
for c in clips {
    let ref = c.ref
    // A failed run (network, server) has no final line; retry rather than score it as 100% deletions.
    let old = previous?[c.id]
    var o = old.map { (hyp: $0.hyp, finalMs: $0.finalMs, first: $0.firstPartial, ticks: $0.ticks) } ?? run(c.wav, ctx: c.ctx), attempts = old?.attempts ?? 1
    while o.finalMs < 0, attempts < 3, previous == nil { o = run(c.wav, ctx: c.ctx); attempts += 1 }
    let a = norm(ref), b = norm(o.hyp)
    let e = lev(a, b), sdi = align(itn(a), itn(b)), ei = sdi.s + sdi.d + sdi.i
    var row = Row(audio: c.id, sec: c.sec, ref: ref, hyp: o.hyp, cer: Double(e) / Double(max(a.count, 1)),
                  cerItn: Double(ei) / Double(max(itn(a).count, 1)), refLen: a.count, edits: e, editsItn: ei,
                  volcEditsItn: c.raw.map { lev(itn(a), itn(norm($0))) } ?? -1, finalMs: o.finalMs, firstPartial: o.first, ticks: o.ticks)
    (row.sub, row.del, row.ins, row.attempts) = (sdi.s, sdi.d, sdi.i, attempts)
    rows.append(row)
    print(String(format: "%@  %5.1fs  cer %5.1f%%  itn %5.1f%%  final %4d ms", row.audio, row.sec, row.cer * 100, row.cerItn * 100, row.finalMs))
}

func pct(_ v: [Int], _ p: Double) -> Int { let s = v.sorted(); return s.isEmpty ? -1 : s[Int((Double(s.count - 1) * p).rounded())] }
let tot = rows.map(\.refLen).reduce(0, +)
print("\n== diffs (ITN-normalized CER > 0), worst first")
for r in rows.filter({ $0.editsItn > 0 }).sorted(by: { $0.cerItn > $1.cerItn }) {
    print(String(format: "-- %@ %.1fs cer %.1f%%\n   ref: %@\n   hyp: %@", r.audio, r.sec, r.cerItn * 100, r.ref, r.hyp))
}
// The recorded `raw` only differs from the reference where the user edited the paste.
if manifest == nil { print("\n== edited clips: replay vs recorded raw (ITN-normalized edits)") }
for r in rows where manifest == nil && r.ref != (recs.first { $0.audio == r.audio }?.raw ?? r.ref) {
    print("   \(r.audio)  replay \(r.editsItn)  recorded \(r.volcEditsItn)\(r.editsItn < r.volcEditsItn ? "  <- replay better" : "")")
}
let itnOnly = rows.filter { $0.edits > 0 && $0.editsItn == 0 }
print("\n== ITN-only clips: \(itnOnly.count)")
for r in itnOnly { print("   ref: \(r.ref)\n   hyp: \(r.hyp)") }
let ok = rows.map(\.finalMs).filter { $0 >= 0 }, short = rows.filter { $0.sec <= 15 && $0.finalMs >= 0 }.map(\.finalMs)
let itnTot = rows.map { itn(norm($0.ref)).count }.reduce(0, +)
print(String(format: "\n== S/D/I (ITN-normalized) %d/%d/%d of %d chars   failed runs %d   audio sent %.1f min (incl. retries)",
             rows.map(\.sub).reduce(0, +), rows.map(\.del).reduce(0, +), rows.map(\.ins).reduce(0, +), itnTot,
             rows.filter { $0.finalMs < 0 }.count, rows.map { $0.sec * Double($0.attempts) }.reduce(0, +) / 60))
print(String(format: """

== %d clips, %d ref chars
Volc (raw) CER micro %.2f%% ITN-normalized
CER micro %.2f%%  (ITN-normalized %.2f%%)   macro %.2f%% (%.2f%%)
release→final ms p50 %d p90 %d max %d   (≤15 s clips: p50 %d p90 %d)
first partial s p50 %.2f   tick ms p50 %d p90 %d max %d
""", rows.count, tot,
    manifest == nil ? Double(rows.map(\.volcEditsItn).reduce(0, +)) / Double(tot) * 100 : .nan,
    Double(rows.map(\.edits).reduce(0, +)) / Double(tot) * 100, Double(rows.map(\.editsItn).reduce(0, +)) / Double(itnTot) * 100,
    rows.map(\.cer).reduce(0, +) / Double(rows.count) * 100, rows.map(\.cerItn).reduce(0, +) / Double(rows.count) * 100,
    pct(ok, 0.5), pct(ok, 0.9), rows.map(\.finalMs).max() ?? -1, pct(short, 0.5), pct(short, 0.9),
    Double(pct(rows.compactMap { $0.firstPartial.map { Int($0 * 1000) } }, 0.5)) / 1000,
    pct(rows.flatMap(\.ticks), 0.5), pct(rows.flatMap(\.ticks), 0.9), rows.flatMap(\.ticks).max() ?? -1))
if CommandLine.arguments.count > 1 {
    let enc = JSONEncoder()
    enc.outputFormatting = [.prettyPrinted, .withoutEscapingSlashes]
    try! enc.encode(rows).write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
}
