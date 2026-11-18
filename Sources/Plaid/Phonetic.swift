import Foundation

/// Sound-alike hotword correction after recognition. Hotword lines `Target | alias | alias ~~~ block | block`:
/// any span that sounds close enough to the target or an alias becomes the target, unless a block word is within
/// 5 tokens. Chinese is compared as toneless pinyin with common confusions folded (zh/z, ang/an, l/n); Latin as
/// lowercase letters. A span must mix scripts exactly as the pattern does, so an English target never eats Chinese
/// syllables and 高 Code never matches 高考; 克劳德 -> Claude needs an explicit alias.
/// Check: `cat Sources/Plaid/Phonetic.swift scripts/hotwords_check.swift | swift -`
enum Phonetic {
    struct Entry { let target: String; let aliases: [String]; let block: [String] }

    /// key: folded toneless pinyin or lowercase letters; tone: pinyin with tone marks, unfolded (Latin: same as key).
    private struct Token { let range: Range<String.Index>; let key: String; let tone: String; let cjk: Bool }

    static func apply(_ text: String, _ entries: [Entry], threshold: Double = 0.85) -> String {
        let toks = tokens(text)
        guard !toks.isEmpty else { return text }
        // joined[i]: token i+1 follows token i with only whitespace between, so a span may cover both.
        let joined = toks.indices.dropLast().map { text[toks[$0].range.upperBound..<toks[$0 + 1].range.lowerBound].allSatisfy(\.isWhitespace) }
        var found: [(lo: Int, hi: Int, score: Double, target: String)] = []
        for e in entries {
            for p in [e.target] + e.aliases {
                let pt = tokens(p), key = pt.map(\.key).joined()
                let scripts = Set(pt.map(\.cjk))
                // One- or two-character Chinese words collide with everyday speech even at full similarity (李娜/你那,
                // 刘洋/留言, 林峰/两分 on the public sets), so they must match exactly, tones included (付平 -> 傅平).
                let short = scripts == [true] && pt.count <= 2, tone = pt.map(\.tone).joined()
                for lo in toks.indices {
                    for n in short ? [pt.count] : Array(max(1, pt.count - 1)...(pt.count + 1)) {
                        let hi = lo + n - 1
                        guard hi < toks.count, (lo..<hi).allSatisfy({ joined[$0] }), Set(toks[lo...hi].map(\.cjk)) == scripts else { continue }
                        let span = String(text[toks[lo].range.lowerBound..<toks[hi].range.upperBound])
                        if span == e.target { continue }
                        let k = toks[lo...hi].map(\.key).joined()
                        // Short keys (PR, CI, MLX) fuzzy-match too many ordinary words; only an explicit alias spelled
                        // the same way (pr for PR) may replace them.
                        if key.count < 4, !(p != e.target && k == key) { continue }
                        if short, toks[lo...hi].map(\.tone).joined() != tone { continue }
                        // An inflection, fragment or neighbour-extended copy of the pattern (prompts, Claude for
                        // Claude Code, a Plaid) is not a mishearing.
                        if k != key, k.contains(key) || key.contains(k) { continue }
                        // Same text up to case: only worth changing when the target's casing says something (GitHub);
                        // an all-lowercase target would just lowercase a sentence-initial Agent.
                        if span.lowercased() == e.target.lowercased(), !e.target.contains(where: \.isUppercase) { continue }
                        let m = max(k.count, key.count)
                        guard Double(abs(k.count - key.count)) <= Double(m) * (1 - threshold) else { continue }
                        let score = 1 - Double(lev(Array(k), Array(key))) / Double(m)
                        guard score >= threshold else { continue }
                        let ctx = text[toks[max(0, lo - 5)].range.lowerBound..<toks[min(toks.count - 1, hi + 5)].range.upperBound]
                        if e.block.contains(where: { ctx.contains($0) }) { continue }
                        found.append((lo, hi, score, e.target))
                    }
                }
            }
        }
        // Best score first, longer span on ties; skip anything overlapping a span already taken.
        var taken: [(lo: Int, hi: Int, target: String)] = []
        for f in found.sorted(by: { ($0.score, $0.hi - $0.lo) > ($1.score, $1.hi - $1.lo) })
        where !taken.contains(where: { f.lo <= $0.hi && $0.lo <= f.hi }) {
            taken.append((f.lo, f.hi, f.target))
        }
        var out = text
        for t in taken.sorted(by: { $0.lo > $1.lo }) {
            out.replaceSubrange(toks[t.lo].range.lowerBound..<toks[t.hi].range.upperBound, with: t.target)
        }
        return out
    }

    /// One token per Chinese character, one per run of ASCII letters/digits; everything else separates.
    private static func tokens(_ s: String) -> [Token] {
        var out: [Token] = []
        var i = s.startIndex
        while i < s.endIndex {
            let c = s[i]
            if c.isASCII, c.isLetter || c.isNumber {
                var j = s.index(after: i)
                while j < s.endIndex, s[j].isASCII, s[j].isLetter || s[j].isNumber { j = s.index(after: j) }
                let k = s[i..<j].lowercased()
                out.append(Token(range: i..<j, key: k, tone: k, cjk: false))
                i = j
            } else {
                let j = s.index(after: i)
                if let py = pinyin(c) { out.append(Token(range: i..<j, key: py.key, tone: py.tone, cjk: true)) }
                i = j
            }
        }
        return out
    }

    private static var cache: [Character: (key: String, tone: String)] = [:]
    private static let lock = NSLock()

    /// Toneless pinyin with confusable sounds folded, nil for anything that is not a Han character.
    /// ponytail: per-character reading, so polyphones (行 xing/hang) take the default reading.
    private static func pinyin(_ c: Character) -> (key: String, tone: String)? {
        guard let u = c.unicodeScalars.first, c.unicodeScalars.count == 1, (0x4E00...0x9FFF).contains(u.value) || (0x3400...0x4DBF).contains(u.value) else { return nil }
        lock.lock(); defer { lock.unlock() }
        if let p = cache[c] { return p }
        let m = NSMutableString(string: String(c))
        CFStringTransform(m, nil, kCFStringTransformMandarinLatin, false)
        let tone = (m as String).lowercased().trimmingCharacters(in: .whitespaces)
        CFStringTransform(m, nil, kCFStringTransformStripDiacritics, false)
        var p = (m as String).lowercased().trimmingCharacters(in: .whitespaces)
        for (a, b) in [("zh", "z"), ("ch", "c"), ("sh", "s")] where p.hasPrefix(a) { p = b + p.dropFirst(2) }
        if p.hasPrefix("n") { p = "l" + p.dropFirst() }
        for (a, b) in [("ang", "an"), ("eng", "en"), ("ing", "in")] where p.hasSuffix(a) { p = p.dropLast(3) + b }
        cache[c] = (p, tone)
        return (p, tone)
    }

    private static func lev(_ a: [Character], _ b: [Character]) -> Int {
        var prev = Array(0...b.count)
        for (i, x) in a.enumerated() {
            var cur = [i + 1]
            for (j, y) in b.enumerated() { cur.append(min(prev[j + 1] + 1, cur[j] + 1, prev[j] + (x == y ? 0 : 1))) }
            prev = cur
        }
        return prev[b.count]
    }
}
