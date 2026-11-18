// swiftc Sources/Plaid/Recovery.swift scripts/recovery_check.swift -o /tmp/plaid-recovery-check && /tmp/plaid-recovery-check
// Exercises the actual retry coordinator with an isolated WAV and inert recognition collaborators.
import AVFoundation
import Combine
import Foundation

struct Record { let id: Date; var audio: String?; let audioMs: Int }
final class Records {
    static let shared = Records()
    static let audioDir = FileManager.default.temporaryDirectory.appendingPathComponent("plaid-recovery-check-\(UUID().uuidString)")
    var all: [Record] = []
    var recoveredCount = 0
    static func flush() {}
    func recovered(_ id: Date, text: String) { precondition(text == "Recovered fixture"); recoveredCount += 1 }
}
final class VolcSession {
    static var deferred = false
    static var sessions: [VolcSession] = []
    let done: (Result<String, Error>) -> Void
    var bytes = 0, cancels = 0
    init(onText: @escaping (String) -> Void, onDone: @escaping (Result<String, Error>) -> Void) {
        done = onDone
        Self.sessions.append(self)
    }
    func begin(dialog: [String] = []) { precondition(dialog.isEmpty) }
    func append(_ pcm: Data) { bytes += pcm.count }
    func finish(timeout: TimeInterval) {
        precondition(bytes > 0 && timeout >= 30)
        if !Self.deferred { done(.success("Recovered fixture")) }
    }
    func cancel() { cancels += 1 }
}
struct Resampler {
    init(from format: AVAudioFormat) { precondition(format.sampleRate == 16000) }
    func convert(_ buffer: AVAudioPCMBuffer) -> Data { Data(count: Int(buffer.frameLength) * 2) }
}

@main
struct RecoveryCheck {
    static func awaitCondition(_ condition: () -> Bool) {
        let end = Date().addingTimeInterval(3)
        while !condition(), Date() < end { RunLoop.main.run(until: Date().addingTimeInterval(0.01)) }
        precondition(condition(), "Retry coordinator did not settle")
    }
    static func main() throws {
        let fm = FileManager.default, dir = Records.audioDir
        try fm.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? fm.removeItem(at: dir) }
        let format = AVAudioFormat(standardFormatWithSampleRate: 16000, channels: 1)!
        let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 1600)!
        buffer.frameLength = 1600
        buffer.floatChannelData![0].initialize(repeating: 0, count: 1600)
        let wav = dir.appendingPathComponent("fixture.wav")
        do {
            let file = try AVAudioFile(forWriting: wav, settings: format.settings)
            try file.write(from: buffer)
        }
        let record = Record(id: Date(), audio: "fixture.wav", audioMs: 100)
        Records.shared.all = [record]
        let recovery = Recovery.shared
        recovery.recording = true
        recovery.retry(recordID: record.id)
        precondition(!recovery.busy && recovery.error != nil && VolcSession.sessions.isEmpty)
        recovery.recording = false
        recovery.retry(recordID: record.id)
        precondition(recovery.busy && recovery.activeRecordID == record.id)
        awaitCondition { !recovery.busy }
        precondition(recovery.activeRecordID == nil)
        precondition(Records.shared.recoveredCount == 1 && VolcSession.sessions.count == 1 && recovery.error == nil)
        precondition(VolcSession.sessions[0].bytes == 3200)

        VolcSession.deferred = true
        recovery.retry(recordID: record.id)
        awaitCondition { VolcSession.sessions.count == 2 }
        recovery.cancel()
        VolcSession.sessions[1].done(.success("Recovered fixture"))
        RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        precondition(!recovery.busy && recovery.activeRecordID == nil && Records.shared.recoveredCount == 1)
        precondition(VolcSession.sessions[1].cancels == 1)

        Records.shared.all = [Record(id: record.id, audio: "missing.wav", audioMs: 100)]
        recovery.retry(recordID: record.id)
        awaitCondition { !recovery.busy }
        precondition(recovery.error != nil && Records.shared.recoveredCount == 1)
        print("recovery checks passed (audio read, busy guard, cancellation, late callback, missing file)")
    }
}
