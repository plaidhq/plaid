import AppKit
import AVFoundation

// CLI modes, no UI:
//   Plaid --selftest        frame encode/parse checks
//   Plaid --file a.wav      stream a file at real-time pace, print partials + release→final latency
//   Plaid --file a.wav b.wav  several in turn, PLAID_FILE_GAP seconds apart
//   swift scripts/eval.swift  run the saved recordings through the ASR, CER vs what was pasted
//   PLAID_API_KEY=… Plaid --store-key   save the key to the Keychain as this app (no access prompts later)
//   Plaid --ogg a.wav b.ogg write the Ogg/Opus stream we would upload, for checking with ffprobe
//   Plaid --refine "text"   run the LLM optimize pass with the saved settings, print result + latency
//   Plaid --settings        launch and open the settings window
//   Plaid --field [pid]     print the focused text field (of the frontmost app, or pid) as edit learning sees it
//   Plaid --export x.zip [n]  feedback bundle of the last n (20) records, same as Settings › Records › Export
//   scripts/replay.sh x.zip replay a feedback bundle exported from Settings › Records
//   Plaid --hud-demo        cycle the HUD states (PLAID_SNAPSHOT=dir writes a PNG per state)
//   Plaid --command "Plaid 搜索 x"  parse as a voice command with the saved actions and run it (output printed, never pasted)
//   Plaid --mic out.wav     record 2 s from the mic (PLAID_MIC_DEVICE=… for a device), print format/first-buffer/RMS
setlinebuf(stdout)
let args = CommandLine.arguments
if args.contains("--selftest") {
    selfTest()
    Records.flush()
    exit(0)
}
if args.contains("--store-key"), let k = ProcessInfo.processInfo.environment["PLAID_API_KEY"] {
    Keychain.set(k)
    print(Keychain.get() == k ? "stored" : "failed")
    exit(0)
}
if let i = args.firstIndex(of: "--ogg"), i + 2 < args.count {
    let pcm = readPCM(args[i + 1])
    let enc = OggOpus()
    var ogg = Data()
    for off in stride(from: 0, to: pcm.count, by: 6400) { ogg += enc.encode(pcm.subdata(in: off..<min(off + 6400, pcm.count))) }
    ogg += enc.encode(Data(), last: true)
    try! ogg.write(to: URL(fileURLWithPath: args[i + 2]))
    print("pcm \(pcm.count) B -> ogg \(ogg.count) B")
    exit(0)
}
if let i = args.firstIndex(of: "--refine"), i + 1 < args.count {
    let t0 = Date()
    // --history takes a JSON array of previous outputs, oldest first (a record's `ctx` reversed).
    let history = args.firstIndex(of: "--history").flatMap { j in j + 1 < args.count ? try? JSONDecoder().decode([String].self, from: Data(args[j + 1].utf8)) : nil } ?? []
    Refine.run(args[i + 1], app: nil, history: history) { r in
        let ms = Int(Date().timeIntervalSince(t0) * 1000)
        switch r {
        case .success(let t): print("refined (\(ms) ms): \(t)"); exit(0)
        case .failure(let e): print("refine failed (\(ms) ms, timedOut=\(e.timedOut)): \(e.localizedDescription)"); exit(1)
        }
    }
    RunLoop.main.run()
}
if let i = args.firstIndex(of: "--field") {
    print("AX trusted: \(AXIsProcessTrusted())")
    let f = i + 1 < args.count ? Int32(args[i + 1]).flatMap { Field.focused(pid: $0) } : Field.focused()
    print(f.map { "caret \($0.caret.map(String.init) ?? "-") value \($0.value.debugDescription)" } ?? "no readable field")
    exit(0)
}
if let i = args.firstIndex(of: "--mic"), i + 1 < args.count {
    let engine = AVAudioEngine(), input = engine.inputNode
    let dev = Mic.override
    print("inputs \(Mic.inputs().map { "\($0.name) [\($0.uid)]" }), using \(Mic.current?.name ?? "-")")
    if let dev { print("apply: \(Mic.apply(dev, to: input))") }
    let fmt = input.outputFormat(forBus: 0), rs = Resampler(from: fmt)
    var pcm = Data(), first: Date?
    let t0 = Date()
    input.installTap(onBus: 0, bufferSize: 1600, format: fmt) { buf, _ in
        let d = rs.convert(buf)
        DispatchQueue.main.async { if first == nil { first = Date() }; pcm += d }
    }
    try! engine.start()
    print("format=\(fmt) start=\(Int(Date().timeIntervalSince(t0) * 1000)) ms")
    DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
        engine.stop()
        print("first buffer \(first.map { Int($0.timeIntervalSince(t0) * 1000) } ?? -1) ms, \(pcm.count / 32) ms audio, RMS \(Int(Resampler.rms(pcm)))")
        try! (Records.wavHeader(pcm.count) + pcm).write(to: URL(fileURLWithPath: args[i + 1]))
        exit(0)
    }
    RunLoop.main.run()
}
if let i = args.firstIndex(of: "--export"), i + 1 < args.count {
    let n = i + 2 < args.count ? Int(args[i + 2]) ?? 20 : 20
    let records = Array(Records.shared.all.suffix(n))
    Records.export(records, to: URL(fileURLWithPath: args[i + 1]), info: Records.exportInfo()) { result in
        switch result {
        case .success: print("exported \(records.count) records"); exit(0)
        case .failure(let error): print("export failed: \(error.localizedDescription)"); exit(1)
        }
    }
    RunLoop.main.run()
}
if let i = args.firstIndex(of: "--command"), i + 1 < args.count {
    runCommandCLI(args[i + 1])
    RunLoop.main.run()
}
if args.contains("--hud-demo") {
    // Cycles the HUD states on screen; PLAID_SNAPSHOT=dir also writes one PNG per state.
    NSApplication.shared.setActivationPolicy(.accessory)
    let hud = HUD(), dir = ProcessInfo.processInfo.environment["PLAID_SNAPSHOT"]
    let long = "我的意思是说你大概的算一下普遍情况就可以了，还有风险。然后目前不太建议直接关闭，一方面是说这个功能确实有帮助，另外一方面是有一些关闭需要别人团队，而不是单向的。"
    let steps: [(String, () -> Void)] = [
        ("listening", { hud.listen(); hud.level(40) }), ("quiet", { hud.quiet("几乎没有声音 · MiRemoteV 2ch") }), ("speaking", { hud.level(1800) }),
        ("live", { hud.show("净抓这些鸡毛蒜皮的小事") }), ("long", { hud.level(900); hud.show(long) }),
        ("toggled", { hud.listen(); hud.continuous() }), ("toggled-live", { hud.show("净抓这些鸡毛蒜皮的小事") }),
        ("finishing", { hud.finishing() }), ("optimizing", { hud.optimizing("净抓这些鸡毛蒜皮的小事") }),
        ("done", { hud.done("净抓这些鸡毛蒜皮的小事。") }),
        ("saved", { hud.saved("输入位置已变化或无法确认，本次结果没有自动粘贴。") }),
        ("command", { hud.listen(); hud.show("Plaid，搜索 Swift 并发", command: true) }), ("running", { hud.running("搜索：Swift 并发") }),
        ("note", { hud.note("2026-11-10") }), ("ask", { hud.ask("推送：main   ⏎ 执行 · esc 取消", seconds: 3) }), ("error", { hud.flash("未设置火山引擎 API Key") }),
        ("low", { hud.flash("MiRemoteV 2ch 音量过低，识别会漏字；可在 设置 › 麦克风 换输入设备") }),
    ]
    for (n, (name, step)) in steps.enumerated() {
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2 * Double(n) + 0.3) {
            step()
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) { if let dir { hud.snapshot("\(dir)/hud-\(n)-\(name).png") } }
        }
    }
    DispatchQueue.main.asyncAfter(deadline: .now() + 1.2 * Double(steps.count) + 3.5) { exit(0) }
    NSApplication.shared.run()
}
var fileQueue: [String] = []
var fileSession: VolcSession?
if let i = args.firstIndex(of: "--file"), i + 1 < args.count {
    fileQueue = Array(args[(i + 1)...].prefix { !$0.hasPrefix("--") })
    fileSession = streamFile(fileQueue.removeFirst())
    RunLoop.main.run()
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()

final class AppDelegate: NSObject, NSApplicationDelegate {
    private let status = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    private let hud = HUD()
    private var engine = AVAudioEngine()
    private var engineDevice = ""  // UID the engine was pointed at; "" = default input
    private var micName: String?  // device of this utterance, logged in the record
    /// One utterance at a time; its ASR session lives in the phases that use it.
    private enum Phase {
        case idle
        case recording(VolcSession, toggled: Bool)  // mic open; toggled = tap-started, keeps going after key-up until ⌥ / ⏎
        case tail(VolcSession)  // key released: capturing until the first quiet buffer, at most tailMs
        case finalizing(VolcSession)  // mic closed, waiting for the final result
        case refining(raw: String, id: Int)  // LLM pass in flight; esc pastes the raw text
    }
    private var phase = Phase.idle {
        didSet {
            Recovery.shared.recording = !idle
            BackgroundLearner.shared.paused = !idle
        }
    }
    private var refineID = 0
    private var refineTask: URLSessionDataTask?
    private var pasteTarget: PasteTarget?
    private var targetCaptureComplete = false
    private var deliveryGeneration = 0
    private var triggerMs: Int?, micStartMs: Int?
    private var idle: Bool { if case .idle = phase { true } else { false } }
    private var session: VolcSession? {
        switch phase {
        case .recording(let s, _), .tail(let s), .finalizing(let s): s
        case .idle, .refining: nil
        }
    }
    /// The session whose mic is open (recording or tail).
    private var capturing: VolcSession? {
        switch phase {
        case .recording(let s, _), .tail(let s): s
        default: nil
        }
    }
    private var gesture = HotkeyGesture()
    private var toggled: Bool { if case .recording(_, toggled: true) = phase { true } else { false } }
    private var lastFlags: (code: Int, at: TimeInterval) = (-1, 0)
    private var keyTap: CFMachPort?
    private var levels: [Int] = []  // per tap buffer RMS of this utterance, for calibrating silenceRMS
    // Per-utterance timings and context for the records log.
    private var startAt = Date(), audioEndAt = Date()
    private var firstTextAt: Date?, releaseAt: Date?
    private var appName: String?, windowTitle: String?, appBundle: String?
    /// A matched voice command marked `confirm`, waiting for ⏎ (esc drops it). Not a Phase: it must not block the next press.
    private var pendingAction: (action: Action, args: String, env: [String: String])?
    private var asrCtx: [String] = []
    private var audio = Data()  // this utterance's 16 kHz PCM, saved for replay (`Plaid --file`)
    private var field: Field?  // focused text field at key-down

    func applicationDidFinishLaunching(_ note: Notification) {
        Hotkeys.registerDefaults()
        status.button?.image = NSImage(systemSymbolName: "mic", accessibilityDescription: "Plaid")
        let menu = NSMenu()
        menu.addItem(withTitle: "历史…", action: #selector(openHistory), keyEquivalent: "").target = self
        menu.addItem(withTitle: "设置…", action: #selector(openSettings), keyEquivalent: ",").target = self
        menu.addItem(.separator())
        menu.addItem(withTitle: "退出", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        status.menu = menu

        // Without a main menu an accessory app's text fields ignore ⌘C/⌘V/⌘A/⌘Z. Never shown, only routes key equivalents.
        let edit = NSMenu(title: "Edit")
        for (title, action, key) in [("Undo", "undo:", "z"), ("Redo", "redo:", "Z"), ("Cut", "cut:", "x"), ("Copy", "copy:", "c"),
                                     ("Paste", "paste:", "v"), ("Select All", "selectAll:", "a"), ("Close", "performClose:", "w")] {
            edit.addItem(withTitle: title, action: Selector(action), keyEquivalent: key)
        }
        let main = NSMenu()
        main.addItem(withTitle: "Edit", action: nil, keyEquivalent: "").submenu = edit
        NSApp.mainMenu = main

        AVCaptureDevice.requestAccess(for: .audio) { [weak self] ok in
            guard ok else { return }
            DispatchQueue.main.async { [weak self] in self?.prepareIfIdle() }
        }
        // Mic setting and device hot-plug: re-prepare while idle (cheap when nothing changed).
        for name in [UserDefaults.didChangeNotification, AVCaptureDevice.wasConnectedNotification, AVCaptureDevice.wasDisconnectedNotification] {
            NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in self?.schedulePrepare() }
        }
        // Needed for both the global key monitor and posting ⌘V.
        AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue(): true] as CFDictionary)
        if Config.apiKey == nil || args.contains("--settings") { SettingsWindow.show(args.contains("--settings") ? nil : .general) }
        if !Actions.shared.list.isEmpty { DispatchQueue.global(qos: .utility).async { _ = Actions.loginPath } }

        let handler: (NSEvent) -> Void = { [weak self] e in self?.flagsChanged(e) }
        NSEvent.addGlobalMonitorForEvents(matching: .flagsChanged, handler: handler)
        NSEvent.addLocalMonitorForEvents(matching: .flagsChanged) { handler($0); return $0 }
        // A global monitor added before Accessibility is granted may stay dead; add another once trusted.
        // Duplicate events are harmless: flagsChanged() drops the repeat (same keyCode and timestamp).
        // The key tap is needed up front for non-modifier hotkeys.
        installKeyTap()
        if !AXIsProcessTrusted() {
            Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] t in
                guard AXIsProcessTrusted() else { return }
                t.invalidate()
                NSEvent.addGlobalMonitorForEvents(matching: .flagsChanged, handler: handler)
                self?.installKeyTap()
            }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        refineTask?.cancel()
        BackgroundLearner.shared.cancelAll()
        Recovery.shared.cancel()
        if capturing != nil { closeMic() }
        session?.cancel()
        ClipboardPaste.shared.restore()
        DictionaryDraft.shared.flush()
        Records.flush()
    }

    @objc private func openSettings() { SettingsWindow.show() }
    @objc private func openHistory() { SettingsWindow.show(.history) }

    private var prepareID = 0
    private func schedulePrepare() {
        prepareID += 1
        let id = prepareID
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { [weak self] in
            if let self, id == self.prepareID { self.prepareIfIdle() }
        }
    }

    /// prepareEngine may replace the engine, so never mid-utterance.
    private func prepareIfIdle() {
        guard idle, AVCaptureDevice.authorizationStatus(for: .audio) == .authorized else { return }
        prepareEngine()
    }

    private func flagsChanged(_ e: NSEvent) {
        let code = Int(e.keyCode)
        guard !Hotkeys.recording, var down = Hotkeys.modifierDown(code, e.modifierFlags) else { return }
        guard code != lastFlags.code || e.timestamp != lastFlags.at else { return }  // same event via two monitors
        lastFlags = (code, e.timestamp)
        // A modifier reports flagsChanged only on press and release, so while it is held the next event for it is the
        // release whatever its flags say. A synthetic release that keeps the key's flag bit set (remote-control helpers
        // posting Right ⌥) otherwise left the utterance recording forever.
        if gesture.press?.code == code { down = false }
        if code == Hotkeys.code("holdKey") || code == Hotkeys.code("tapKey") {
            Trace.log("key \(code) \(down ? "down" : "up") flags=\(String(e.modifierFlags.rawValue, radix: 16))")
        }
        _ = hotkey(code, down: down, at: e.timestamp, flags: e.modifierFlags)
    }

    /// Recognize synchronously so chord/repeat/swallow decisions stay ordered. Audio work is deferred out of the tap.
    private func hotkey(_ code: Int, down: Bool, at: TimeInterval, flags: NSEvent.ModifierFlags, repeatKey: Bool = false) -> Bool {
        let previous = gesture.press
        let result = gesture.key(code, down: down, at: at, flags: flags,
                                 hold: Hotkeys.code("holdKey"), tap: Hotkeys.code("tapKey"), toggled: toggled, repeatKey: repeatKey)
        if let p = gesture.press, p.otherKey, previous?.otherKey != true {
            Trace.log("hotkey chord ignored key=\(code) flags=\(String(flags.rawValue, radix: 16))")
        }
        let triggeredAt = gesture.press?.at ?? previous?.at
        for action in result.actions { DispatchQueue.main.async { self.applyHotkey(action, triggeredAt: triggeredAt) } }
        if let p = gesture.press, !p.otherKey, p.code == Hotkeys.code("holdKey"),
           previous?.code != p.code || previous?.at != p.at {
            let code = p.code, pressedAt = p.at
            let delay = max(0, pressedAt + HotkeyGesture.holdDelay - ProcessInfo.processInfo.systemUptime)
            DispatchQueue.main.asyncAfter(deadline: .now() + delay) { [self] in
                guard !Hotkeys.recording, gesture.press?.code == code, gesture.press?.at == pressedAt else { return }
                for action in gesture.advance(at: ProcessInfo.processInfo.systemUptime, hold: Hotkeys.code("holdKey")) {
                    applyHotkey(action, triggeredAt: pressedAt)
                }
            }
        }
        return result.swallow
    }

    private func applyHotkey(_ action: HotkeyGesture.Action, triggeredAt: TimeInterval? = nil) {
        switch action {
        case .startHold: start(triggeredAt: triggeredAt)
        case .startToggle:
            start(triggeredAt: triggeredAt)
            if case .recording(let s, toggled: false) = phase { phase = .recording(s, toggled: true); hud.continuous() }
        case .stop: stop()
        case .cancel:
            Trace.log("hotkey chord cancelled")
            cancelUtterance()
        }
    }

    /// Session-level keyDown/keyUp tap: handles non-modifier hotkeys (swallowed), notes keys pressed during a hold,
    /// and swallows ⏎ / esc while toggled so the ⏎ doesn't reach the focused app (e.g. send a chat message) before the paste.
    private func installKeyTap() {
        guard keyTap == nil else { return }
        keyTap = CGEvent.tapCreate(
            tap: .cgSessionEventTap, place: .headInsertEventTap, options: .defaultTap,
            eventsOfInterest: CGEventMask(1 << CGEventType.keyDown.rawValue | 1 << CGEventType.keyUp.rawValue),
            callback: { _, type, e, me in
                let d = Unmanaged<AppDelegate>.fromOpaque(me!).takeUnretainedValue()
                return d.keyDown(type, e) ? nil : Unmanaged.passUnretained(e)
            },
            userInfo: Unmanaged.passUnretained(self).toOpaque())
        guard let keyTap else { return Trace.log("key tap unavailable") }  // no Accessibility yet; retried on next start()
        CFRunLoopAddSource(CFRunLoopGetMain(), CFMachPortCreateRunLoopSource(nil, keyTap, 0), .commonModes)
    }

    /// Returns true to swallow the event.
    private func keyDown(_ type: CGEventType, _ e: CGEvent) -> Bool {
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            if let keyTap { CGEvent.tapEnable(tap: keyTap, enable: true) }
            return false
        }
        guard !Hotkeys.recording else { return false }
        let code = Int(e.getIntegerValueField(.keyboardEventKeycode))
        let flags = NSEvent.ModifierFlags(rawValue: UInt(e.flags.rawValue))
        if type == .keyDown, !Hotkeys.hasOtherModifiers(code, flags), commandKey(code) { return true }
        let at = NSEvent(cgEvent: e)?.timestamp ?? ProcessInfo.processInfo.systemUptime
        return hotkey(code, down: type == .keyDown, at: at, flags: flags,
                      repeatKey: e.getIntegerValueField(.keyboardEventAutorepeat) != 0)
    }

    /// Only unmodified Return/Escape belong to Plaid; application shortcuts must pass through untouched.
    private func commandKey(_ code: Int) -> Bool {
        if let p = pendingAction, idle, [36, 76, 53].contains(code) {  // ⏎ runs a command waiting for confirmation, esc drops it
            pendingAction = nil
            DispatchQueue.main.async { [self] in
                if code == 53 { hud.hide(); Trace.log("action \(p.action.name) cancelled") } else { launch(p.action, args: p.args, env: p.env) }
            }
            return true
        }
        if case .refining(let raw, _) = phase, code == 53 {  // esc: skip the LLM pass
            refineTask?.cancel()
            refineTask = nil
            phase = .idle  // synchronously, so a second esc or the LLM reply can't deliver too
            deliver(raw: raw, text: raw, llm: "skipped", llmMs: nil, asrAt: Date())
            return true
        }
        if code == 53, !idle {
            Trace.log("esc while waiting")
            gesture.clearPress()
            cancelUtterance()
            return true
        }
        // A hold whose release never arrives can still be ended with Return.
        guard case .recording = phase else { return false }
        switch code {
        case 36, 76:  // return, keypad enter
            Trace.log("return")
            gesture.clearPress()
            stop()
            return true
        case 53:  // esc
            Trace.log("esc")
            gesture.clearPress()
            cancelUtterance()
            return true
        default:
            return false
        }
    }

    private func start(triggeredAt: TimeInterval? = nil) {
        guard !Recovery.shared.busy else { return hud.flash("正在重新识别历史录音，请稍后再试") }
        switch phase {
        case .tail(let s), .finalizing(let s):
            if capturing != nil { closeMic() }
            asrCtx = s.dialog
            let partial = s.latestText
            s.cancel()
            phase = .idle
            preserveFailure("被新的听写中断", partial: partial)
        case .refining(let raw, _):
            refineTask?.cancel()
            refineTask = nil
            phase = .idle
            deliver(raw: raw, text: raw, llm: "skipped", llmMs: nil, asrAt: Date(), allowPaste: false)
        case .recording:
            return Trace.log("start skipped: previous utterance still recording")
        case .idle: break
        }
        deliveryGeneration += 1
        guard Config.apiKey != nil else {
            SettingsWindow.show(.general)
            return hud.flash("未设置火山引擎 API Key")
        }
        installKeyTap()
        pendingAction = nil
        hud.listen()
        startAt = Date()
        audioEndAt = startAt
        triggerMs = triggeredAt.map { max(0, Int((ProcessInfo.processInfo.systemUptime - $0) * 1000)) }
        micStartMs = nil
        firstTextAt = nil
        releaseAt = nil
        windowTitle = nil
        asrCtx = []
        let front = NSWorkspace.shared.frontmostApplication
        appName = front?.localizedName
        appBundle = front?.bundleIdentifier
        pasteTarget = front.map { PasteTarget(pid: $0.processIdentifier) }
        targetCaptureComplete = false
        if let front, (Refine.enabled && Refine.appContext) || !Actions.shared.list.isEmpty {
            DispatchQueue.global().async { [weak self] in
                let t = Refine.windowTitle(of: front)
                DispatchQueue.main.async { self?.windowTitle = t }
            }
        }
        weak var made: VolcSession?  // callbacks from a session that is no longer this utterance's are dropped
        let s = VolcSession(
            onText: { [weak self] t in
                guard let self, let made, self.session === made else { return }
                guard !t.isEmpty else { return }
                if firstTextAt == nil { firstTextAt = Date() }
                hud.show(t, command: Actions.shared.parse(t) != .dictation)
            },
            onDone: { [weak self] r in
                guard let self, let made, self.session === made else { return }
                finished(r)
            })
        made = s
        phase = .recording(s, toggled: false)
        audio = Data()
        field = nil
        beginSession(s)
        EditWatch.shared.check()  // after the context read: both share the AX queue
        prepareEngine()
        micName = Mic.current?.name
        let input = engine.inputNode
        let fmt = input.outputFormat(forBus: 0)
        let rs = Resampler(from: fmt)
        levels = []
        // Int16 RMS below which a tap buffer counts as silence (Settings › Microphone). Tap buffers arrive every 100 ms,
        // so the tail is 0-100 ms when quiet.
        let silence = Double(Config.value("silenceRMS") ?? "") ?? Config.silenceDefault
        input.installTap(onBus: 0, bufferSize: 1600, format: fmt) { [weak self] buf, _ in
            let pcm = rs.convert(buf)
            let rms = Resampler.rms(pcm)
            let quiet = rms < silence
            DispatchQueue.main.async {
                // Buffers still queued when the mic closed belong after the session's last packet: drop them.
                guard let self, self.capturing === s else { return }
                s.append(pcm)
                self.audio.append(pcm)
                self.levels.append(Int(rms))
                self.hud.level(rms)
                self.checkQuiet(rms)
                if quiet, case .tail = self.phase { self.endUtterance() }
            }
        }
        do {
            try engine.start()
            micStartMs = max(0, Int(Date().timeIntervalSince(startAt) * 1000))
            Trace.log("start mic=\(micName ?? "?") triggerMs=\(triggerMs ?? -1) micStartMs=\(micStartMs ?? -1)")
        } catch {
            Trace.log("start failed: \(error)")
            input.removeTap(onBus: 0)
            s.cancel()
            phase = .idle
            preserveFailure("麦克风：\(error.localizedDescription)", partial: "")
            hud.flash("麦克风：\(error.localizedDescription)")
        }
    }

    /// Points the engine at the chosen mic. No voice processing: while enabled, even with the engine stopped, macOS
    /// cuts every other app's mic input by ~40 dB (measured 2026-11-13), and enabling it costs ~600 ms per call.
    private func prepareEngine() {
        let dev = Mic.override
        if (dev?.uid ?? "") != engineDevice {
            engine = AVAudioEngine()
            engineDevice = dev?.uid ?? ""
            if let dev, !Mic.apply(dev, to: engine.inputNode) { NSLog("mic: cannot select \(dev.name)") }
        }
    }

    /// 2 s in without any transcript and the input still near silence: say which device is recording. Cleared once
    /// a buffer gets loud, since the user may simply not have started talking.
    private func checkQuiet(_ rms: Double) {
        guard firstTextAt == nil else { return }
        if rms >= Double(Mic.quietRMS) { return hud.unquiet() }
        if levels.count == 20, lowInput { hud.quiet("几乎没有声音 · \(micName ?? "麦克风")") }
    }

    private var lowInput: Bool { levels.count >= 10 && pct(levels, 0.9)! < Mic.quietRMS }

    private var lowInputNote: String {
        "\(micName ?? "麦克风") 音量过低，识别会漏字；可在 设置 › 麦克风 换输入设备"
    }

    /// ASR context: text before the caret, then recent outputs, then the app, newest first.
    /// Waits ≤ 250 ms for the AX read; the full request is held (and audio buffered) until then.
    private func beginSession(_ s: VolcSession) {
        let useCtx = Config.value("asrContext") != "0"
        let recent = Array(Records.shared.recent(3).reversed())
        let app = appName.map { "当前应用：\($0)" }
        func dialog(_ f: Field?) -> [String] {
            guard useCtx else { return [] }
            let before = f.map { $0.before(300) } ?? ""
            return ([before] + recent.filter { !before.contains($0) } + [app ?? ""]).filter { !$0.isEmpty }
        }
        if !useCtx { s.begin() }
        let pid = pasteTarget?.pid
        let utterance = startAt
        Field.queue.async {
            let window = pid.flatMap { PasteTarget.focusedWindow(pid: $0) }
            let f = pid.flatMap { Field.focused(pid: $0) }
            DispatchQueue.main.async { [self] in
                guard startAt == utterance else { return }
                field = f
                pasteTarget?.window = window
                pasteTarget?.element = f?.element
                targetCaptureComplete = true
                if session === s, useCtx { s.begin(dialog: dialog(f)) }
            }
        }
        if useCtx {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { s.begin(dialog: dialog(nil)) }
        }
    }

    /// Keep capturing after release until the first quiet buffer so a trailing syllable isn't clipped, capped at tailMs (300).
    private func stop() {
        guard case .recording(let s, _) = phase else { return }
        phase = .tail(s)
        releaseAt = Date()
        Trace.log("stop")
        hud.finishing()
        let tail = Double(Config.value("tailMs") ?? "") ?? 300
        DispatchQueue.main.asyncAfter(deadline: .now() + tail / 1000) { [self] in
            if case .tail(let cur) = phase, cur === s { endUtterance() }
        }
    }

    private func endUtterance() {
        guard let s = capturing else { return }
        closeMic()
        phase = .finalizing(s)
        Trace.log("end levels=\(levels.count) p90=\(pct(levels, 0.9) ?? 0)")
        s.finish()
    }

    /// Explicit cancellation drops audio/results in every waiting phase and invalidates pending pastes.
    private func cancelUtterance() {
        deliveryGeneration += 1
        refineTask?.cancel()
        refineTask = nil
        let active = session
        if capturing != nil { closeMic() }
        active?.cancel()
        phase = .idle
        hud.hide()
    }

    private func closeMic() {
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        audioEndAt = Date()
    }

    private func finished(_ r: Result<String, Error>) {
        // A server error can end the session while toggled and still capturing; stop the mic too.
        guard let s = session else { return }
        let open = capturing != nil
        switch r {
        case .success(let t): Trace.log("finished chars=\(t.count)\(open ? " (still recording)" : "")")
        case .failure(let e): Trace.log("finished error: \(e.localizedDescription)")
        }
        if open { closeMic() }
        let partial = s.latestText
        s.cancel()
        asrCtx = s.dialog
        phase = .idle
        switch r {
        case .success(let raw) where !raw.isEmpty:
            switch Actions.shared.parse(raw) {
            case .dictation: break
            case .unknown(let rest):
                Trace.log("action: no match for \(rest.count) chars")
                return hud.flash(rest.isEmpty ? "没有听到命令" : "没有匹配的命令：\(rest)")
            case .run(let a, let a_args):
                return command(a, args: a_args, text: raw)
            }
            let asrAt = Date()
            guard Refine.enabled else {
                return deliver(raw: raw, text: raw, llm: "off", llmMs: nil, asrAt: asrAt)
            }
            refineID += 1
            let id = refineID
            phase = .refining(raw: raw, id: id)
            hud.optimizing(raw)
            let app = Refine.appContext ? [appName, windowTitle].compactMap { $0 }.joined(separator: " — ") : ""
            refineTask = Refine.run(raw, app: app, history: Records.shared.recent(Refine.historyCount)) { [weak self] r in
                guard let self, case .refining(_, id) = self.phase else { return }  // skipped or superseded
                self.refineTask = nil
                self.phase = .idle
                let ms = Int(Date().timeIntervalSince(asrAt) * 1000)
                switch r {
                case .success(let t): self.deliver(raw: raw, text: t, llm: "ok", llmMs: ms, asrAt: asrAt)
                case .failure(let e): self.deliver(raw: raw, text: raw, llm: e.timedOut ? "timeout" : e.localizedDescription, llmMs: ms, asrAt: asrAt)
                }
            }
        case .success:
            if (pct(levels, 0.9) ?? 0) >= Mic.quietRMS, audio.count >= 16000 {
                preserveFailure("未识别到文字", partial: partial)
                hud.flash("未识别到文字，可在历史里重试")
            } else if lowInput { hud.flash(lowInputNote) } else { hud.hide() }
            Trace.log("empty result")
        case .failure(let e):
            preserveFailure(e.localizedDescription, partial: partial)
            hud.flash(e.localizedDescription + "；可在历史里查看")
        }
    }

    private func makeRecord(raw: String, text: String, llm: String, llmMs: Int?, asrAt: Date) -> Record {
        let release = releaseAt ?? audioEndAt
        func ms(_ from: Date, _ to: Date) -> Int { max(0, Int(to.timeIntervalSince(from) * 1000)) }
        return Record(at: startAt, app: appName, audioMs: ms(startAt, audioEndAt), firstTextMs: firstTextAt.map { ms(startAt, $0) },
                      asrMs: ms(release, asrAt), llmMs: llmMs, llm: llm, totalMs: ms(release, Date()), raw: raw, text: text,
                      audio: Records.saveAudio(audio, at: startAt), noiseRMS: pct(levels, 0.1), speechRMS: pct(levels, 0.9),
                      ctx: asrCtx.isEmpty ? nil : asrCtx, engine: nil, mic: micName,
                      triggerMs: triggerMs, micStartMs: micStartMs)
    }

    private func preserveFailure(_ error: String, partial: String) {
        var record = makeRecord(raw: partial, text: partial, llm: "off", llmMs: nil, asrAt: Date())
        record.outcome = "failed"
        record.error = error
        Records.shared.add(record)
        Trace.log("saved failed utterance chars=\(partial.count)")
    }

    private func deliver(raw: String, text: String, llm: String, llmMs: Int?, asrAt: Date, allowPaste: Bool = true) {
        var record = makeRecord(raw: raw, text: text, llm: llm, llmMs: llmMs, asrAt: asrAt)
        record.outcome = "saved"  // persist first: target verification may time out or a new utterance may supersede it
        Records.shared.add(record)
        let id = record.id, generation = deliveryGeneration, before = field
        let release = releaseAt ?? audioEndAt
        let quiet = lowInput, quietMessage = lowInputNote
        guard allowPaste else { return }
        paste(text, target: pasteTarget, captured: targetCaptureComplete, generation: generation) { [weak self] posted in
            Records.shared.setDelivery(id, pasted: posted, error: posted ? nil : "输入位置已变化或无法确认，未自动粘贴",
                                       totalMs: posted ? max(0, Int(Date().timeIntervalSince(release) * 1000)) : nil)
            guard let self else { return }
            if posted {
                EditWatch.shared.pasted(text, record: id, before: before)
                if self.idle, self.deliveryGeneration == generation { self.hud.done(text) }
                if llm == "off", raw.unicodeScalars.contains(where: { $0.value >= 0x3400 }) {
                    BackgroundLearner.shared.submit(raw, id: id) { [weak self] learned in self?.announce(learned) }
                }
                if quiet {
                    DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { [weak self] in
                        guard let self, self.idle, self.deliveryGeneration == generation else { return }
                        self.hud.flash(quietMessage)
                    }
                }
            } else if self.idle, self.deliveryGeneration == generation {
                self.hud.saved("输入位置已变化或无法确认，本次结果没有自动粘贴。")
            }
        }
    }

    /// A voice command replaces the paste. Not recorded in records.jsonl (it would feed ASR/LLM context and learning);
    /// events.log has one line per run.
    private func command(_ a: Action, args: String, text: String) {
        let env = Actions.env(text: text, args: args, app: appName, bundle: appBundle, window: windowTitle, before: field?.before(2000))
        let label = args.isEmpty ? a.name : "\(a.name)：\(args)"
        guard a.confirm else { return launch(a, args: args, env: env) }
        pendingAction = (a, args, env)
        let seconds = 8.0
        hud.ask("\(label)   ⏎ 执行 · esc 取消", seconds: seconds)
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { [weak self] in
            if self?.pendingAction?.action.id == a.id { self?.pendingAction = nil }
        }
    }

    private func launch(_ a: Action, args: String, env: [String: String]) {
        let target = pasteTarget, captured = targetCaptureComplete, generation = deliveryGeneration
        hud.running(args.isEmpty ? a.name : "\(a.name)：\(args)")
        Actions.shared.run(a, args: args, env: env) { [weak self] r in
            guard let self else { return }
            let mine = self.idle && self.deliveryGeneration == generation
            guard r.ok else {
                if mine { self.hud.flash("\(a.name) 失败：\(r.failure)") }
                return
            }
            switch a.output {
            case .none:
                if mine { self.hud.done(a.name) }
            case .hud:
                if mine { self.hud.note(r.lastLine.isEmpty ? a.name : r.lastLine) }
            case .paste:
                let t = r.out.hasSuffix("\n") ? String(r.out.dropLast()) : r.out
                if t.isEmpty { if mine { self.hud.done(a.name) }; return }
                self.paste(t, target: target, captured: captured, generation: generation) { [weak self] posted in
                    guard let self, self.idle, self.deliveryGeneration == generation else { return }
                    if posted { self.hud.done(t) }
                    else { self.hud.flash("输入位置已变化，结果可在命令页查看") }
                }
            }
        }
    }

    private func announce(_ learned: [String]) {
        guard !learned.isEmpty else { return }  // a learned rewrite applies silently from now on, so say it once
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { [weak self] in
            guard let self, self.idle else { return }  // don't cover the next utterance's HUD
            self.hud.flash("学会：" + learned.joined(separator: "，").replacingOccurrences(of: " -> ", with: " → "))
        }
    }

    /// Verify the original target off-main and never activate an app to force a paste.
    private func paste(_ text: String, target: PasteTarget?, captured: Bool, generation: Int, done: @escaping (Bool) -> Void) {
        guard captured, let target, NSWorkspace.shared.frontmostApplication?.processIdentifier == target.pid,
              generation == deliveryGeneration, idle else { return done(false) }
        var settled = false  // only touched on main
        func finish(_ matches: Bool) {
            guard !settled else { return }
            settled = true
            let posted = matches && self.idle && generation == self.deliveryGeneration
                && NSWorkspace.shared.frontmostApplication?.processIdentifier == target.pid
            if posted { ClipboardPaste.shared.write(text) }
            Trace.log("delivery \(posted ? "posted" : "saved") target=\(target.pid)")
            done(posted)
        }
        Field.queue.async {
            let matches = target.matchesFocus()
            DispatchQueue.main.async { finish(matches) }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) { finish(false) }
    }
}

func streamFile(_ path: String) -> VolcSession {
    if Config.apiKey == nil { fatalError("set PLAID_API_KEY or store the key via the app menu") }
    let pcm = readPCM(path)
    let t0 = Date()
    var released = Date()
    var s: VolcSession!
    s = VolcSession(
        onText: { print(String(format: "%6.2fs  %@", Date().timeIntervalSince(t0), $0)) },
        onDone: { r in
            switch r {
            case .success(let t):
                print(String(format: "final (%.0f ms after release): %@", Date().timeIntervalSince(released) * 1000, t))
                if case .run(let a, let a_args) = Actions.shared.parse(t) {
                    print("action \(a.name) args \(a_args.debugDescription)")
                    return runAction(a, args: a_args, text: t) { nextFile() }
                }
                guard Refine.enabled, !t.isEmpty else { return nextFile() }
                let t1 = Date()
                Refine.run(t, app: nil, history: []) { r in
                    let ms = Int(Date().timeIntervalSince(t1) * 1000)
                    print("refined (\(ms) ms): " + ((try? r.get()) ?? "FAILED \(r)"))
                    nextFile()
                }
            case .failure(let e): print("error: \(e.localizedDescription)"); exit(1)
            }
        })
    // --ctx takes one text, or a JSON array (a record's `ctx`, newest first).
    if let i = args.firstIndex(of: "--ctx"), i + 1 < args.count {
        let v = args[i + 1]
        s.begin(dialog: (try? JSONDecoder().decode([String].self, from: Data(v.utf8))) ?? [v])
    } else { s.begin() }
    var off = 0
    // 100 ms feeds, like mic tap buffers; the session decides packet sizes. PLAID_FEED_SPEED=4 feeds 4x real time (eval).
    Timer.scheduledTimer(withTimeInterval: 0.1 / (Double(Config.value("feedSpeed") ?? "") ?? 1), repeats: true) { timer in
        let end = min(off + 3200, pcm.count)
        s.append(pcm.subdata(in: off..<end))
        off = end
        if off == pcm.count {
            timer.invalidate()
            released = Date()
            s.finish()
        }
    }
    return s
}

/// `--command` / `--file`: run a matched action from the CLI and print the result.
func runAction(_ a: Action, args: String, text: String, then: @escaping () -> Void) {
    let env = Actions.env(text: text, args: args, app: "Terminal", bundle: nil, window: nil, before: nil)
    Actions.shared.run(a, args: args, env: env) { r in
        print("exit \(r.code.map(String.init) ?? "-")\(r.timedOut ? " (timeout)" : "") \(r.ms) ms, output \(a.output.rawValue)")
        if !r.out.isEmpty { print("stdout: " + r.out.trimmingCharacters(in: .newlines)) }
        if !r.err.isEmpty { print("stderr: " + r.err.trimmingCharacters(in: .newlines)) }
        then()
    }
}

func runCommandCLI(_ text: String) {
    switch Actions.shared.parse(text) {
    case .dictation: print("dictation (wake word \(Actions.wakeWord.debugDescription))"); exit(0)
    case .unknown(let rest): print("no matching command: \(rest.debugDescription)"); exit(1)
    case .run(let a, let a_args):
        print("action \(a.name) args \(a_args.debugDescription)")
        runAction(a, args: a_args, text: text) { exit(0) }
    }
}

/// The next `--file` path after PLAID_FILE_GAP seconds, or exit when none are left.
func nextFile() {
    guard !fileQueue.isEmpty else { exit(0) }
    let path = fileQueue.removeFirst()
    DispatchQueue.main.asyncAfter(deadline: .now() + (Double(Config.value("fileGap") ?? "") ?? 0)) {
        print("--- \(path)")
        fileSession = streamFile(path)
    }
}

func readPCM(_ path: String) -> Data {
    let file = try! AVAudioFile(forReading: URL(fileURLWithPath: path))
    let buf = AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: AVAudioFrameCount(file.length))!
    try! file.read(into: buf)
    return Resampler(from: file.processingFormat).convert(buf)
}

func selfTest() {
    let f = Volc.frame(type: Volc.audioOnly, last: true, json: false, payload: Data([1, 2, 3]))
    precondition(f == Data([0x11, 0x22, 0x00, 0x00, 0, 0, 0, 3, 1, 2, 3]))

    let json = Data(#"{"result":{"text":"你好"}}"#.utf8)
    var resp = Data([0x11, 0x93, 0x10, 0x00, 0, 0, 0, 7])  // full response, seq + last flags
    resp.append(contentsOf: [0, 0, 0, UInt8(json.count)])
    resp.append(json)
    precondition(Volc.parse(resp) == Volc.Response(text: "你好", last: true))

    var err = Data([0x11, 0xF0, 0x10, 0x00])
    err.append(contentsOf: [0x02, 0xAE, 0xA5, 0x41, 0, 0, 0, 3])  // 45000001
    err.append(contentsOf: Array("bad".utf8))
    precondition(Volc.parse(err) == Volc.Response(last: true, error: "45000001 bad"))
    precondition(Diff.insertedSpan("ab", "aXYb")! == (1, 3, "XY"))
    precondition(Diff.editedSpan("aXYb", 1, 3, "aXZb more") == nil)  // suffix changed
    precondition(Diff.editedSpan("aXY", 1, 3, "aXZ more") == "XZ more")
    func fx(_ a: String, _ b: String) -> [String] { Diff.replacements(a, b).map { "\($0.from)>\($0.to)" } }
    precondition(fx("我叫理科，你好", "我叫李珂，你好") == ["理科>李珂"], "\(fx("我叫理科，你好", "我叫李珂，你好"))")
    precondition(fx("明天跟王思邈约个饭", "明天跟王思淼约个饭") == ["思邈>思淼"], "\(fx("明天跟王思邈约个饭", "明天跟王思淼约个饭"))")
    precondition(fx("当工成师很累", "当工程师很累") == ["工成师>工程师"], "\(fx("当工成师很累", "当工程师很累"))")
    precondition(fx("讨论 keep a life 的问题", "讨论 Keeplife 的问题") == ["keep a life>Keeplife"], "\(fx("讨论 keep a life 的问题", "讨论 Keeplife 的问题"))")
    precondition(fx("今天开会。", "今天开会，然后吃饭") == [])  // punctuation + kept typing
    precondition(fx("嗯我们去吧", "我们去吧") == [])  // deletion only
    func rl(_ a: String, _ b: String) -> [String] {
        Records.rules(a, b, ["Claude", "Claude Code", "Plaid", "CI"]).map { "\($0.from)>\($0.to)" }
    }
    precondition(rl("我们用克劳德开了三个", "我们用 Claude 开了三个") == ["克劳德>Claude"], "\(rl("我们用克劳德开了三个", "我们用 Claude 开了三个"))")
    precondition(rl("用克劳德 Code 写", "用 Claude Code 写") == ["克劳德>Claude", "克劳德 Code>Claude Code"], "\(rl("用克劳德 Code 写", "用 Claude Code 写"))")
    precondition(rl("写的，Prettier 录音会先", "写的，Plaid 录音会先") == [])  // ASCII heard: literal rewrite too risky
    precondition(rl("跑一下 ci 吧", "跑一下 CI 吧") == [])  // case only: the rewrite is case-insensitive anyway
    precondition(rl("的录音会先", "Plaid 的录音会先") == [])  // nothing was heard in its place
    precondition(rl("当工成师很累", "当工程师很累") == [])  // not a hotword
    if let c = Hotwords.context(dialog: ["x"]), c.contains("hotwords") {  // only when hotwords.txt has terms
        precondition(c.hasPrefix("{\"hotwords\":"), c)
        precondition((try? JSONSerialization.jsonObject(with: Data(c.utf8))) != nil, c)
    }
    precondition(Records.wavHeader(10).count == 44)
    let td = Volc.tidy("另外一方面是 有一些，其实 如果 e to e 测试 OK。 好")
    precondition(td == "另外一方面是有一些，其实如果 e to e 测试 OK。好", td)
    let acts = [Action(name: "搜索", aliases: ["搜一下"], run: "x"), Action(name: "搜索文件", run: "x"), Action(name: "Git Push", run: "x"),
                Action(name: "空命令", run: " ")]
    func ps(_ t: String, _ wake: String = "Plaid") -> String {
        switch Actions.parse(t, acts, wake: wake) {
        case .dictation: "dictation"
        case .unknown(let r): "unknown:" + r
        case .run(let a, let r): a.name + ":" + r
        }
    }
    for (t, wake, want) in [
        ("Plaid，搜索 Swift 并发。", "Plaid", "搜索:Swift 并发"), ("嗯，plaid 搜一下天气", "Plaid", "搜索:天气"),
        ("Plaid 搜索文件 报告", "Plaid", "搜索文件:报告"), ("嘿，Plaid。搜索", "嘿 Plaid", "搜索:"),
        ("Plaid", "Plaid", "unknown:"), ("Plaid 的录音会先", "Plaid", "unknown:的录音会先"), ("Plaids 搜索", "Plaid", "dictation"),
        ("我们 Plaid 搜索一下", "Plaid", "dictation"), ("Plaid git push", "Plaid", "Git Push:"), ("Plaid gitpush now", "Plaid", "Git Push:now"),
        ("Plaid 空命令", "Plaid", "unknown:空命令"), ("搜索引擎优化很重要", "", "搜索:引擎优化很重要"), ("今天开会", "", "dictation"),
        ("ＰＬＡＩＤ 搜索 x", "Plaid", "搜索:x"), ("Plaid 搜索：-v #tag。", "Plaid", "搜索:-v #tag"),
    ] { precondition(ps(t, wake) == want, "\(t) -> \(ps(t, wake)), want \(want)") }
    precondition(Actions.parse("Plaid 的录音会先", [], wake: "Plaid") == .dictation)  // no commands: never swallow
    precondition(Actions.parse("Plaid 搜索", [Action(name: "搜索", run: " ")], wake: "Plaid") == .dictation)
    let words = Hotwords.Snapshot(lines: ["Neo | Nio", "wrong -> right", "-> ignored"], threshold: 0.85)
    precondition(words.terms == ["Neo", "right", "ignored"])
    precondition(words.apply("wrong") == "right")
    precondition(words.context(dialog: ["prior"])?.hasPrefix("{\"hotwords\":") == true)

    let board = NSPasteboard(name: NSPasteboard.Name("plaid-check-\(UUID().uuidString)"))
    defer { board.releaseGlobally() }
    board.setString("original", forType: .string)
    var posted = 0
    let clipboard = ClipboardPaste(board: board, post: { posted += 1 })
    clipboard.write("first")
    precondition(board.string(forType: .string) == "first" && posted == 1)
    clipboard.write("second")
    clipboard.restore()
    precondition(board.string(forType: .string) == "original" && posted == 2)
    clipboard.write("third")
    board.clearContents()
    board.setString("user copied this", forType: .string)
    clipboard.restore()
    precondition(board.string(forType: .string) == "user copied this")
    clipboard.restore()
    precondition(board.string(forType: .string) == "user copied this")
    print("selftest ok (frames, learning, commands, hotwords, clipboard)")
}
