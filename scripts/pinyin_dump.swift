import Foundation
// Same transform as Phonetic.pinyin; emits "char\ttone\tkey" for every Han character it covers.
var out = "# Generated from macOS CFStringTransform (Phonetic.swift) by scripts/pinyin_dump.swift; do not edit.\n"
for r in [0x3400...0x4DBF, 0x4E00...0x9FFF] {
    for v in r {
        let c = Character(Unicode.Scalar(v)!)
        let m = NSMutableString(string: String(c))
        CFStringTransform(m, nil, kCFStringTransformMandarinLatin, false)
        let tone = (m as String).lowercased().trimmingCharacters(in: .whitespaces)
        CFStringTransform(m, nil, kCFStringTransformStripDiacritics, false)
        var p = (m as String).lowercased().trimmingCharacters(in: .whitespaces)
        for (a, b) in [("zh", "z"), ("ch", "c"), ("sh", "s")] where p.hasPrefix(a) { p = b + p.dropFirst(2) }
        if p.hasPrefix("n") { p = "l" + p.dropFirst() }
        for (a, b) in [("ang", "an"), ("eng", "en"), ("ing", "in")] where p.hasSuffix(a) { p = p.dropLast(3) + b }
        out += "\(c)\t\(tone)\t\(p)\n"
    }
}
try! out.write(toFile: CommandLine.arguments[1], atomically: true, encoding: .utf8)
