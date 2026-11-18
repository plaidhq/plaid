import AppKit

/// One voice command: an utterance that opens with `name` (or an alias) runs `run` instead of being pasted.
struct Action: Codable, Identifiable, Equatable {
    enum Output: String, Codable, CaseIterable { case none, hud, paste }
    var id = UUID()  // in-memory only, for SwiftUI and the running set
    var name = ""
    var aliases: [String] = []
    var run = ""  // one shell line for /bin/zsh -c
    var output = Output.hud  // what happens to stdout: dropped, shown in the HUD (last line), or pasted at the caret
    var confirm = false  // ask for ⏎ before running

    private enum CodingKeys: String, CodingKey { case name, aliases, run, output, confirm }

    init(name: String = "", aliases: [String] = [], run: String = "", output: Output = .hud, confirm: Bool = false) {
        self.name = name; self.aliases = aliases; self.run = run; self.output = output; self.confirm = confirm
    }

    /// Hand-edited files may leave out everything but name and run.
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        run = try c.decode(String.self, forKey: .run)
        aliases = try c.decodeIfPresent([String].self, forKey: .aliases) ?? []
        output = try c.decodeIfPresent(Output.self, forKey: .output) ?? .hud
        confirm = try c.decodeIfPresent(Bool.self, forKey: .confirm) ?? false
    }

    var triggers: [String] { ([name] + aliases).map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty } }
}

/// Voice commands, kept in ~/Library/Application Support/Plaid/actions.json (Settings › 命令, or edit by hand; the file
/// is re-read when it changes). Trigger modes: with a wake word ("commandMode" = wake, default), only utterances that
/// open with it are commands; with "bare", any utterance that opens with a command name is. Main thread only.
final class Actions: ObservableObject {
    static let shared = Actions()
    static let url = Hotwords.url.deletingLastPathComponent().appendingPathComponent("actions.json")
    static let timeout: TimeInterval = 30

    @Published var list: [Action] = [] { didSet { if !loading, list != oldValue { save() } } }
    /// Set when actions.json doesn't parse; the page turns read-only so the user's file isn't overwritten.
    @Published private(set) var loadError: String?
    @Published private(set) var lastRun: [UUID: RunResult] = [:]
    @Published private(set) var running: Set<UUID> = []
    private var loading = false
    private var loaded = false
    private var loadedAt: Date?  // mtime of the file as last read; nil = missing

    private init() { reload() }

    static var bare: Bool { Config.value("commandMode") == "bare" }
    /// An empty wake word (cleared in Settings) means bare mode too, so not Config.value, which treats "" as unset.
    static var wakeWord: String {
        bare ? "" : (ProcessInfo.processInfo.environment["PLAID_WAKE_WORD"] ?? UserDefaults.standard.string(forKey: "wakeWord") ?? "Plaid")
    }

    /// Re-reads the file if its modification date moved.
    func reload() {
        let mtime = (try? FileManager.default.attributesOfItem(atPath: Self.url.path))?[.modificationDate] as? Date
        guard !loaded || mtime != loadedAt else { return }
        loaded = true
        loadedAt = mtime
        loading = true
        defer { loading = false }
        guard let data = try? Data(contentsOf: Self.url) else { list = []; loadError = nil; return }
        do {
            let parsed = try JSONDecoder().decode([Action].self, from: data)
            // Keep ids of unchanged entries so open editors don't jump.
            list = parsed.enumerated().map { i, a in
                var a = a
                if i < list.count, list[i].name == a.name { a.id = list[i].id }
                return a
            }
            loadError = nil
        } catch {
            loadError = "\(error)"
        }
    }

    private func save() {
        guard loadError == nil else { return }
        let enc = JSONEncoder()
        enc.outputFormatting = [.prettyPrinted, .withoutEscapingSlashes]
        try? FileManager.default.createDirectory(at: Self.url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? enc.encode(list).write(to: Self.url, options: .atomic)
        loadedAt = (try? FileManager.default.attributesOfItem(atPath: Self.url.path))?[.modificationDate] as? Date
    }

    // MARK: Matching

    enum Parsed: Equatable {
        case dictation
        case unknown(String)  // wake word said, no command after it (text after the wake word)
        case run(Action, args: String)
    }

    func parse(_ text: String) -> Parsed {
        reload()
        return Self.parse(text, list, wake: Self.wakeWord)
    }

    /// Leading fillers and punctuation are skipped; wake word and names compare case- and width-insensitively with
    /// spaces and punctuation ignored ("嘿，Plaid。" matches "嘿 Plaid"). The longest matching name wins.
    static func parse(_ text: String, _ actions: [Action], wake: String) -> Parsed {
        let actions = actions.filter { !$0.run.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !$0.triggers.isEmpty }
        guard !actions.isEmpty else { return .dictation }  // no commands defined: the wake word is just a word
        let filler = Set("嗯呃额啊哦诶唉")
        var rest = Substring(text).drop { isSep($0) || filler.contains($0) }
        let wake = wake.trimmingCharacters(in: .whitespaces)
        if !wake.isEmpty {
            guard let r = strip(wake, from: rest) else { return .dictation }
            rest = r
        }
        var best: (action: Action, rest: Substring, len: Int)?
        for a in actions {
            for t in a.triggers where t.count > (best?.len ?? 0) {
                if let r = strip(t, from: rest) { best = (a, r, t.count) }
            }
        }
        guard let best else {
            return wake.isEmpty ? .dictation : .unknown(trim(rest))
        }
        return .run(best.action, args: trim(best.rest))
    }

    private static func isSep(_ c: Character) -> Bool {
        c.isWhitespace || c.isPunctuation || c.isSymbol
    }

    /// The rest of `s` after `prefix`, or nil. A Latin prefix must end at a word boundary ("Plaid" doesn't match "Plaids").
    private static func strip(_ prefix: String, from s: Substring) -> Substring? {
        var i = s.startIndex
        for p in prefix where !isSep(p) {
            while i < s.endIndex, isSep(s[i]) { i = s.index(after: i) }
            guard i < s.endIndex, String(s[i]).compare(String(p), options: [.caseInsensitive, .widthInsensitive]) == .orderedSame else { return nil }
            i = s.index(after: i)
        }
        if let last = prefix.last(where: { !isSep($0) }), last.isASCII, last.isLetter || last.isNumber,
           i < s.endIndex, s[i].isASCII, s[i].isLetter || s[i].isNumber { return nil }
        return s[i...]
    }

    /// Whitespace and sentence punctuation off both ends; other symbols stay ("-v", "#tag", "$5").
    private static func trim(_ s: Substring) -> String {
        let end = Set("。.！!？?，,、；;：:")
        var t = s.drop { $0.isWhitespace || end.contains($0) }
        while let l = t.last, l.isWhitespace || end.contains(l) { t = t.dropLast() }
        return String(t)
    }

    // MARK: Running

    struct RunResult {
        var code: Int32?  // exit status; nil when killed by a signal or never started
        var timedOut = false
        var out = "", err = ""
        var ms = 0
        var ok: Bool { code == 0 && !timedOut }
        /// One line for the HUD / settings: last stderr line, else exit status.
        var failure: String {
            if timedOut { return "超时（\(Int(Actions.timeout)) 秒）" }
            let line = err.split(whereSeparator: \.isNewline).last.map(String.init) ?? ""
            return line.isEmpty ? (code.map { "退出码 \($0)" } ?? "被信号终止") : line
        }
        var lastLine: String { out.split(whereSeparator: \.isNewline).last.map(String.init) ?? "" }
    }

    /// Environment for one run. `before` is the focused field's text before the caret, when it was read.
    static func env(text: String, args: String, app: String?, bundle: String?, window: String?, before: String?) -> [String: String] {
        var e = ProcessInfo.processInfo.environment
        e["PLAID_TEXT"] = text
        e["PLAID_ARGS"] = args
        e["PLAID_APP"] = app ?? ""
        e["PLAID_BUNDLE"] = bundle ?? ""
        e["PLAID_WINDOW"] = window ?? ""
        e["PLAID_BEFORE"] = before ?? ""
        return e
    }

    /// Runs `a` with `args` on stdin; `done` on main. A second run of an action that is still running is refused.
    func run(_ a: Action, args: String, env: [String: String], done: @escaping (RunResult) -> Void) {
        guard !running.contains(a.id) else {
            return done(RunResult(code: nil, err: "上一次还在运行"))
        }
        running.insert(a.id)
        let id = a.id
        DispatchQueue.global(qos: .userInitiated).async {
            var env = env
            env["PATH"] = Self.loginPath  // first use may block on the login shell, so off main
            let r = Self.spawn(["/bin/zsh", "-c", a.run], env: env, stdin: Data(args.utf8), timeout: Self.timeout)
            DispatchQueue.main.async {
                self.running.remove(id)
                self.lastRun[id] = r
                Trace.log("action \(a.name) args=\(args.count) code=\(r.code.map(String.init) ?? "-")\(r.timedOut ? " timeout" : "") ms=\(r.ms) out=\(r.out.utf8.count)B\(r.ok ? "" : " err=" + r.failure.prefix(80))")
                done(r)
            }
        }
    }

    /// PATH of the user's login shell: a GUI app gets only /usr/bin:/bin:/usr/sbin:/sbin, so Homebrew's bun/node/jq
    /// would be missing. Resolved once (interactive, so .zshrc counts); falls back to adding the Homebrew dirs.
    static let loginPath: String = {
        let base = ProcessInfo.processInfo.environment["PATH"] ?? "/usr/bin:/bin:/usr/sbin:/sbin"
        let shell = ProcessInfo.processInfo.environment["SHELL"] ?? "/bin/zsh"
        let marker = "__PLAID_PATH__"
        let r = spawn([shell, "-lic", "printf '\\n\(marker)%s\\n' \"$PATH\""], env: ProcessInfo.processInfo.environment, stdin: Data(), timeout: 5)
        if let line = r.out.split(separator: "\n").last(where: { $0.hasPrefix(marker) }) {
            let p = String(line.dropFirst(marker.count))
            if !p.isEmpty { Trace.log("login PATH from \(shell)"); return p }
        }
        Trace.log("login PATH unavailable (\(r.failure)), using defaults")
        return base + ":/opt/homebrew/bin:/usr/local/bin"
    }()

    /// Blocking. Own process group so a timeout kills the whole pipeline; stdout/stderr capped at 256 KB each.
    /// Returns once the process exits and its pipes close, waiting at most 1 s for pipes a background child keeps open.
    private static func spawn(_ argv: [String], env: [String: String], stdin: Data, timeout: TimeInterval) -> RunResult {
        let t0 = Date()
        let inP = Pipe(), outP = Pipe(), errP = Pipe()
        var fa: posix_spawn_file_actions_t?
        posix_spawn_file_actions_init(&fa)
        defer { posix_spawn_file_actions_destroy(&fa) }
        posix_spawn_file_actions_adddup2(&fa, inP.fileHandleForReading.fileDescriptor, 0)
        posix_spawn_file_actions_adddup2(&fa, outP.fileHandleForWriting.fileDescriptor, 1)
        posix_spawn_file_actions_adddup2(&fa, errP.fileHandleForWriting.fileDescriptor, 2)
        var attr: posix_spawnattr_t?
        posix_spawnattr_init(&attr)
        defer { posix_spawnattr_destroy(&attr) }
        // New process group; close every other fd; default signal handlers and an empty mask in the child.
        posix_spawnattr_setflags(&attr, Int16(POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_CLOEXEC_DEFAULT | POSIX_SPAWN_SETSIGDEF | POSIX_SPAWN_SETSIGMASK))
        posix_spawnattr_setpgroup(&attr, 0)
        var all = sigset_t(), none = sigset_t()
        sigfillset(&all); sigdelset(&all, SIGKILL); sigdelset(&all, SIGSTOP)
        sigemptyset(&none)
        posix_spawnattr_setsigdefault(&attr, &all)
        posix_spawnattr_setsigmask(&attr, &none)
        let cargv = argv.map { strdup($0) } + [nil]
        let cenv = env.map { strdup("\($0.key)=\($0.value)") } + [nil]
        defer { (cargv + cenv).forEach { free($0) } }
        var pid: pid_t = 0
        let rc = posix_spawn(&pid, argv[0], &fa, &attr, cargv, cenv)
        try? inP.fileHandleForReading.close()
        try? outP.fileHandleForWriting.close()
        try? errP.fileHandleForWriting.close()
        guard rc == 0 else {
            try? inP.fileHandleForWriting.close()
            return RunResult(code: nil, err: "无法启动 \(argv[0])：\(String(cString: strerror(rc)))")
        }

        final class Box: @unchecked Sendable {
            let lock = NSLock()
            var out = Data(), err = Data(), timedOut = false
            func with<T>(_ f: (Box) -> T) -> T { lock.lock(); defer { lock.unlock() }; return f(self) }
        }
        let box = Box(), pipes = DispatchGroup()
        signal(SIGPIPE, SIG_IGN)  // a command that never reads stdin must not kill Plaid when we write it
        DispatchQueue.global().async {
            try? inP.fileHandleForWriting.write(contentsOf: stdin)
            try? inP.fileHandleForWriting.close()
        }
        for (h, isOut) in [(outP.fileHandleForReading, true), (errP.fileHandleForReading, false)] {
            pipes.enter()
            DispatchQueue.global().async {
                while true {
                    let d = h.availableData
                    if d.isEmpty { break }
                    box.with { b in
                        if isOut { if b.out.count < 256 << 10 { b.out += d } } else if b.err.count < 256 << 10 { b.err += d }
                    }
                }
                pipes.leave()
            }
        }
        let kill = DispatchWorkItem {
            box.with { $0.timedOut = true }
            Darwin.kill(-pid, SIGTERM)
            DispatchQueue.global().asyncAfter(deadline: .now() + 1) { Darwin.kill(-pid, SIGKILL) }
        }
        DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: kill)
        var status: Int32 = 0
        while waitpid(pid, &status, 0) == -1, errno == EINTR {}
        kill.cancel()
        _ = pipes.wait(timeout: .now() + 1)
        let exited = status & 0x7f == 0
        return box.with { b in
            RunResult(code: exited ? (status >> 8) & 0xff : nil, timedOut: b.timedOut,
                      out: String(decoding: b.out, as: UTF8.self), err: String(decoding: b.err, as: UTF8.self),
                      ms: Int(Date().timeIntervalSince(t0) * 1000))
        }
    }

    /// Starting points offered by Settings › 命令 › 添加.
    static let examples: [Action] = [
        Action(name: "搜索", aliases: ["搜一下"],
               run: #"q=$(printf %s "$PLAID_ARGS" | perl -pe 's/([^A-Za-z0-9._~-])/sprintf("%%%02X",ord($1))/ge'); open "https://www.google.com/search?q=$q""#,
               output: .none),
        Action(name: "今天日期", run: #"date +%Y-%m-%d"#, output: .paste),
        Action(name: "暂停音乐", run: #"osascript -e 'if application "Music" is running then tell application "Music" to pause'"#, output: .none),
        // AppleScript's `system attribute` decodes env vars in the legacy system encoding (GB on a Chinese system); pass argv instead.
        Action(name: "通知", run: #"osascript -e 'on run argv' -e 'display notification (item 1 of argv) with title "Plaid"' -e 'end run' "$PLAID_ARGS""#, output: .none),
    ]
}
