import AppKit

/// Single-key bindings; -1 disables a binding. Names are only used by the recorder UI.
enum Hotkeys {
    static var stopRecording: (() -> Void)?
    static var recording: Bool { stopRecording != nil }

    /// Device-dependent bits distinguish the two sides of each modifier.
    static let modifiers: [Int: (name: String, mask: UInt)] = [
        54: ("右 ⌘", 0x10), 55: ("左 ⌘", 0x08), 56: ("左 ⇧", 0x02), 60: ("右 ⇧", 0x04),
        58: ("左 ⌥", 0x20), 61: ("右 ⌥", 0x40), 59: ("左 ⌃", 0x01), 62: ("右 ⌃", 0x2000),
        63: ("fn", NSEvent.ModifierFlags.function.rawValue),
    ]
    private static let names: [Int: String] = [
        122: "F1", 120: "F2", 99: "F3", 118: "F4", 96: "F5", 97: "F6", 98: "F7", 100: "F8", 101: "F9", 109: "F10",
        103: "F11", 111: "F12", 105: "F13", 107: "F14", 113: "F15", 106: "F16", 64: "F17", 79: "F18", 80: "F19", 90: "F20",
        49: "空格", 48: "Tab", 51: "⌫", 117: "⌦", 115: "Home", 119: "End", 116: "Page Up", 121: "Page Down",
        123: "←", 124: "→", 125: "↓", 126: "↑",
    ]

    static func registerDefaults() {
        UserDefaults.standard.register(defaults: ["holdKey": 61, "tapKey": 61, "holdKeyName": "右 ⌥", "tapKeyName": "右 ⌥"])
    }

    static func code(_ key: String) -> Int { UserDefaults.standard.integer(forKey: key) }

    static func modifierDown(_ code: Int, _ flags: NSEvent.ModifierFlags) -> Bool? {
        modifiers[code].map { flags.rawValue & $0.mask != 0 }
    }

    static func name(_ e: NSEvent) -> String {
        let c = Int(e.keyCode)
        return modifiers[c]?.name ?? names[c] ?? e.charactersIgnoringModifiers?.uppercased() ?? "Key \(c)"
    }

    static func hasOtherModifiers(_ code: Int, _ flags: NSEvent.ModifierFlags) -> Bool {
        var relevant: NSEvent.ModifierFlags = [.command, .shift, .option, .control, .function]
        switch code {
        case 54, 55: relevant.remove(.command)
        case 56, 60: relevant.remove(.shift)
        case 58, 61: relevant.remove(.option)
        case 59, 62: relevant.remove(.control)
        case 63: relevant.remove(.function)
        default:
            // macOS marks function/navigation keys with Fn even without a physical Fn press.
            if names[code]?.hasPrefix("F") == true || (115...126).contains(code) { relevant.remove(.function) }
        }
        if !flags.intersection(relevant).isEmpty { return true }
        // The aggregate bit above cannot distinguish the opposite key of the same family.
        return modifiers.contains { $0.key != code && flags.rawValue & $0.value.mask != 0 && $0.key != 63 }
    }
}

/// Recognizes standalone presses before opening the mic; any chord invalidates the whole press.
/// Event timestamps, not delivery time, decide taps. All mutations run on the main run loop.
struct HotkeyGesture {
    static let holdDelay: TimeInterval = 0.3
    enum Action: Equatable { case startHold, startToggle, stop, cancel }
    struct Press {
        let code: Int
        let at: TimeInterval
        let endsToggle: Bool
        var otherKey: Bool
        var started = false
    }
    private(set) var press: Press?
    private var keysDown: Set<Int> = []
    private var swallowed: Set<Int> = []

    mutating func clearPress() { press = nil }

    /// A chord after the hold threshold cancels without finalizing or pasting its audio.
    private mutating func interrupt() -> [Action] {
        guard let p = press, !p.otherKey else { return [] }
        press?.otherKey = true
        return p.started ? [.cancel] : []
    }

    mutating func advance(at: TimeInterval, hold: Int) -> [Action] {
        guard let p = press, p.code == hold, !p.otherKey, !p.endsToggle, !p.started,
              at - p.at >= Self.holdDelay else { return [] }
        press?.started = true
        return [.startHold]
    }

    /// Non-modifier key events return whether their down/up pair belongs to Plaid.
    mutating func key(_ code: Int, down: Bool, at: TimeInterval, flags: NSEvent.ModifierFlags,
                      hold: Int, tap: Int, toggled: Bool, repeatKey: Bool = false) -> (swallow: Bool, actions: [Action]) {
        let modifier = Hotkeys.modifiers[code] != nil
        let wasDown = keysDown.contains(code)
        if !modifier {
            if down { keysDown.insert(code) } else { keysDown.remove(code) }
        }
        if let p = press, p.code != hold, p.code != tap { press = nil }
        var actions: [Action] = []
        if down, let p = press, code != p.code { actions += interrupt() }
        if down, let p = press, Hotkeys.hasOtherModifiers(p.code, flags) { actions += interrupt() }

        let bound = code == hold || code == tap
        if down, bound, !repeatKey, !wasDown, press == nil {
            let chord = Hotkeys.hasOtherModifiers(code, flags) || !keysDown.subtracting([code]).isEmpty
            press = Press(code: code, at: at, endsToggle: toggled, otherKey: chord)
            if !modifier, !chord { swallowed.insert(code) }
        }
        let swallow = !modifier && swallowed.contains(code)
        if !down {
            swallowed.remove(code)
            if let p = press, p.code == code {
                press = nil
                if !p.otherKey {
                    if p.endsToggle { actions.append(.stop) }
                    else if code == tap, at - p.at < Self.holdDelay { actions.append(.startToggle) }
                    else if p.started { actions.append(.stop) }
                }
            }
        }
        return (swallow, actions)
    }
}
