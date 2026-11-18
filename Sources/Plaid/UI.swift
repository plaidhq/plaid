import AppKit
import SwiftUI

/// Shared look for the settings pages: titled pages on the window background, content in bordered cards.
struct PageHeader<Trailing: View>: View {
    let title: LocalizedStringKey
    var subtitle: String?
    @ViewBuilder var trailing: Trailing

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.system(size: 24, weight: .bold))
                if let subtitle { Text(subtitle).foregroundStyle(.secondary) }
            }
            Spacer()
            trailing
        }
    }
}

extension PageHeader where Trailing == EmptyView {
    init(title: LocalizedStringKey, subtitle: String? = nil) {
        self.init(title: title, subtitle: subtitle) { EmptyView() }
    }
}

/// Shared geometry; pages and rows read these instead of carrying their own numbers.
enum Metrics {
    static let gutter: CGFloat = 24
    static let column: CGFloat = 760
    static let gap: CGFloat = 24
    static let radius: CGFloat = 12
    static let rowInset: CGFloat = 16
}

extension Color {
    /// Card surface. controlBackgroundColor is white in light mode but equals the window background in dark mode
    /// (both #1E1E1E), so dark takes one opaque step up. Opaque on purpose: RowGroup paints rows over a
    /// separator-coloured gap.
    static let cardFill = Color(nsColor: NSColor(name: nil) { a in   // macOS 10.15+
        a.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua ? NSColor(white: 0.18, alpha: 1) : .controlBackgroundColor
    })
}

extension View {
    func card(padding: CGFloat = 16) -> some View {
        self.padding(padding)
            .frame(maxWidth: .infinity, alignment: .topLeading)
            .background(Color.cardFill)
            .clipShape(RoundedRectangle(cornerRadius: Metrics.radius, style: .continuous))   // new: rows inside get rounded corners
            .overlay(RoundedRectangle(cornerRadius: Metrics.radius, style: .continuous).strokeBorder(Color(nsColor: .separatorColor).opacity(0.5)))
    }

    /// Chrome for text input that isn't a system text field: search, add-hotword, editors, read-only results.
    func inputField() -> some View {
        self.padding(.horizontal, 10).padding(.vertical, 7)
            .background(RoundedRectangle(cornerRadius: 8, style: .continuous).fill(Color(nsColor: .textBackgroundColor)))
            .overlay(RoundedRectangle(cornerRadius: 8, style: .continuous).strokeBorder(Color(nsColor: .separatorColor)))
    }

    /// Page geometry: gutters, capped width, centered. `Page` applies it; History applies it directly (its List scrolls).
    func pageColumn() -> some View {
        self.padding(.horizontal, Metrics.gutter).padding(.top, 8).padding(.bottom, Metrics.gutter)
            .frame(maxWidth: Metrics.column + 2 * Metrics.gutter)
            .frame(maxWidth: .infinity)
    }

    /// A RowGroup child: fills the width and paints the card surface. SettingRow/StackedRow apply it; custom rows apply it themselves.
    func row() -> some View {
        self.padding(.horizontal, Metrics.rowInset).padding(.vertical, 10)
            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
            .background(Color.cardFill)
    }
}

/// Observe the containing window, including closes that retain its SwiftUI hierarchy.
struct WindowVisibility: NSViewRepresentable {
    var changed: (Bool) -> Void

    func makeNSView(context: Context) -> ObserverView {
        let view = ObserverView()
        view.changed = changed
        return view
    }

    func updateNSView(_ view: ObserverView, context: Context) { view.changed = changed }
    static func dismantleNSView(_ view: ObserverView, coordinator: ()) { view.stop() }

    final class ObserverView: NSView {
        var changed: ((Bool) -> Void)?
        private var observers: [NSObjectProtocol] = []
        private var visible = false

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            stop()
            guard let window else { return }
            let center = NotificationCenter.default
            for name in [NSWindow.didBecomeKeyNotification, NSWindow.didChangeOcclusionStateNotification,
                         NSWindow.didMiniaturizeNotification, NSWindow.didDeminiaturizeNotification] {
                observers.append(center.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in self?.refresh() })
            }
            observers.append(center.addObserver(forName: NSWindow.willCloseNotification, object: window, queue: .main) { [weak self] _ in self?.setVisible(false) })
            for name in [NSApplication.didHideNotification, NSApplication.didUnhideNotification] {
                observers.append(center.addObserver(forName: name, object: NSApp, queue: .main) { [weak self] _ in self?.refresh() })
            }
            DispatchQueue.main.async { [weak self] in self?.refresh() }
        }

        private func refresh() {
            setVisible(window.map { $0.isVisible && !$0.isMiniaturized && $0.occlusionState.contains(.visible) && !NSApp.isHidden } ?? false)
        }

        private func setVisible(_ value: Bool) {
            guard visible != value else { return }
            visible = value
            changed?(value)
        }

        func stop() {
            observers.forEach(NotificationCenter.default.removeObserver)
            observers.removeAll()
            setVisible(false)
        }

        deinit { stop() }
    }
}

/// A scrolling settings page: header, then sections, in the shared column.
struct Page<Trailing: View, Content: View>: View {
    let title: LocalizedStringKey
    var subtitle: String?
    @ViewBuilder var trailing: Trailing
    @ViewBuilder var content: Content

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Metrics.gap) {
                PageHeader(title: title, subtitle: subtitle) { trailing }
                content
            }
            .pageColumn()
        }
    }
}

extension Page where Trailing == EmptyView {
    init(title: LocalizedStringKey, subtitle: String? = nil, @ViewBuilder content: () -> Content) {
        self.init(title: title, subtitle: subtitle, trailing: { EmptyView() }, content: content)
    }
}

/// Section: heading (title, optional count, optional trailing control), content, optional one-line footnote.
/// Init convention for SectionBlock / SettingRow / StackedRow: unlabeled title first, then options, then builders.
/// `Page` keeps labeled `title:` to mirror `PageHeader`.
struct SectionBlock<Trailing: View, Content: View>: View {
    let title: String
    let count: Int?
    let footnote: String?
    let trailing: Trailing
    let content: Content

    init(_ title: String, count: Int? = nil, footnote: String? = nil,
         @ViewBuilder trailing: () -> Trailing, @ViewBuilder content: () -> Content) {
        self.title = title; self.count = count; self.footnote = footnote
        self.trailing = trailing(); self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(title).font(.system(size: 15, weight: .semibold))
                if let count { Text(count.formatted()).font(.callout.monospacedDigit()).foregroundStyle(.secondary) }
                Spacer()
                trailing
            }
            content
            if let footnote {
                Text(footnote).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

extension SectionBlock where Trailing == EmptyView {
    init(_ title: String, count: Int? = nil, footnote: String? = nil, @ViewBuilder content: () -> Content) {
        self.init(title, count: count, footnote: footnote, trailing: { EmptyView() }, content: content)
    }
}

/// Rows in one card separated by hairlines. Works with `if` / `ForEach` children: every row paints an opaque cardFill
/// background and the 1pt gaps between them show the separator colour, so no child enumeration is needed
/// (Group(subviews:) is macOS 15).
struct RowGroup<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        VStack(spacing: 1) { content }
            .background(Color(nsColor: .separatorColor))
            .card(padding: 0)
    }
}

/// One setting: title and optional subtitle left, control right. Toggles inside: `.toggleStyle(.switch).labelsHidden()`;
/// pickers: `.labelsHidden().fixedSize()`.
struct SettingRow<Control: View>: View {
    let title: String
    let subtitle: String?
    let tint: Color?   // subtitle colour: .orange for warnings, .red for invalid input
    let control: Control
    @Namespace private var accessibility

    init(_ title: String, subtitle: String? = nil, tint: Color? = nil, @ViewBuilder control: () -> Control) {
        self.title = title; self.subtitle = subtitle; self.tint = tint; self.control = control()
    }

    var body: some View {
        ViewThatFits(in: .horizontal) {
            HStack(spacing: 16) {
                label.frame(minWidth: 180, alignment: .leading)
                Spacer(minLength: 12)
                control.accessibilityLabeledPair(role: .content, id: title, in: accessibility)
            }
            VStack(alignment: .leading, spacing: 10) {
                label
                control.accessibilityLabeledPair(role: .content, id: title, in: accessibility)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .row()
    }

    private var label: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title).accessibilityLabeledPair(role: .label, id: title, in: accessibility)
            if let subtitle { Text(subtitle).font(.callout).foregroundStyle(tint ?? Color.secondary) }
        }
        .fixedSize(horizontal: false, vertical: true)
    }
}

extension SettingRow where Control == EmptyView {
    init(_ title: String, subtitle: String? = nil, tint: Color? = nil) {
        self.init(title, subtitle: subtitle, tint: tint, control: { EmptyView() })
    }
}

/// Title/subtitle above a full-width control (editors, chip flows, test output).
struct StackedRow<Control: View>: View {
    let title: String
    let subtitle: String?
    let control: Control

    init(_ title: String, subtitle: String? = nil, @ViewBuilder control: () -> Control) {
        self.title = title; self.subtitle = subtitle; self.control = control()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                if let subtitle { Text(subtitle).font(.callout).foregroundStyle(.secondary) }
            }
            .fixedSize(horizontal: false, vertical: true)
            control
        }
        .row()
    }
}

/// Inline notice between sections: tinted icon + text, optional action. Not a RowGroup child (it has its own fill);
/// inside a card use SettingRow(tint:) instead.
struct Callout<Action: View>: View {
    let symbol: String
    let color: Color
    let text: String
    @ViewBuilder var action: Action

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: symbol).foregroundStyle(color)
            Text(text).font(.callout).fixedSize(horizontal: false, vertical: true)
            Spacer()
            action
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10, style: .continuous).fill(color.opacity(0.1)))
    }
}

extension Callout where Action == EmptyView {
    init(symbol: String, color: Color, text: String) { self.init(symbol: symbol, color: color, text: text, action: { EmptyView() }) }
}

/// A tinted SF Symbol tile, used as a row or stat icon.
struct IconBadge: View {
    let symbol: String
    let color: Color
    var size: CGFloat = 26

    var body: some View {
        Image(systemName: symbol)
            .font(.system(size: size * 0.5, weight: .semibold))
            .foregroundStyle(color)
            .frame(width: size, height: size)
            .background(RoundedRectangle(cornerRadius: size * 0.28, style: .continuous).fill(color.opacity(0.14)))
    }
}

/// App icons by display name (records keep only the localized name). Resolved once per name.
enum AppIcon {
    private static var cache: [String: NSImage] = [:]

    static func image(_ name: String?) -> NSImage {
        let fallback = NSWorkspace.shared.icon(for: .application)
        guard let name, !name.isEmpty else { return fallback }
        if let c = cache[name] { return c }
        let ws = NSWorkspace.shared
        var icon = ws.runningApplications.first { $0.localizedName == name }?.icon
        if icon == nil {
            let home = FileManager.default.homeDirectoryForCurrentUser.path
            for dir in ["/Applications", "/System/Applications", "/System/Applications/Utilities", home + "/Applications"] {
                let path = "\(dir)/\(name).app"
                if FileManager.default.fileExists(atPath: path) { icon = ws.icon(forFile: path); break }
            }
        }
        cache[name] = icon ?? fallback
        return cache[name]!
    }
}

/// Left-to-right wrapping layout for chips.
struct FlowLayout: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let w = proposal.width.flatMap { $0.isFinite ? $0 : nil }
        let rows = arrange(subviews, width: w ?? .infinity)
        return CGSize(width: w ?? rows.width, height: rows.height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        for (s, p) in zip(subviews, arrange(subviews, width: bounds.width).points) {
            s.place(at: CGPoint(x: bounds.minX + p.x, y: bounds.minY + p.y), proposal: .unspecified)
        }
    }

    private func arrange(_ subviews: Subviews, width: CGFloat) -> (points: [CGPoint], width: CGFloat, height: CGFloat) {
        var points: [CGPoint] = [], x: CGFloat = 0, y: CGFloat = 0, row: CGFloat = 0, maxX: CGFloat = 0
        for s in subviews {
            let z = s.sizeThatFits(.unspecified)
            if x > 0, x + z.width > width {
                x = 0
                y += row + spacing
                row = 0
            }
            points.append(CGPoint(x: x, y: y))
            x += z.width + spacing
            maxX = max(maxX, x - spacing)
            row = max(row, z.height)
        }
        return (points, maxX, y + row)
    }
}

/// "2 小时 5 分", "12 分钟", "45 秒".
func duration(_ ms: Int) -> String {
    let s = ms / 1000
    if s >= 3600 { return "\(s / 3600) 小时 \(s % 3600 / 60) 分" }
    if s >= 60 { return "\(s / 60) 分钟" }
    return "\(s) 秒"
}

func ms(_ v: Int?) -> String { v.map { "\($0) ms" } ?? "–" }
