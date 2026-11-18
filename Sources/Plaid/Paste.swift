import AppKit

/// Identity only: never activates an app or moves its selection to deliver a result.
struct PasteTarget {
    let pid: pid_t
    var window: AXUIElement?
    var element: AXUIElement?

    static func focusedWindow(pid: pid_t) -> AXUIElement? {
        focusedAttribute(pid: pid, name: kAXFocusedWindowAttribute)
    }

    func matchesFocus() -> Bool {
        if let window {
            guard let current = Self.focusedWindow(pid: pid), CFEqual(window, current) else { return false }
        }
        if let element {
            guard let current = Self.focusedAttribute(pid: pid, name: kAXFocusedUIElementAttribute), CFEqual(element, current) else { return false }
        }
        return true
    }

    private static func focusedAttribute(pid: pid_t, name: String) -> AXUIElement? {
        let app = AXUIElementCreateApplication(pid)
        AXUIElementSetMessagingTimeout(app, 0.12)
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(app, name as CFString, &value) == .success,
              let value, CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
        return (value as! AXUIElement)
    }
}

/// Restores the clipboard only while this paste still owns it, including overlapping Plaid pastes.
/// A named pasteboard and injected key poster allow checks without touching the user's clipboard or keyboard.
final class ClipboardPaste {
    static let shared = ClipboardPaste()
    private let board: NSPasteboard
    private let post: () -> Void
    private var original: [NSPasteboardItem]?
    private var ownedChange: Int?
    private var restoration: DispatchWorkItem?

    init(board: NSPasteboard = .general, post: @escaping () -> Void = ClipboardPaste.postPaste) {
        self.board = board
        self.post = post
    }

    func write(_ text: String) {
        restore()
        original = board.pasteboardItems?.map { item in
            let copy = NSPasteboardItem()
            for type in item.types {
                if let data = item.data(forType: type) { copy.setData(data, forType: type) }
            }
            return copy
        } ?? []
        board.clearContents()
        board.setString(text, forType: .string)
        ownedChange = board.changeCount
        post()
        let work = DispatchWorkItem { [weak self] in self?.restore() }
        restoration = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5, execute: work)
    }

    func restore() {
        restoration?.cancel()
        restoration = nil
        defer { original = nil; ownedChange = nil }
        guard let original, ownedChange == board.changeCount else { return }
        board.clearContents()
        if !original.isEmpty { board.writeObjects(original) }
    }

    private static func postPaste() {
        let source = CGEventSource(stateID: .combinedSessionState)
        for down in [true, false] {
            let event = CGEvent(keyboardEventSource: source, virtualKey: 9, keyDown: down)
            event?.flags = .maskCommand
            event?.post(tap: .cghidEventTap)
        }
    }
}
