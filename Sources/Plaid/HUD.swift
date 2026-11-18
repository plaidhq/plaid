import AppKit
import SwiftUI

/// Nonactivating dictation feedback, anchored to the screen chosen when listening starts.
final class HUD {
    private let model = HUDModel()
    private let panel: NSPanel
    private var hideID = 0
    private var screenFrame: NSRect?
    private static let canvas = NSSize(width: 760, height: 240)

    init() {
        panel = NSPanel(contentRect: NSRect(origin: .zero, size: Self.canvas), styleMask: [.borderless, .nonactivatingPanel],
                        backing: .buffered, defer: true)
        panel.level = .statusBar
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.ignoresMouseEvents = true
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.contentView = NSHostingView(rootView: HUDView(model: model))
    }

    func listen() {
        screenFrame = NSScreen.main?.visibleFrame
        model.continuous = false
        model.command = false
        present(.listening, "")
    }
    func continuous() { model.continuous = true }
    func show(_ text: String, command: Bool = false) {
        model.command = command
        present(model.mode == .finishing ? .finishing : .live, text)
    }
    func finishing() {
        model.continuous = false
        if panel.isVisible, model.mode != .error { present(.finishing, model.mode == .quiet ? "" : model.text) }
    }
    func quiet(_ hint: String) { if model.mode == .listening { present(.quiet, hint) } }
    func unquiet() { if model.mode == .quiet { present(.listening, "") } }
    func optimizing(_ text: String) { present(.optimizing, text) }
    func running(_ text: String) { present(.running, text) }
    func note(_ text: String) { present(.note, text); hide(after: 4) }
    func ask(_ text: String, seconds: Double) { present(.ask, text); hide(after: seconds) }
    func done(_ text: String) { present(.done, text); hide(after: 0.5) }
    func flash(_ text: String) { present(.error, text); hide(after: 5) }

    /// Saved text is recoverable, not an ASR error. Keep its location visible longer than success feedback.
    func saved(_ text: String) { present(.saved, text); hide(after: 8) }

    func level(_ rms: Double) {
        let v = (log10(max(rms, 1)) - log10(60)) / (log10(3000) - log10(60))
        model.level = CGFloat(min(max(v, 0), 1))
    }

    /// Renders without Screen Recording permission; used by the HUD demo.
    func snapshot(_ path: String) {
        guard let v = panel.contentView, let rep = v.bitmapImageRepForCachingDisplay(in: v.bounds) else { return }
        v.cacheDisplay(in: v.bounds, to: rep)
        try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: path))
    }

    func hide() {
        hideID += 1
        guard model.visible else { panel.orderOut(nil); screenFrame = nil; return }
        let id = hideID
        withAnimation(.easeOut(duration: 0.16)) { model.visible = false }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) { [self] in
            if hideID == id { panel.orderOut(nil); model.level = 0; screenFrame = nil }
        }
    }

    private func hide(after seconds: Double) {
        let id = hideID
        DispatchQueue.main.asyncAfter(deadline: .now() + seconds) { [self] in if hideID == id { hide() } }
    }

    private func present(_ mode: HUDModel.Mode, _ text: String) {
        hideID += 1
        if screenFrame == nil { screenFrame = NSScreen.main?.visibleFrame }
        if let frame = screenFrame {
            panel.setFrameOrigin(NSPoint(x: frame.midX - Self.canvas.width / 2, y: frame.minY + 40))
        }
        // Partial updates change text without spring-animating the entire container.
        model.mode = mode
        model.text = text
        if !panel.isVisible || !model.visible {
            panel.orderFrontRegardless()
            let id = hideID
            DispatchQueue.main.async {
                guard self.hideID == id else { return }
                withAnimation(.easeOut(duration: 0.16)) { self.model.visible = true }
            }
        }
    }
}

final class HUDModel: ObservableObject {
    enum Mode { case listening, quiet, live, finishing, optimizing, done, error, running, note, ask, saved }
    @Published var mode = Mode.listening
    @Published var text = ""
    @Published var visible = false
    @Published var level: CGFloat = 0
    @Published var continuous = false
    @Published var command = false
}

private struct HUDView: View {
    @ObservedObject var model: HUDModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack {
            Spacer(minLength: 0)
            if model.visible {
                pill.transition(.opacity)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(.bottom, 24)
    }

    private var pill: some View {
        VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 9) {
                indicator.frame(width: 25, height: 20).accessibilityHidden(true)
                Text(status).font(.system(size: 12, weight: .semibold)).foregroundStyle(tint)
                Spacer(minLength: 12)
                if let hint { Text(hint).font(.system(size: 11)).foregroundStyle(.white.opacity(0.7)) }
            }
            if !model.text.isEmpty {
                Text(previewText)
                    .font(.system(size: 15, weight: .medium))
                    .foregroundStyle(model.command && model.mode == .live ? .cyan : .white.opacity(0.95))
                    .lineLimit(3)
                    .truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .topLeading)
                    .frame(height: keepsTranscriptHeight ? 64 : nil, alignment: .topLeading)
                    .accessibilityLabel(model.text)
                    .contentTransition(.identity)
            }
            if model.mode == .saved {
                Text("菜单栏 → 历史，可复制或重新识别")
                    .font(.system(size: 12)).foregroundStyle(.white.opacity(0.75))
            }
        }
        .padding(.horizontal, 16).padding(.vertical, 12)
        .frame(width: model.text.isEmpty ? 280 : 460)
        .background {
            RoundedRectangle(cornerRadius: 16, style: .continuous)
                .fill(Color(white: 0.08).opacity(0.94))
                .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(.white.opacity(0.14), lineWidth: 0.5))
                .shadow(color: .black.opacity(0.3), radius: 14, y: 6)
        }
        .environment(\.colorScheme, .dark)
        .accessibilityElement(children: .combine)
    }

    private var keepsTranscriptHeight: Bool {
        [.live, .finishing, .optimizing, .done].contains(model.mode)
    }

    private var previewText: String {
        // Multiline head truncation retains the first line, so select the actual tail before layout.
        if keepsTranscriptHeight, model.text.count > 72 { return "…" + String(model.text.suffix(72)) }
        return model.text
    }

    private var status: String {
        switch model.mode {
        case .listening, .live: model.command ? "语音命令" : model.continuous ? "连续听写" : "正在听"
        case .quiet: "输入音量过低"
        case .finishing: "正在收尾"
        case .optimizing: "正在优化"
        case .done: "已发送"
        case .error: "需要处理"
        case .running: "正在执行"
        case .note: "命令结果"
        case .ask: "等待确认"
        case .saved: "已保存 · 未自动粘贴"
        }
    }

    private var hint: String? {
        switch model.mode {
        case .listening, .quiet, .live: model.continuous ? "⏎ 结束 · esc 取消" : "esc 取消"
        case .finishing: "esc 取消"
        case .optimizing: "esc 跳过优化"
        default: nil
        }
    }

    private var tint: Color {
        switch model.mode {
        case .error, .quiet: .orange
        case .saved: .white.opacity(0.85)
        case .done: .green
        default: .white.opacity(0.85)
        }
    }

    @ViewBuilder private var indicator: some View {
        switch model.mode {
        case .listening, .quiet, .live: Bars(level: model.level)
        case .finishing: Wave(active: !reduceMotion)
        case .optimizing: Image(systemName: "sparkles").foregroundStyle(.purple)
        case .done: Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .error: Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
        case .running, .note: Image(systemName: "terminal").foregroundStyle(.cyan)
        case .ask: Image(systemName: "return").foregroundStyle(.yellow)
        case .saved: Image(systemName: "tray.and.arrow.down").foregroundStyle(.white)
        }
    }
}

/// Stable bars follow the input level without introducing random layout changes.
private struct Bars: View {
    let level: CGFloat
    private static let shape: [CGFloat] = [0.45, 0.75, 1, 0.75, 0.45]
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        HStack(spacing: 3) {
            ForEach(0..<5, id: \.self) { i in
                Capsule().fill(.white).frame(width: 3, height: 4 + 16 * level * Self.shape[i])
            }
        }
        .animation(reduceMotion ? nil : .easeOut(duration: 0.1), value: level)
    }
}

private struct Wave: View {
    let active: Bool

    var body: some View {
        TimelineView(.animation(minimumInterval: 1.0 / 30, paused: !active)) { t in
            let p = active ? t.date.timeIntervalSinceReferenceDate * 7 : 0
            HStack(spacing: 3) {
                ForEach(0..<5, id: \.self) { i in
                    Capsule().fill(.white.opacity(0.85)).frame(width: 3, height: 5 + 7 * (1 + sin(p - Double(i) * 0.8)))
                }
            }
        }
    }
}
