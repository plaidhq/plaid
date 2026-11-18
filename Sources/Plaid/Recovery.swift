import AVFoundation
import Combine

/// Recognition-only replay. All mutable state and ASR callbacks are confined to the main thread.
final class Recovery: ObservableObject {
    static let shared = Recovery()
    @Published var recording = false
    @Published private(set) var busy = false
    @Published private(set) var activeRecordID: Date?
    @Published private(set) var status: String?
    @Published private(set) var error: String?
    private var session: VolcSession?
    private var token: UUID?

    private init() {}

    func retry(recordID: Date) {
        guard !recording, !busy else {
            error = "请等当前听写或重试结束后再试。"
            return
        }
        guard let record = Records.shared.all.last(where: { $0.id == recordID }), let audio = record.audio else {
            error = "这条记录没有保留录音。"
            return
        }
        let id = UUID()
        token = id
        activeRecordID = recordID
        busy = true
        error = nil
        status = "正在读取录音…"
        let url = Records.audioDir.appendingPathComponent(audio)
        DispatchQueue.global(qos: .userInitiated).async {
            // Audio saving shares the ordered persistence queue; a just-created filename may not exist yet.
            Records.flush()
            let result = Result { try Self.readAudio(url) }
            DispatchQueue.main.async {
                guard self.token == id else { return }
                switch result {
                case .failure(let error): self.complete(.failure(error), recordID: recordID, token: id)
                case .success(let pcm):
                    guard !self.recording else {
                        self.complete(.failure(RecoveryError("当前正在听写，请稍后重试。")), recordID: recordID, token: id)
                        return
                    }
                    self.status = "正在重新识别；不会自动粘贴…"
                    let session = VolcSession(onText: { _ in }, onDone: { result in
                        DispatchQueue.main.async { self.complete(result, recordID: recordID, token: id) }
                    })
                    self.session = session
                    session.begin(dialog: [])
                    for offset in stride(from: 0, to: pcm.count, by: 3200) {
                        session.append(pcm.subdata(in: offset..<min(offset + 3200, pcm.count)))
                    }
                    session.finish(timeout: max(30, Double(record.audioMs) / 1000 + 10))
                }
            }
        }
    }

    func cancel() {
        token = nil
        session?.cancel()
        session = nil
        busy = false
        activeRecordID = nil
        status = "已取消重试，原记录和录音未改变。"
    }

    private func complete(_ result: Result<String, Error>, recordID: Date, token id: UUID) {
        guard token == id else { return }
        token = nil
        session?.cancel()
        session = nil
        busy = false
        activeRecordID = nil
        switch result {
        case .success(let text):
            guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                error = "没有识别到文字，原记录和录音已保留。"
                status = nil
                return
            }
            guard Records.shared.all.contains(where: { $0.id == recordID }) else {
                error = "原记录已删除，无法保存重试结果。"
                status = nil
                return
            }
            Records.shared.recovered(recordID, text: text)
            status = "识别结果已保存在原记录中，请手动复制；未自动粘贴。"
        case .failure(let failure):
            error = failure.localizedDescription
            status = nil
        }
    }

    private static func readAudio(_ url: URL) throws -> Data {
        let file = try AVAudioFile(forReading: url)
        guard file.length > 0, file.processingFormat.sampleRate > 0,
              let buffer = AVAudioPCMBuffer(pcmFormat: file.processingFormat, frameCapacity: 16384) else {
            throw RecoveryError("录音为空或格式不可用。")
        }
        let converter = Resampler(from: file.processingFormat)
        var pcm = Data()
        while file.framePosition < file.length {
            try file.read(into: buffer)
            guard buffer.frameLength > 0 else { break }
            pcm += converter.convert(buffer)
        }
        guard !pcm.isEmpty else { throw RecoveryError("无法读取录音。") }
        return pcm
    }
}

private struct RecoveryError: LocalizedError {
    let errorDescription: String?
    init(_ message: String) { errorDescription = message }
}
