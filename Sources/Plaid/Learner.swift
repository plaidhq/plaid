import Foundation

/// Best-effort learning is serialized and yields result processing to live dictation.
final class BackgroundLearner {
    static let shared = BackgroundLearner()
    private struct Job {
        let id: Date
        let text: String
        let endpoint: Refine.Endpoint
        let learned: ([String]) -> Void
    }
    var paused = false { didSet { if !paused { drainResults(); pump() } } }
    private var pending: [Job] = []
    private var completed: [(Job, String)] = []
    private var task: URLSessionDataTask?
    private var running = false

    func submit(_ text: String, id: Date, learned: @escaping ([String]) -> Void) {
        guard let endpoint = Refine.learner else { return }
        pending.append(Job(id: id, text: text, endpoint: endpoint, learned: learned))
        // Learning must not build an unbounded backlog during rapid dictation or a slow endpoint.
        if pending.count > 8 { pending.removeFirst(pending.count - 8) }
        pump()
    }

    func cancelAll() {
        pending.removeAll()
        completed.removeAll()
        task?.cancel()
        task = nil
    }

    private func pump() {
        guard !paused, !running else { return }
        guard Config.value("autoLearn") != "0" else { pending.removeAll(); completed.removeAll(); return }
        while !pending.isEmpty {
            let job = pending.removeFirst()
            guard Records.shared.all.contains(where: { $0.id == job.id }) else { continue }
            running = true
            task = Refine.run(job.text, app: nil, history: [], via: job.endpoint, timeoutMs: 20000) { [weak self] result in
                guard let self else { return }
                self.task = nil
                self.running = false
                if case .success(let text) = result, text != job.text, Config.value("autoLearn") != "0" {
                    self.completed.append((job, text))
                }
                if !self.paused { self.drainResults(); self.pump() }
            }
            return
        }
    }

    private func drainResults() {
        let results = completed
        completed.removeAll()
        guard Config.value("autoLearn") != "0" else { return }
        for (job, text) in results where Records.shared.all.contains(where: { $0.id == job.id }) {
            job.learned(Records.shared.setChecked(job.id, text))
        }
    }
}
