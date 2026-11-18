import AVFoundation
import Foundation

// Volcengine bigmodel streaming ASR over WebSocket.
// Docs: https://www.volcengine.com/docs/6561/1354869
// Frames: 4-byte header | [seq/event if flagged] | u32 size | payload, all big-endian.
// We send uncompressed payloads without sequence numbers; the server mirrors the compression.
enum Volc {
    static let fullRequest: UInt8 = 0b0001, audioOnly: UInt8 = 0b0010
    static let serverFull: UInt8 = 0b1001, serverError: UInt8 = 0b1111

    static func frame(type: UInt8, last: Bool = false, json: Bool, payload: Data) -> Data {
        var d = Data([0x11, type << 4 | (last ? 0b0010 : 0), json ? 0x10 : 0x00, 0x00])
        withUnsafeBytes(of: UInt32(payload.count).bigEndian) { d.append(contentsOf: $0) }
        d.append(payload)
        return d
    }

    struct Response: Equatable {
        var text: String?
        var last = false
        var error: String?
    }

    /// The second pass joins VAD segments with a space ("另外一方面是 有一些"); drop spaces between CJK characters.
    static func tidy(_ t: String) -> String {
        t.replacingOccurrences(of: #"(?<=[\p{Han}\p{InCJKSymbolsAndPunctuation}\p{InHalfwidthAndFullwidthForms}]) +(?=[\p{Han}\p{InCJKSymbolsAndPunctuation}\p{InHalfwidthAndFullwidthForms}])"#, with: "", options: .regularExpression)
    }

    static func parse(_ m: Data) -> Response? {
        let b = [UInt8](m)
        guard b.count >= 4 else { return nil }
        let type = b[1] >> 4, flags = b[1] & 0x0f
        var i = Int(b[0] & 0x0f) * 4
        func u32() -> UInt32? {
            guard i + 4 <= b.count else { return nil }
            defer { i += 4 }
            return b[i..<i + 4].reduce(0) { $0 << 8 | UInt32($1) }
        }
        if flags & 0b0001 != 0 { _ = u32() }  // sequence
        if flags & 0b0100 != 0 { _ = u32() }  // event
        let code = type == serverError ? u32() : nil
        guard let size = u32(), i + Int(size) <= b.count else { return nil }
        let payload = Data(b[i..<i + Int(size)])
        if let code { return Response(last: true, error: "\(code) \(String(decoding: payload, as: UTF8.self))") }
        guard type == serverFull else { return nil }
        if payload.starts(with: [0x1f, 0x8b]) { return Response(last: true, error: "unexpected gzip payload") }
        guard !payload.isEmpty else { return Response(last: flags & 0b0010 != 0) }
        let obj = try? JSONSerialization.jsonObject(with: payload) as? [String: Any]
        let text = (obj?["result"] as? [String: Any])?["text"] as? String
        return Response(text: text, last: flags & 0b0010 != 0)
    }
}

enum Config {
    // Env (PLAID_NONSTREAM, ...) wins over `defaults write dev.neo.plaid.volc nonstream ...`.
    /// silenceRMS when unset. Raw input: room ~300-400, speech 1000+.
    static let silenceDefault: Double = 500

    static func value(_ key: String) -> String? {
        let env = "PLAID_" + key.replacingOccurrences(of: "([A-Z])", with: "_$1", options: .regularExpression).uppercased()
        let v = ProcessInfo.processInfo.environment[env] ?? UserDefaults.standard.string(forKey: key)
        return v?.isEmpty == false ? v : nil
    }
    static var url: String { value("url") ?? "wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async" }
    static var resourceId: String { value("resourceId") ?? "volc.seedasr.sauc.duration" }
    static var apiKey: String? { ProcessInfo.processInfo.environment["PLAID_API_KEY"] ?? Keychain.get() }
}

/// Generic passwords under service dev.neo.plaid.volc; "apiKey" is the Volcengine key, "llm:<host>" the LLM keys.
enum Keychain {
    private static func query(_ account: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: "dev.neo.plaid.volc", kSecAttrAccount as String: account]
    }

    static func get(_ account: String = "apiKey") -> String? {
        var q = query(account)
        q[kSecReturnData as String] = true
        var out: AnyObject?
        guard SecItemCopyMatching(q as CFDictionary, &out) == errSecSuccess, let d = out as? Data else { return nil }
        return String(decoding: d, as: UTF8.self)
    }

    /// Empty value deletes the item.
    static func set(_ v: String, account: String = "apiKey") {
        SecItemDelete(query(account) as CFDictionary)
        guard !v.isEmpty else { return }
        var q = query(account)
        q[kSecValueData as String] = Data(v.utf8)
        SecItemAdd(q as CFDictionary, nil)
    }
}

/// ~/Library/Application Support/Plaid/hotwords.txt, re-read per utterance.
/// One term per line (sent as ASR hotwords), `#` comments, `heard -> wanted` lines rewrite the result.
enum Hotwords {
    static let url = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        .appendingPathComponent("Plaid/hotwords.txt")
    static let autoURL = url.deletingLastPathComponent().appendingPathComponent("hotwords.auto.txt")  // Records.writeAutoRules

    private static func lines(auto withAuto: Bool = true) -> [String] {
        let extra = Config.value("hotwordsExtra").flatMap { try? String(contentsOfFile: $0, encoding: .utf8) } ?? ""
        let auto = withAuto ? (try? String(contentsOf: autoURL, encoding: .utf8)) ?? "" : ""
        return (((try? String(contentsOf: url, encoding: .utf8)) ?? "") + "\n" + auto + "\n" + extra).split(separator: "\n")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty && !$0.hasPrefix("#") }
    }

    /// Immutable rules for one utterance; file edits take effect on the next utterance.
    struct Snapshot {
        let terms: [String]
        let entries: [Phonetic.Entry]
        private let replacements: [(String, String)]
        private let threshold: Double

        init(lines: [String], threshold: Double) {
            self.threshold = threshold
            var seen = Set<String>()
            terms = lines.map { line in
                line.contains("->") ? line.components(separatedBy: "->").last!.trimmingCharacters(in: .whitespaces) : Hotwords.target(line)
            }.filter { seen.insert($0).inserted }
            replacements = lines.compactMap { line in
                let p = line.components(separatedBy: "->").map { $0.trimmingCharacters(in: .whitespaces) }
                return p.count == 2 && !p[0].isEmpty ? (p[0], p[1]) : nil
            }
            entries = lines.filter { !$0.contains("->") }.compactMap { line in
                let p = line.components(separatedBy: "~~~")
                let words = p[0].components(separatedBy: "|").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
                let block = p.count > 1 ? p[1].components(separatedBy: "|").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty } : []
                return words.first.map { Phonetic.Entry(target: $0, aliases: Array(words.dropFirst()), block: block) }
            }
        }

        func apply(_ text: String) -> String {
            let literal = replacements.reduce(text) { $0.replacingOccurrences(of: $1.0, with: $1.1, options: .caseInsensitive) }
            return Phonetic.apply(literal, entries, threshold: threshold)
        }

        func context(dialog: [String]) -> String? {
            func json(_ v: Any) -> String { String(decoding: try! JSONSerialization.data(withJSONObject: v), as: UTF8.self) }
            var parts: [String] = []
            if !entries.isEmpty { parts.append("\"hotwords\":" + json(entries.map { ["word": $0.target] })) }
            if !dialog.isEmpty { parts.append("\"context_type\":\"dialog_ctx\",\"context_data\":" + json(dialog.map { ["text": $0] })) }
            return parts.isEmpty ? nil : "{" + parts.joined(separator: ",") + "}"
        }
    }

    static func snapshot(auto: Bool = true) -> Snapshot {
        Snapshot(lines: lines(auto: auto), threshold: Double(Config.value("hotwordThresh") ?? "") ?? 0.85)
    }

    /// Value for `corpus.context`: hotwords (bidirectional endpoint caps them at ~100 tokens) plus
    /// `dialog_ctx` texts, newest first (capped at 800 tokens / 20 entries server-side).
    /// Built by hand so hotwords come first: the streaming pass keeps only the first ~100 tokens of this string, and
    /// JSONSerialization put context_data first, so a long before-caret text pushed every hotword out of the partials.
    static func context(dialog: [String] = []) -> String? { snapshot().context(dialog: dialog) }

    static func add(_ line: String) {
        var s = (try? String(contentsOf: url, encoding: .utf8)) ?? ""
        if !s.isEmpty, !s.hasSuffix("\n") { s += "\n" }
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? (s + line + "\n").write(to: url, atomically: true, encoding: .utf8)
    }

    static func contains(_ line: String) -> Bool { lines().contains(line) }

    /// Wanted spellings: plain terms plus the right side of rewrites. Given to the LLM pass as vocabulary.
    /// `auto: false` leaves out learned rules, so a term the user deleted can't keep teaching itself.
    static func terms(auto: Bool = true) -> [String] { snapshot(auto: auto).terms }

    private static func target(_ line: String) -> String {
        line.components(separatedBy: "~~~")[0].components(separatedBy: "|")[0].trimmingCharacters(in: .whitespaces)
    }

    /// Non-rewrite lines: `Target | alias | alias ~~~ block | block`, or a plain term.
    static func entries() -> [Phonetic.Entry] { snapshot().entries }

    /// Literal `heard -> wanted` rewrites, then sound-alike matching against targets and aliases.
    static func apply(_ text: String) -> String { snapshot().apply(text) }
}

/// One utterance: open on key-down, stream PCM, `finish()` on key-up, wait for the last response.
/// Callbacks arrive on the main queue.
final class VolcSession {
    private let task: URLSessionWebSocketTask
    private var buffer = Data()
    private var latest = ""
    var latestText: String { latest }
    private var lastRaw: String?  // processing queue only
    private let processing = DispatchQueue(label: "plaid.volc-text", qos: .userInitiated)
    private let hotwords = Hotwords.snapshot()
    private var begun = false  // full request sent
    private var finished = false  // last audio packet sent
    private var done = false  // onDone called or cancelled
    private(set) var dialog: [String] = []  // context actually sent, kept in the record for replay
    private let opus = Config.value("codec") == "pcm" ? nil : OggOpus()
    private let onText: (String) -> Void
    private let onDone: (Result<String, Error>) -> Void

    struct Failure: LocalizedError { let errorDescription: String? }

    init(onText: @escaping (String) -> Void, onDone: @escaping (Result<String, Error>) -> Void) {
        self.onText = onText
        self.onDone = onDone
        var req = URLRequest(url: URL(string: Config.url)!)
        req.setValue(Config.apiKey, forHTTPHeaderField: "X-Api-Key")
        req.setValue(Config.resourceId, forHTTPHeaderField: "X-Api-Resource-Id")
        req.setValue(UUID().uuidString, forHTTPHeaderField: "X-Api-Connect-Id")
        task = URLSession.shared.webSocketTask(with: req)
        task.resume()
    }

    /// Sends the full request. Audio is held until then, so context gathered after key-down still goes first.
    /// `dialog` = context texts, newest first. Idempotent; `finish()` begins without context if not called yet.
    func begin(dialog: [String] = []) {
        guard !begun, !done else { return }
        begun = true
        self.dialog = dialog
        var request: [String: Any] = [
            "model_name": "bigmodel",
            "enable_itn": true,
            "enable_punc": true,
            "enable_ddc": true,  // drop filler words
            "enable_nonstream": Config.value("nonstream") != "0",  // second pass per VAD segment, default on since 2026-11-05; ~+300 ms on release
        ]
        if let ctx = hotwords.context(dialog: dialog) { request["corpus"] = ["context": ctx] }
        let params: [String: Any] = [
            "user": ["uid": "plaid"],
            "audio": ["format": opus == nil ? "pcm" : "ogg", "codec": opus == nil ? "raw" : "opus", "rate": 16000, "bits": 16, "channel": 1],
            "request": request,
        ]
        send(Volc.frame(type: Volc.fullRequest, json: true, payload: try! JSONSerialization.data(withJSONObject: params)))
        receive()
        flush()
    }

    /// 16 kHz mono s16le. Sent in 200 ms packets, the size the docs recommend.
    /// A 100 ms first packet and enable_accelerate_text (score 10/20) were benched: no first-text gain.
    func append(_ pcm: Data) {
        guard !finished else { return }  // audio after the last packet makes the server fail the request
        buffer.append(pcm)
        flush()
    }

    private func flush() {
        while begun, buffer.count >= 6400 {
            let chunk = buffer.prefix(6400)
            send(Volc.frame(type: Volc.audioOnly, json: false, payload: opus?.encode(chunk) ?? chunk))
            buffer.removeFirst(6400)
        }
    }

    func finish(timeout: TimeInterval = 5) {
        guard !finished, !done else { return }
        begin()
        finished = true
        send(Volc.frame(type: Volc.audioOnly, last: true, json: false, payload: opus?.encode(buffer, last: true) ?? buffer))
        buffer.removeAll()
        // Server occasionally never sends the last packet; fall back to the latest partial rather than lose it.
        DispatchQueue.main.asyncAfter(deadline: .now() + timeout) { [weak self] in
            guard let self else { return }
            self.complete(self.latest.isEmpty ? .failure(Failure(errorDescription: "timed out waiting for final result")) : .success(self.latest))
        }
    }

    func cancel() {
        done = true
        task.cancel(with: .normalClosure, reason: nil)
    }

    private func send(_ d: Data) {
        task.send(.data(d)) { [weak self] err in
            if let err { DispatchQueue.main.async { self?.complete(.failure(err)) } }
        }
    }

    private func receive() {
        task.receive { [weak self] result in
            guard let self else { return }
            self.processing.async {
                let parsed: Result<Volc.Response?, Error>
                switch result {
                case .failure(let err):
                    let status = (self.task.response as? HTTPURLResponse).map { " (HTTP \($0.statusCode), logid \($0.value(forHTTPHeaderField: "X-Tt-Logid") ?? "-"))" } ?? ""
                    parsed = .failure(Failure(errorDescription: err.localizedDescription + status))
                case .success(let msg):
                    if case .data(let d) = msg, var r = Volc.parse(d) {
                        if let raw = r.text {
                            if raw == self.lastRaw { r.text = nil }
                            else { self.lastRaw = raw; r.text = self.hotwords.apply(Volc.tidy(raw)) }
                        }
                        parsed = .success(r)
                    } else { parsed = .success(nil) }
                }
                DispatchQueue.main.async {
                    guard !self.done else { return }
                    switch parsed {
                    case .failure(let err): self.complete(.failure(err))
                    case .success(let response):
                        guard let r = response else { return self.receive() }
                        if let e = r.error { return self.complete(.failure(Failure(errorDescription: e))) }
                        if let t = r.text, t != self.latest { self.latest = t; self.onText(t) }
                        if r.last { return self.complete(.success(self.latest)) }
                        self.receive()
                    }
                }
            }
        }
    }

    private func complete(_ r: Result<String, Error>) {
        guard !done else { return }
        cancel()
        onDone(r)
    }
}

/// Converts any input format to 16 kHz mono s16le.
final class Resampler {
    static let target = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16000, channels: 1, interleaved: true)!
    private let conv: AVAudioConverter
    private let ratio: Double
    private let mono: AVAudioFormat?  // set for multichannel float input: use channel 0 only

    /// Multichannel deinterleaved float input often has no channel layout, which AVAudioConverter can downmix to
    /// silence (seen with macOS voice processing's 9 channels); so it is reduced to channel 0.
    init(from: AVAudioFormat) {
        mono = from.channelCount > 1 && from.commonFormat == .pcmFormatFloat32 && !from.isInterleaved
            ? AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: from.sampleRate, channels: 1, interleaved: false) : nil
        conv = AVAudioConverter(from: mono ?? from, to: Self.target)!
        ratio = 16000 / from.sampleRate
    }

    func convert(_ input: AVAudioPCMBuffer) -> Data {
        var buf = input
        if let mono, let m = AVAudioPCMBuffer(pcmFormat: mono, frameCapacity: input.frameLength) {
            m.frameLength = input.frameLength
            m.floatChannelData![0].update(from: input.floatChannelData![0], count: Int(input.frameLength))
            buf = m
        }
        let out = AVAudioPCMBuffer(pcmFormat: Self.target, frameCapacity: AVAudioFrameCount(Double(buf.frameLength) * ratio) + 32)!
        var fed = false
        conv.convert(to: out, error: nil) { _, status in
            if fed { status.pointee = .noDataNow; return nil }
            fed = true
            status.pointee = .haveData
            return buf
        }
        return Data(bytes: out.int16ChannelData![0], count: Int(out.frameLength) * 2)
    }

    static func rms(_ pcm: Data) -> Double {
        let n = pcm.count / 2
        guard n > 0 else { return 0 }
        let sum = pcm.withUnsafeBytes { $0.bindMemory(to: Int16.self).reduce(0.0) { $0 + Double($1) * Double($1) } }
        return (sum / Double(n)).squareRoot()
    }
}
