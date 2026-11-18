import AppKit

// swiftc Sources/Plaid/Hotkeys.swift scripts/hotkeys_check.swift -o /tmp/plaid-hotkeys-check && /tmp/plaid-hotkeys-check
@main
struct HotkeyChecks {
    static func main() {
        let option = NSEvent.ModifierFlags(rawValue: 0x80140) // Right Option + Caps Lock, as seen in events.log.
        let shiftOption = NSEvent.ModifierFlags(rawValue: 0xa0142)
        var count = 0
        func check(_ value: Bool, _ label: String) {
            precondition(value, label)
            count += 1
        }
        struct Rig {
            var gesture = HotkeyGesture()
            var hold = 61, tap = 61
            var toggled = false
            mutating func key(_ code: Int, _ down: Bool, _ at: Double, _ flags: NSEvent.ModifierFlags = [], repeatKey: Bool = false)
                -> (swallow: Bool, actions: [HotkeyGesture.Action]) {
                gesture.key(code, down: down, at: at, flags: flags, hold: hold, tap: tap, toggled: toggled, repeatKey: repeatKey)
            }
            mutating func advance(_ at: Double) -> [HotkeyGesture.Action] { gesture.advance(at: at, hold: hold) }
        }

        check(!Hotkeys.hasOtherModifiers(61, option), "Caps Lock does not block Right Option")
        check(Hotkeys.hasOtherModifiers(61, shiftOption), "reported Shift + Option flags")
        check(Hotkeys.hasOtherModifiers(61, .init(rawValue: 0x80060)), "opposite Option is another modifier")
        check(Hotkeys.hasOtherModifiers(61, [.option, .function]), "Fn + Option is a chord")
        check(!Hotkeys.hasOtherModifiers(63, .function), "Fn alone")
        check(!Hotkeys.hasOtherModifiers(103, .function), "F11 carries intrinsic Fn")
        check(!Hotkeys.hasOtherModifiers(123, [.function, .numericPad]), "arrow intrinsic flags")
        check(Hotkeys.hasOtherModifiers(103, [.function, .command]), "Command + F11")

        var r = Rig()
        check(r.key(61, true, 1, option).actions.isEmpty, "modifier down does not open mic")
        check(r.advance(1.29).isEmpty, "hold waits 300 ms")
        check(r.key(61, false, 1.1).actions == [.startToggle], "standalone tap toggles")
        check(r.advance(1.5).isEmpty, "released press has no timer action")
        r = Rig()
        _ = r.key(61, true, 2, option)
        check(r.advance(2.31) == [.startHold], "long standalone press starts hold")
        check(r.advance(2.5).isEmpty, "hold only starts once")
        check(r.key(61, false, 2.6).actions == [.stop], "hold release stops")

        // Both modifier orders, and releasing the other modifier before the hotkey.
        r = Rig()
        _ = r.key(56, true, 3, .shift)
        _ = r.key(61, true, 3.1, shiftOption)
        check(r.advance(3.5).isEmpty, "Shift before Option cannot start hold")
        _ = r.key(56, false, 3.6, option)
        check(r.key(61, false, 3.7).actions.isEmpty, "chord stays invalid after Shift release")
        r = Rig()
        _ = r.key(61, true, 4, option)
        _ = r.key(56, true, 4.1, shiftOption)
        _ = r.key(56, false, 4.15, option)
        check(r.key(61, false, 4.2).actions.isEmpty, "Option before Shift cannot toggle")
        check(r.advance(4.5).isEmpty, "chord invalidates timer")

        for code in [0, 36, 53, 49] {
            r = Rig()
            _ = r.key(61, true, 5, option)
            let chord = r.key(code, true, 5.1, option)
            check(!chord.swallow && chord.actions.isEmpty, "Option chord passes down: \(code)")
            check(!r.key(code, false, 5.15, option).swallow, "Option chord passes up: \(code)")
            check(r.key(61, false, 5.2).actions.isEmpty, "ordinary key invalidates tap: \(code)")
        }
        r = Rig()
        _ = r.key(0, true, 6)
        _ = r.key(61, true, 6.1, option)
        check(r.key(61, false, 6.2).actions.isEmpty, "ordinary key held before Option")
        _ = r.key(0, false, 6.3)
        _ = r.key(61, true, 6.4, option)
        check(r.key(61, false, 6.5).actions == [.startToggle], "next standalone press recovers")

        r = Rig()
        _ = r.key(61, true, 7, option)
        _ = r.advance(7.31)
        check(r.key(0, true, 7.4, option).actions == [.cancel], "late chord cancels, never finalizes")
        check(r.key(61, false, 7.5).actions.isEmpty, "late chord release does not stop/paste")

        r = Rig(); r.toggled = true
        _ = r.key(61, true, 8, option)
        _ = r.key(0, true, 8.1, option)
        check(r.key(61, false, 8.2).actions.isEmpty, "shortcut does not end continuous dictation")
        _ = r.key(0, false, 8.3)
        _ = r.key(61, true, 8.4, option)
        check(r.key(61, false, 8.5).actions == [.stop], "standalone tap ends continuous dictation")

        r = Rig(); r.hold = 49; r.tap = 49
        check(!r.key(49, true, 9, .command).swallow, "Command + bound Space passes down")
        check(!r.key(49, true, 9.1, [], repeatKey: true).swallow, "repeat after modifier release still passes")
        let up = r.key(49, false, 9.2)
        check(!up.swallow && up.actions.isEmpty, "Command + Space passes up without toggle")
        check(r.key(49, true, 9.3).swallow, "standalone bound Space is swallowed")
        let tapUp = r.key(49, false, 9.4)
        check(tapUp.swallow && tapUp.actions == [.startToggle], "standalone Space consumes its pair")
        _ = r.key(49, true, 10)
        r.gesture.clearPress()
        check(r.key(49, false, 10.1).swallow, "cancel still consumes the captured key-up")

        r = Rig(); r.hold = -1
        _ = r.key(61, true, 11, option)
        check(r.advance(11.4).isEmpty, "tap-only long press never starts hold")
        check(r.key(61, false, 11.5).actions.isEmpty, "tap-only long release does nothing")
        r = Rig(); r.tap = -1
        _ = r.key(61, true, 12, option)
        check(r.key(61, false, 12.1).actions.isEmpty, "hold-only short press does not record")
        r = Rig()
        _ = r.key(61, true, 13, option)
        r.hold = 62; r.tap = 62
        _ = r.key(62, true, 13.1, .init(rawValue: 0x42000))
        check(r.key(62, false, 13.2).actions == [.startToggle], "binding changes clear stale presses")
        print("hotkeys check ok (\(count) assertions)")
    }
}
