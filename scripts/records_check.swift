// Isolated persistence checks; never reads or writes the user's Plaid data.
// swiftc Sources/Plaid/Records.swift Sources/Plaid/Opus.swift scripts/records_check.swift -o /tmp/plaid-records-check && /tmp/plaid-records-check
import AVFoundation
import AppKit
import Combine
import Foundation

// Minimal collaborators keep this check independent of app startup, models and credentials.
enum Hotwords {
    static var directory = FileManager.default.temporaryDirectory.appendingPathComponent("plaid-records-check-\(UUID().uuidString)")
    static var url: URL { directory.appendingPathComponent("hotwords.txt") }
    static var autoURL: URL { directory.appendingPathComponent("hotwords.auto.txt") }
    static func terms(auto: Bool = true) -> [String] { ["Neo"] }
}
enum Config { static func value(_ key: String) -> String? { nil } }
enum Resampler {
    static let target = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16000, channels: 1, interleaved: false)!
}
enum Mic {
    struct Device { let name: String }
    static var current: Device? { nil }
}
enum Diff {
    static func replacements(_ before: String, _ after: String) -> [(from: String, to: String)] {
        before == after ? [] : [(before, after)]
    }
}

@main
struct RecordsCheck {
    static func check(_ value: Bool) { precondition(value) }

    static func record(_ offset: Int, outcome: String? = nil) -> Record {
        Record(at: Date(timeIntervalSince1970: 1_700_000_000 + Double(offset)), app: "Check", audioMs: 1000,
               asrMs: 10, llm: "off", totalMs: 20, raw: "sample", text: "sample", outcome: outcome)
    }

    static func main() throws {
        let fm = FileManager.default
        let root = Hotwords.directory
        try fm.createDirectory(at: root, withIntermediateDirectories: true)
        defer {
            Records.flush()
            try? fm.removeItem(at: root)
        }
        precondition(Records.url.deletingLastPathComponent().standardizedFileURL.path == root.standardizedFileURL.path)
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        let legacy = (0..<1005).map { record($0, outcome: $0 == 1004 ? "failed" : nil) }
        let data = try legacy.reduce(into: Data()) { data, r in
            let line = try encoder.encode(r)
            data += line
            data.append(0x0a)
        }
        try data.write(to: Records.url)
        let old = try decoder.decode(Record.self, from: encoder.encode(legacy[0]))
        precondition(old.outcome == nil && old.error == nil && old.triggerMs == nil && old.micStartMs == nil)

        let records = Records.shared
        Records.flush()
        precondition(records.all.count == 1000 && records.all.first?.at == legacy[5].at)
        precondition(records.usage.values.reduce(0) { $0 + $1.n } == 1004, "Backfill must include the pre-retention prefix, excluding failures")
        let disk = try String(contentsOf: Records.url, encoding: .utf8)
        precondition(disk.split(separator: "\n").count == 1000)

        for i in 1005..<1015 { records.add(record(i)) }
        precondition(records.all.count == 1000)
        let count = records.usage.values.reduce(0) { $0 + $1.n }
        let failed = record(1015, outcome: "failed")
        records.add(failed)
        precondition(records.usage.values.reduce(0) { $0 + $1.n } == count)
        records.recovered(failed.id, text: "recovered")
        records.recovered(failed.id, text: "recovered again")
        precondition(records.all.last?.outcome == "saved")
        precondition(records.usage.values.reduce(0) { $0 + $1.n } == count + 1, "Recovery counts a failed utterance once")
        records.setDelivery(failed.id, pasted: true, error: nil)
        precondition(records.all.last?.outcome == nil)
        Records.flush()
        try check(String(contentsOf: Records.url, encoding: .utf8).split(separator: "\n").count == 1000)

        // Audio writes and record writes use the same queue, including clear followed by a fresh add.
        let pcm = Data(repeating: 0, count: 3200)
        let audio = Records.saveAudio(pcm, at: record(1016).at)!
        Records.flush()
        precondition(fm.fileExists(atPath: Records.audioDir.appendingPathComponent(audio).path))
        try check(Data(contentsOf: Records.audioDir.appendingPathComponent(audio)).count == pcm.count + 44)

        for i in 1017..<1019 {
            var r = record(i)
            r.raw = "左克劳德右"
            r.text = "左Neo右"
            r.llm = "ok"
            records.add(r)
        }
        precondition(records.autoRules().contains { $0.from == "克劳德" && $0.to == "Neo" })
        Records.flush()
        try check(String(contentsOf: Hotwords.autoURL, encoding: .utf8).contains("克劳德 -> Neo"))
        records.clear()
        records.add(record(1020))
        Records.flush()
        precondition(records.all.count == 1 && records.autoRules().isEmpty)
        precondition(!fm.fileExists(atPath: Records.audioDir.path))
        try check(String(contentsOf: Records.url, encoding: .utf8).split(separator: "\n").count == 1)
        try check(!String(contentsOf: Hotwords.autoURL, encoding: .utf8).contains("克劳德 -> Neo"))
        let preserved = try JSONDecoder().decode([String: Usage].self, from: Data(contentsOf: Records.usageURL))
        precondition(preserved.values.reduce(0) { $0 + $1.n } == records.usage.values.reduce(0) { $0 + $1.n })
        print("records checks passed")
    }
}
