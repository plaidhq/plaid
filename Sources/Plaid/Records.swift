import AVFoundation

/// One pasted utterance. Latencies in ms; "release" is key-up, or the ⌥/⏎ that ends a toggled dictation.
struct Record: Codable, Identifiable {
    var at: Date  // key-down
    var app: String?
    var audioMs: Int
    var firstTextMs: Int?  // key-down → first partial in the HUD
    var asrMs: Int  // release → ASR final
    var llmMs: Int?
    var llm: String  // off | ok | timeout | skipped (esc) | error message
    var totalMs: Int  // release → paste
    var raw: String
    var text: String
    var audio: String?  // file name under Records.audioDir
    var edited: String?  // the pasted span after the user's manual correction
    var pasted: String?  // the span as the target app showed it right after the paste (baseline for `edited`)
    var noiseRMS: Int?  // 10th / 90th percentile of the 100 ms buffer RMS: noise floor and speech level
    var speechRMS: Int?
    var ctx: [String]?  // ASR dialog_ctx sent with this utterance, newest first (replay: Plaid --file a.wav --ctx '<json>')
    var engine: String?  // "local" on records from the removed on-device engine, nil = Volcengine
    var vp: Bool?  // capture used voice processing (removed 2026-11-13); nil before 2026-11-08 and after removal
    var checked: String?  // background LLM rewrite of raw for learning only (autoLearn), when refine didn't run
    var mic: String?  // input device name; nil in records before 2026-11-09
    var outcome: String?  // nil = pasted, failed = no result, saved = text available without automatic paste
    var error: String?
    var triggerMs: Int?  // physical press → capture start
    var micStartMs: Int?  // capture start → engine.start success
    var isFailure: Bool { outcome == "failed" }
    var id: Date { at }
}

/// One local calendar day of dictation, kept in usage.json so totals survive records.jsonl dropping old lines.
struct Usage: Codable {
    var n = 0, ms = 0, chars = 0
    var apps: [String: Int] = [:]  // chars per app

    /// Characters as a reader counts them in mixed text: one per CJK ideograph, one per run of letters/digits.
    static func chars(_ s: String) -> Int {
        var n = 0, inWord = false
        for u in s.unicodeScalars {
            if u.properties.isIdeographic {
                n += 1
                inWord = false
            } else if CharacterSet.alphanumerics.contains(u) {
                if !inWord { n += 1 }
                inWord = true
            } else {
                inWord = false
            }
        }
        return n
    }

    mutating func add(_ r: Record) {
        guard !r.isFailure else { return }
        let c = Self.chars(r.text)
        n += 1
        ms += r.audioMs
        chars += c
        apps[r.app ?? "未知应用", default: 0] += c
    }

    static func + (a: Usage, b: Usage) -> Usage {
        Usage(n: a.n + b.n, ms: a.ms + b.ms, chars: a.chars + b.chars, apps: a.apps.merging(b.apps, uniquingKeysWith: +))
    }
}

func pct(_ v: [Int], _ p: Double) -> Int? {
    let s = v.sorted()
    return s.isEmpty ? nil : s[Int((Double(s.count - 1) * p).rounded())]
}

/// Append-only log at ~/Library/Application Support/Plaid/records.jsonl. Main thread only.
final class Records: ObservableObject {
    static let shared = Records()
    static let url = Hotwords.url.deletingLastPathComponent().appendingPathComponent("records.jsonl")
    static let usageURL = url.deletingLastPathComponent().appendingPathComponent("usage.json")
    @Published private(set) var all: [Record] = []
    @Published private(set) var usage: [String: Usage] = [:]  // keyed by Records.day
    static let limit = 1000
    fileprivate static let io = DispatchQueue(label: "plaid.records.io", qos: .utility)
    private var autoBody: String?

    private init() {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        let lines = ((try? String(contentsOf: Self.url, encoding: .utf8)) ?? "").split(separator: "\n")
        let parsed = lines.compactMap { try? decoder.decode(Record.self, from: Data($0.utf8)) }
        all = Array(parsed.suffix(Self.limit))
        if let d = try? Data(contentsOf: Self.usageURL), let u = try? JSONDecoder().decode([String: Usage].self, from: d) {
            usage = u
        } else {
            // Backfill from the complete legacy log before applying retention. Existing totals are never rebuilt from the retained tail.
            for r in parsed { usage[Self.day(r.at), default: Usage()].add(r) }
            saveUsage()
        }
        if parsed.count > Self.limit { persist() }
        writeAutoRules()
    }

    /// Call on termination or before a test reads persisted files. Never call from the persistence queue.
    static func flush() { io.sync {} }

    static func saveHotwords(_ text: String, completion: @escaping (Result<Void, Error>) -> Void) {
        io.async {
            let result = Result {
                try FileManager.default.createDirectory(at: Hotwords.url.deletingLastPathComponent(), withIntermediateDirectories: true)
                try text.write(to: Hotwords.url, atomically: true, encoding: .utf8)
            }
            DispatchQueue.main.async { completion(result) }
        }
    }

    private static func encoded(_ records: [Record]) -> Data {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        return records.reduce(into: Data()) { data, r in
            if let line = try? encoder.encode(r) { data += line; data.append(0x0a) }
        }
    }

    private func persist() {
        let snapshot = Array(all.suffix(Self.limit))
        Self.io.async {
            try? FileManager.default.createDirectory(at: Self.url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try? Self.encoded(snapshot).write(to: Self.url, options: .atomic)
        }
    }

    private static let dayFormat = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f
    }()

    /// Local calendar day key for `usage`.
    static func day(_ d: Date) -> String { dayFormat.string(from: d) }

    private func saveUsage() {
        let snapshot = usage
        Self.io.async {
            try? FileManager.default.createDirectory(at: Self.usageURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            try? JSONEncoder().encode(snapshot).write(to: Self.usageURL, options: .atomic)
        }
    }

    /// Returns auto rules this record taught (see writeAutoRules).
    @discardableResult
    func add(_ r: Record) -> [String] {
        let trimmed = all.count >= Self.limit
        all = Array((all + [r]).suffix(Self.limit))
        fixCache = nil
        if !r.isFailure {
            usage[Self.day(r.at), default: Usage()].add(r)
            saveUsage()
        }
        if trimmed {
            persist()
        } else {
            Self.io.async {
                let line = Self.encoded([r])
                if let h = try? FileHandle(forWritingTo: Self.url) {
                    defer { try? h.close() }
                    _ = try? h.seekToEnd()
                    try? h.write(contentsOf: line)
                } else {
                    try? FileManager.default.createDirectory(at: Self.url.deletingLastPathComponent(), withIntermediateDirectories: true)
                    try? line.write(to: Self.url, options: .atomic)
                }
            }
        }
        return writeAutoRules()
    }

    func setEdited(_ id: Date, pasted: String, edited: String) {
        update(id) { $0.pasted = pasted; $0.edited = edited }
    }

    /// Returns auto rules this taught (see writeAutoRules).
    @discardableResult
    func setChecked(_ id: Date, _ text: String) -> [String] {
        update(id) { $0.checked = text }
    }

    @discardableResult
    private func update(_ id: Date, _ change: (inout Record) -> Void) -> [String] {
        guard let i = all.lastIndex(where: { $0.id == id }) else { return [] }
        change(&all[i])
        all = Array(all.suffix(Self.limit))
        fixCache = nil
        persist()
        return writeAutoRules()
    }

    /// Updates delivery after the result was durably enqueued; only pasted text becomes recent context.
    func setDelivery(_ id: Date, pasted: Bool, error: String?, totalMs: Int? = nil) {
        update(id) {
            $0.outcome = pasted ? nil : "saved"
            $0.error = error
            if let totalMs { $0.totalMs = totalMs }
        }
    }

    /// Saves a retry result in place, without pasting or applying learning/refinement.
    func recovered(_ id: Date, text: String) {
        guard let r = all.last(where: { $0.id == id }) else { return }
        update(id) { $0.raw = text; $0.text = text; $0.outcome = "saved"; $0.error = nil }
        if r.isFailure, let saved = all.last(where: { $0.id == id }) {
            usage[Self.day(saved.at), default: Usage()].add(saved)
            saveUsage()
        }
    }

    /// Deletes records, audio and derived rules; usage.json (counts only, no text) is kept.
    func clear() {
        all = []
        fixCache = nil
        autoCache = nil
        Self.io.async {
            try? FileManager.default.removeItem(at: Self.url)
            try? FileManager.default.removeItem(at: Self.audioDir)
        }
        writeAutoRules()
    }

    /// Substitutions learned from manual edits, most frequent first.
    private var fixCache: [(from: String, to: String, count: Int, last: Date, context: String)]?

    func fixes() -> [(from: String, to: String, count: Int, last: Date, context: String)] {
        if let fixCache { return fixCache }
        var seen: [String: (from: String, to: String, count: Int, last: Date, context: String)] = [:]
        for r in all where !r.isFailure && r.outcome != "saved" {
            guard let e = r.edited else { continue }
            for f in Diff.replacements(r.pasted ?? r.text, e) {
                let k = f.from + "\u{0}" + f.to
                seen[k, default: (f.from, f.to, 0, r.at, e)].count += 1
                seen[k]!.last = r.at
                seen[k]!.context = e
            }
        }
        fixCache = seen.values.sorted { ($0.count, $0.last) > ($1.count, $1.last) }
        return fixCache!
    }

    /// `heard -> wanted` rules learned without the user typing them: a refine (or background autoLearn) rewrite or a manual edit that turned some
    /// span into a known hotword term, seen at least twice. Derived from records each time, so a rule disappears with its
    /// evidence; "忽略" in the Learned tab (dismissedFixes) and a manual edit reverting the term veto it.
    private var autoCache: [(from: String, to: String, count: Int)]?

    func autoRules() -> [(from: String, to: String, count: Int)] {
        if let autoCache { return autoCache }
        let terms = Hotwords.terms(auto: false).filter { !$0.isEmpty }
        let dismissed = Set((UserDefaults.standard.string(forKey: "dismissedFixes") ?? "").split(separator: "\n").map(String.init))
        var count: [String: Int] = [:], vetoed = Set<String>()
        for r in all where !r.isFailure && r.outcome != "saved" {
            var found: [(from: String, to: String)] = []
            if r.llm == "ok" { found += Self.rules(r.raw, r.text, terms) } else if let c = r.checked { found += Self.rules(r.raw, c, terms) }
            if let e = r.edited {
                found += Self.rules(r.pasted ?? r.text, e, terms)
                for v in Self.rules(e, r.pasted ?? r.text, terms) { vetoed.insert(v.from + " -> " + v.to) }  // edited a term back
            }
            for f in Set(found.map { $0.from + " -> " + $0.to }) { count[f, default: 0] += 1 }
        }
        autoCache = count.filter { $0.value >= 2 && !vetoed.contains($0.key) && !dismissed.contains($0.key) }
            .map { k, v in let p = k.components(separatedBy: " -> "); return (p[0], p[1], v) }
            .sorted { ($0.count, $0.from) > ($1.count, $1.from) }
        return autoCache!
    }

    /// For each hotword term that occurs more often in `after` than in `before`, the span of `before` that sits between
    /// the same neighbouring characters: `heard -> term`. The heard side must be 2-12 characters, free of punctuation,
    /// not already the term in another case, and contain CJK.
    /// ponytail: all-ASCII heards are skipped because rewrites are literal substring replacements and refine on 99 real
    /// utterances proposed "Cloud -> Claude" (would hit "iCloud", "cloud"); route them through Phonetic aliases if needed. Context-based rather than Diff.replacements, whose LCS alignment can
    /// split "克劳德 Code -> Claude Code" into a deletion plus an insertion.
    static func rules(_ before: String, _ after: String, _ terms: [String]) -> [(from: String, to: String)] {
        let punct = CharacterSet.punctuationCharacters.union(.symbols).union(.newlines)
        var out: [(from: String, to: String)] = []
        for t in terms where after.components(separatedBy: t).count > before.components(separatedBy: t).count {
            var from = after.startIndex
            while let r = after.range(of: t, range: from..<after.endIndex) {
                from = r.upperBound
                let left = String(after[..<r.lowerBound].trimmingCharacters(in: .whitespaces).suffix(2))
                let right = String(after[r.upperBound...].trimmingCharacters(in: .whitespaces).prefix(2))
                var starts = left.isEmpty ? [before.startIndex] : []
                var i = before.startIndex
                while !left.isEmpty, let l = before.range(of: left, range: i..<before.endIndex) { starts.append(l.upperBound); i = l.upperBound }
                let spans: [String] = starts.compactMap { s in
                    let end = right.isEmpty ? before.endIndex : before.range(of: right, range: s..<before.endIndex)?.lowerBound
                    return end.map { String(before[s..<$0]).trimmingCharacters(in: .whitespaces) }
                }
                guard let heard = spans.min(by: { $0.count < $1.count }), (2...12).contains(heard.count),
                      heard.unicodeScalars.allSatisfy({ !punct.contains($0) }), !heard.localizedCaseInsensitiveContains(t),
                      heard.contains(where: { !$0.isASCII }) else { continue }
                out.append((heard, t))
            }
        }
        return out
    }

    /// Recomputes autoRules() into hotwords.auto.txt, which Hotwords reads with the user's own file.
    /// Returns rules that weren't in the file before, so the caller can show them once.
    @discardableResult
    func writeAutoRules() -> [String] {
        autoCache = nil
        let lines = autoRules().map { "\($0.from) -> \($0.to)" }
        let body = "# Learned by Plaid from refine rewrites and manual edits; regenerated, do not edit. Remove a rule via 忽略.\n"
            + lines.map { $0 + "\n" }.joined()
        let old = autoBody ?? ((try? String(contentsOf: Hotwords.autoURL, encoding: .utf8)) ?? "")
        autoBody = body
        if old != body {
            Self.io.async {
                try? FileManager.default.createDirectory(at: Hotwords.autoURL.deletingLastPathComponent(), withIntermediateDirectories: true)
                try? body.write(to: Hotwords.autoURL, atomically: true, encoding: .utf8)
            }
        }
        let before = Set(old.split(separator: "\n").map(String.init))
        return lines.filter { !before.contains($0) }
    }

    // MARK: audio

    static let audioDir = url.deletingLastPathComponent().appendingPathComponent("audio")
    static var keepAudio: Bool { Config.value("keepAudio") != "0" }

    /// Saves 16 kHz mono s16le as WAV, keeping the newest 500. Returns the file name.
    static func saveAudio(_ pcm: Data, at: Date) -> String? {
        guard keepAudio, !pcm.isEmpty else { return nil }
        let f = DateFormatter()
        f.dateFormat = "yyyyMMdd-HHmmss.SSS"
        let name = f.string(from: at) + ".wav"
        io.async {
            let fm = FileManager.default
            try? fm.createDirectory(at: audioDir, withIntermediateDirectories: true)
            guard (try? (wavHeader(pcm.count) + pcm).write(to: audioDir.appendingPathComponent(name), options: .atomic)) != nil else { return }
            let files = ((try? fm.contentsOfDirectory(atPath: audioDir.path)) ?? []).filter { $0.hasSuffix(".wav") }.sorted()
            for old in files.dropLast(500) { try? fm.removeItem(at: audioDir.appendingPathComponent(old)) }
        }
        return name
    }

    static func wavHeader(_ n: Int) -> Data {
        var d = Data("RIFF".utf8)
        d.append(le: UInt32(36 + n))
        d += Data("WAVEfmt ".utf8)
        d.append(le: UInt32(16)); d.append(le: UInt16(1)); d.append(le: UInt16(1))
        d.append(le: UInt32(16000)); d.append(le: UInt32(32000)); d.append(le: UInt16(2)); d.append(le: UInt16(16))
        d += Data("data".utf8)
        d.append(le: UInt32(n))
        return d
    }

    /// Outputs from the last 10 minutes, oldest first: context for the LLM pass.
    func recent(_ n: Int) -> [String] {
        all.filter { $0.outcome == nil && $0.at.timeIntervalSinceNow > -600 }.suffix(n).map(\.text)
    }
}

// MARK: export

extension Records {
    /// Capture settings on the main thread before starting background export.
    static func exportInfo() -> [String: String] {
        var info: [String: String] = [
            "version": Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev",
            "os": ProcessInfo.processInfo.operatingSystemVersionString,
        ]
        info["mic"] = Mic.current?.name
        for k in ["micDevice", "nonstream", "silenceRMS", "tailMs", "codec", "asrContext", "learnEdits", "autoLearn",
                  "llmEnabled", "llmFormat", "llmBaseUrl", "llmModel", "llmTimeoutMs"] {
            info[k] = Config.value(k) ?? "default"
        }
        return info
    }

    /// Runs on the ordered I/O queue, after pending audio/record writes and before subsequent deletion.
    static func export(_ rs: [Record], to zip: URL, info: [String: String], completion: @escaping (Result<Void, Error>) -> Void) {
        io.async {
            let result = Result { try exportBundle(rs, to: zip, info: info) }
            DispatchQueue.main.async { completion(result) }
        }
    }

    private static func exportBundle(_ rs: [Record], to zip: URL, info: [String: String]) throws {
        let fm = FileManager.default
        let dir = fm.temporaryDirectory.appendingPathComponent("plaid-export-\(UUID().uuidString)")
        let root = dir.appendingPathComponent(zip.deletingPathExtension().lastPathComponent)
        defer { try? fm.removeItem(at: dir) }
        try fm.createDirectory(at: root.appendingPathComponent("audio"), withIntermediateDirectories: true)
        let enc = JSONEncoder()
        enc.dateEncodingStrategy = .iso8601
        enc.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        try rs.map { try enc.encode($0) + Data([0x0a]) }.reduce(Data(), +).write(to: root.appendingPathComponent("records.jsonl"))
        for a in rs.compactMap(\.audio) {
            try? fm.copyItem(at: audioDir.appendingPathComponent(a), to: root.appendingPathComponent("audio/" + a))
        }
        try? fm.copyItem(at: Hotwords.url, to: root.appendingPathComponent("hotwords.txt"))
        try? fm.copyItem(at: Trace.url, to: root.appendingPathComponent("events.log"))
        try JSONSerialization.data(withJSONObject: info, options: [.prettyPrinted, .sortedKeys]).write(to: root.appendingPathComponent("info.json"))
        try? fm.removeItem(at: zip)
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/ditto")
        p.arguments = ["-c", "-k", "--norsrc", "--noextattr", "--noacl", "--keepParent", root.path, zip.path]
        try p.run()
        p.waitUntilExit()
        if p.terminationStatus != 0 { throw CocoaError(.fileWriteUnknown) }
    }
}

/// Hotkey and utterance lifecycle lines for feedback bundles. Snapshot timestamps at the call site and persist on
/// Records' ordered queue; keeps the newer half once past 256 KB.
enum Trace {
    static let url = Records.audioDir.deletingLastPathComponent().appendingPathComponent("events.log")
    private static let fmt: ISO8601DateFormatter = {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        f.timeZone = .current
        return f
    }()

    static func log(_ s: String) {
        let at = Date()
        Records.io.async { write(s, at: at) }
    }

    private static func write(_ s: String, at: Date) {
        let line = Data((fmt.string(from: at) + " " + s + "\n").utf8)
        guard let h = try? FileHandle(forUpdating: url) else {
            try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try? line.write(to: url)
            return
        }
        defer { try? h.close() }
        let end = (try? h.seekToEnd()) ?? 0
        guard end > 256 << 10 else { try? h.write(contentsOf: line); return }
        try? h.seek(toOffset: end / 2)
        var rest = (try? h.readToEnd()) ?? Data()
        if let nl = rest.firstIndex(of: 0x0a) { rest = rest[(nl + 1)...] }
        try? h.truncate(atOffset: 0)
        try? h.write(contentsOf: rest + line)
    }
}
