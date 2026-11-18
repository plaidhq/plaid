import AppKit
import NaturalLanguage

/// The focused text field of the frontmost app, read through Accessibility. Used for ASR context (text before
/// the caret) and to notice manual corrections of what we pasted. Apps that don't expose AXValue
/// (terminals, canvas editors, some web views) simply yield nil.
/// All calls block on the target app (≤ 150 ms timeout each); run them off the main thread.
struct Field {
    let element: AXUIElement
    let value: String
    let caret: Int?  // UTF-16 offset of the selection start

    static let maxLength = 200_000
    static let queue = DispatchQueue(label: "plaid.ax")
    private static var manualAX = Set<pid_t>()
    private static let lock = NSLock()

    static func focused(pid only: pid_t? = nil) -> Field? {
        let sys = AXUIElementCreateSystemWide()
        AXUIElementSetMessagingTimeout(sys, 0.15)
        guard let app = only.map(AXUIElementCreateApplication) ?? attr(sys, kAXFocusedApplicationAttribute) as! AXUIElement? else { return nil }
        AXUIElementSetMessagingTimeout(app, 0.15)
        var pid: pid_t = 0
        AXUIElementGetPid(app, &pid)
        if pid == getpid() { return nil }
        // Electron apps only build their AX tree when asked.
        lock.lock()
        let first = manualAX.insert(pid).inserted
        lock.unlock()
        if first { AXUIElementSetAttributeValue(app, "AXManualAccessibility" as CFString, kCFBooleanTrue) }
        guard let el = attr(app, kAXFocusedUIElementAttribute) as! AXUIElement? else { return nil }
        AXUIElementSetMessagingTimeout(el, 0.15)
        return read(el)
    }

    /// Re-read the same element later; nil if it went away or isn't plain text.
    static func read(_ el: AXUIElement) -> Field? {
        if attr(el, kAXSubroleAttribute) as? String == kAXSecureTextFieldSubrole as String { return nil }
        guard let v = attr(el, kAXValueAttribute) as? String, v.utf16.count <= maxLength else { return nil }
        var caret: Int?
        if let r = attr(el, kAXSelectedTextRangeAttribute), CFGetTypeID(r) == AXValueGetTypeID() {
            var range = CFRange()
            if AXValueGetValue(r as! AXValue, .cfRange, &range) { caret = range.location }
        }
        return Field(element: el, value: v, caret: caret)
    }

    /// Up to `n` characters before the caret (or the end), for ASR context.
    func before(_ n: Int) -> String {
        let u = Array(value.utf16)
        let end = min(caret ?? u.count, u.count)
        let s = String(utf16CodeUnits: Array(u[max(0, end - n)..<end]), count: end - max(0, end - n))
        return s.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func attr(_ el: AXUIElement, _ name: String) -> CFTypeRef? {
        var v: CFTypeRef?
        return AXUIElementCopyAttributeValue(el, name as CFString, &v) == .success ? v : nil
    }
}

/// Watches the text we pasted for manual corrections: snapshot at key-down, locate the pasted span shortly after
/// the paste, re-read on the next dictation or after 60 s and record what the user changed inside the span.
final class EditWatch {
    static let shared = EditWatch()
    static var enabled: Bool { Config.value("learnEdits") != "0" }
    private let q = Field.queue
    private var pending: (id: Date, el: AXUIElement, base: String, start: Int, end: Int, pasted: String)?
    private var generation = 0

    /// Called after the paste with the field snapshot taken at key-down. Main thread.
    func pasted(_ text: String, record id: Date, before: Field?) {
        generation += 1
        let gen = generation
        guard Self.enabled, let before else { pending = nil; return }
        q.asyncAfter(deadline: .now() + 0.6) { [weak self] in
            guard let after = Field.read(before.element),
                  let span = Diff.insertedSpan(before.value, after.value),
                  Diff.overlap(text, span.text) > 0.8, Diff.overlap(span.text, text) > 0.8 else { return }
            DispatchQueue.main.async {
                guard let self, self.generation == gen else { return }
                self.pending = (id, before.element, after.value, span.start, span.end, span.text)
                DispatchQueue.main.asyncAfter(deadline: .now() + 60) { if self.generation == gen { self.check() } }
            }
        }
    }

    /// Compare the field with the post-paste snapshot; called at the next key-down too. Main thread.
    func check() {
        guard let p = pending else { return }
        pending = nil
        generation += 1
        q.async {
            guard let now = Field.read(p.el), now.value != p.base,
                  let edited = Diff.editedSpan(p.base, p.start, p.end, now.value), edited != p.pasted,
                  Diff.overlap(p.pasted, edited) >= 0.5 else { return }
            DispatchQueue.main.async { Records.shared.setEdited(p.id, pasted: p.pasted, edited: edited) }
        }
    }
}

/// String diffing on Characters. Offsets in `insertedSpan`/`editedSpan` are UTF-16, matching AX.
enum Diff {
    /// Region of `after` that differs from `before`: what the paste inserted.
    static func insertedSpan(_ before: String, _ after: String) -> (start: Int, end: Int, text: String)? {
        let a = Array(before.utf16), b = Array(after.utf16)
        var p = 0
        while p < a.count, p < b.count, a[p] == b[p] { p += 1 }
        var s = 0
        while s < a.count - p, s < b.count - p, a[a.count - 1 - s] == b[b.count - 1 - s] { s += 1 }
        let end = b.count - s
        guard end > p else { return nil }
        return (p, end, String(utf16CodeUnits: Array(b[p..<end]), count: end - p))
    }

    /// The span's current text, if the text around it is unchanged (typing after the span counts as part of it;
    /// alignment later ignores trailing insertions).
    static func editedSpan(_ base: String, _ start: Int, _ end: Int, _ now: String) -> String? {
        let a = Array(base.utf16), b = Array(now.utf16)
        let pre = a[..<start], suf = a[end...]
        guard b.count >= pre.count + suf.count, b.starts(with: pre), b.reversed().starts(with: suf.reversed()) else { return nil }
        let mid = Array(b[start..<(b.count - suf.count)])
        let s = String(utf16CodeUnits: mid, count: mid.count)
        return s.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : s
    }

    /// Share of `a`'s characters that survive in `b` (LCS / |a|).
    static func overlap(_ a: String, _ b: String) -> Double {
        let x = Array(a), y = Array(b)
        guard !x.isEmpty, !y.isEmpty, x.count * y.count <= 4_000_000 else { return 0 }
        return Double(lcs(x, y).count) / Double(x.count)
    }

    /// Substitutions from `a` to `b`, widened to whole ASCII words and to at least 2 characters per side,
    /// so a rule never rewrites a lone CJK character. Insertions at the edges (the user kept typing) are dropped.
    static func replacements(_ a: String, _ b: String) -> [(from: String, to: String)] {
        let x = Array(a), y = Array(b)
        guard x.count * y.count <= 4_000_000 else { return [] }
        let m = lcs(x, y)
        // Gaps between consecutive matches are the changed groups.
        var groups: [(a0: Int, a1: Int, b0: Int, b1: Int)] = []
        var pi = 0, pj = 0
        for (i, j) in m + [(x.count, y.count)] {
            if i > pi || j > pj { groups.append((pi, i, pj, j)) }
            pi = i + 1; pj = j + 1
        }
        func word(_ c: Character) -> Bool { c.isASCII && (c.isLetter || c.isNumber) }
        typealias G = (a0: Int, a1: Int, b0: Int, b1: Int)
        func merged(_ gs: [G]) -> [G] {
            var out: [G] = []
            for g in gs {
                if let last = out.last, g.a0 <= last.a1 {
                    out[out.count - 1] = (last.a0, max(last.a1, g.a1), last.b0, max(last.b1, g.b1))
                } else {
                    out.append(g)
                }
            }
            return out
        }
        // 1. Widen to whole ASCII words, so "keep a life" → "Keeplife" stays one pair.
        groups = merged(groups.map { g in
            var g = g
            while g.a0 > 0, g.b0 > 0, x[g.a0 - 1] == y[g.b0 - 1], word(x[g.a0 - 1]) { g.a0 -= 1; g.b0 -= 1 }
            while g.a1 < x.count, g.b1 < y.count, x[g.a1] == y[g.b1], word(x[g.a1]) { g.a1 += 1; g.b1 += 1 }
            return g
        })
        // 2. Only substitutions; pure insertions (kept typing) and deletions (dropped fillers) aren't ASR fixes.
        // Punctuation-only sides are formatting, not recognition errors.
        let punct = CharacterSet.punctuationCharacters.union(.whitespacesAndNewlines).union(.symbols)
        func core(_ s: String) -> String { String(s.unicodeScalars.filter { !punct.contains($0) }) }
        groups.removeAll { g in
            g.a0 == g.a1 || g.b0 == g.b1 || core(String(x[g.a0..<g.a1])).isEmpty || core(String(y[g.b0..<g.b1])).isEmpty
        }
        // 3. Widen to whole words of the corrected text ("工成" → "工程师").
        // ponytail: NLTokenizer splits unknown names into single characters, so "王思邈" still yields "思邈 → 思淼";
        // the Learned tab shows the sentence so the user can type the full name as a hotword.
        let tok = NLTokenizer(unit: .word)
        tok.string = b
        let words = tok.tokens(for: b.startIndex..<b.endIndex).map {
            (b.distance(from: b.startIndex, to: $0.lowerBound), b.distance(from: b.startIndex, to: $0.upperBound))
        }
        groups = merged(groups.map { g in
            var g = g
            let hit = words.filter { $0.0 < g.b1 && $0.1 > g.b0 }
            let l = min(g.b0 - (hit.map(\.0).min() ?? g.b0), g.a0), r = min((hit.map(\.1).max() ?? g.b1) - g.b1, x.count - g.a1)
            g.a0 -= max(l, 0); g.b0 -= max(l, 0); g.a1 += max(r, 0); g.b1 += max(r, 0)
            return g
        })
        // 4. At least 2 characters per side, alternating left/right context.
        let out = merged(groups.map { g in
            var g = g, left = true
            while g.a1 - g.a0 < 2 || g.b1 - g.b0 < 2 {
                if left, g.a0 > 0, g.b0 > 0 { g.a0 -= 1; g.b0 -= 1 }
                else if g.a1 < x.count, g.b1 < y.count { g.a1 += 1; g.b1 += 1 }
                else if g.a0 > 0, g.b0 > 0 { g.a0 -= 1; g.b0 -= 1 }
                else { break }
                left.toggle()
            }
            return g
        })
        return out.compactMap { g in
            let f = String(x[g.a0..<g.a1]).trimmingCharacters(in: .whitespaces)
            let t = String(y[g.b0..<g.b1]).trimmingCharacters(in: .whitespaces)
            guard !f.isEmpty, !t.isEmpty, f.count <= 12, t.count <= 12, core(f) != core(t), !core(t).isEmpty else { return nil }
            return (f, t)
        }
    }

    /// Matched index pairs of a longest common subsequence.
    private static func lcs(_ x: [Character], _ y: [Character]) -> [(Int, Int)] {
        let n = x.count, m = y.count
        var t = [[Int32]](repeating: [Int32](repeating: 0, count: m + 1), count: n + 1)
        for i in stride(from: n - 1, through: 0, by: -1) {
            for j in stride(from: m - 1, through: 0, by: -1) {
                t[i][j] = x[i] == y[j] ? t[i + 1][j + 1] + 1 : max(t[i + 1][j], t[i][j + 1])
            }
        }
        var out: [(Int, Int)] = [], i = 0, j = 0
        while i < n, j < m {
            if x[i] == y[j] { out.append((i, j)); i += 1; j += 1 }
            else if t[i + 1][j] >= t[i][j + 1] { i += 1 } else { j += 1 }
        }
        return out
    }
}
