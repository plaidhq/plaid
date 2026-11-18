import AppKit
import SwiftUI

/// The single settings window, reused across opens.
enum SettingsWindow {
    private static let window: NSWindow = {
        let w = NSWindow(contentViewController: NSHostingController(rootView: SettingsView()))
        w.title = "Plaid 设置"
        w.styleMask = [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView]
        w.titlebarAppearsTransparent = true
        w.titleVisibility = .hidden
        w.isReleasedWhenClosed = false
        // Open on the Space / Stage the user is on instead of switching back to where the window was last shown.
        w.collectionBehavior = [.moveToActiveSpace]
        w.center()
        // A KeyRecorder left capturing would keep the hotkeys off (and grab the next key Plaid sees) after the user left.
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: w, queue: .main) { _ in
            DictionaryDraft.shared.flush()
            NSApp.setActivationPolicy(.accessory)
        }
        for name in [NSWindow.willCloseNotification, NSApplication.didResignActiveNotification] {
            NotificationCenter.default.addObserver(forName: name, object: name == NSWindow.willCloseNotification ? w : nil, queue: .main) { _ in
                Hotkeys.stopRecording?()
            }
        }
        return w
    }()

    static func show(_ page: SettingsPage? = nil) {
        if let page { SettingsNav.shared.page = page }
        if ProcessInfo.processInfo.environment["PLAID_SNAPSHOT_DARK"] != nil { NSApp.appearance = NSAppearance(named: .darkAqua) }
        // An LSUIElement app's activation request can be refused (macOS 14+ cooperative activation), leaving the window
        // behind the frontmost app. Be a regular app while the window is open (Dock icon, Cmd-Tab); accessory again on close.
        NSApp.setActivationPolicy(.regular)
        if window.isMiniaturized { window.deminiaturize(nil) }
        window.makeKeyAndOrderFront(nil)
        // Activate after the policy change has been processed.
        DispatchQueue.main.async {
            NSApp.activate(ignoringOtherApps: true)
            window.makeKeyAndOrderFront(nil)
        }
        // Record whether activation took, for the next "settings stayed behind" report.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            let front = NSWorkspace.shared.frontmostApplication?.localizedName ?? "?"
            Trace.log("settings shown active=\(NSApp.isActive) key=\(window.isKeyWindow) front=\(front)")
        }
        // Don't land in the first text field (select-all highlight) when the window opens on a page with inputs.
        DispatchQueue.main.async { window.makeFirstResponder(nil) }
        // PLAID_SNAPSHOT=out.png: render the window to a PNG after layout (screenshots need Screen Recording, this doesn't).
        if let out = ProcessInfo.processInfo.environment["PLAID_SNAPSHOT"], let v = window.contentView {
            let width = ProcessInfo.processInfo.environment["PLAID_SNAPSHOT_WIDTH"].flatMap(Double.init) ?? 940
            if let h = ProcessInfo.processInfo.environment["PLAID_SNAPSHOT_HEIGHT"].flatMap(Double.init) { window.setContentSize(.init(width: width, height: h)) }
            let delay = ProcessInfo.processInfo.environment["PLAID_SNAPSHOT_DELAY"].flatMap(Double.init) ?? 1.5
            DispatchQueue.main.asyncAfter(deadline: .now() + delay) {
                guard let rep = v.bitmapImageRepForCachingDisplay(in: v.bounds) else { return }
                v.cacheDisplay(in: v.bounds, to: rep)
                try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: out))
            }
        }
    }
}

enum SettingsPage: String, CaseIterable, Identifiable {
    case overview, history, dictionary, refine, actions, perf, general
    var id: Self { self }

    var title: LocalizedStringKey {
        switch self {
        case .overview: "概览"
        case .history: "历史"
        case .dictionary: "词典"
        case .refine: "优化"
        case .actions: "命令"
        case .perf: "性能"
        case .general: "设置"
        }
    }

    var icon: String {
        switch self {
        case .overview: "chart.bar.xaxis"
        case .history: "clock.arrow.circlepath"
        case .dictionary: "character.book.closed"
        case .refine: "sparkles"
        case .actions: "terminal"
        case .perf: "gauge.with.dots.needle.33percent"
        case .general: "gearshape"
        }
    }
}

/// Sidebar selection, shared so callers can open the window on a page. PLAID_SNAPSHOT_PAGE=<rawValue> seeds it.
final class SettingsNav: ObservableObject {
    static let shared = SettingsNav()
    @Published var page: SettingsPage? = SettingsPage(rawValue: ProcessInfo.processInfo.environment["PLAID_SNAPSHOT_PAGE"] ?? "") ?? .overview
}

struct SettingsView: View {
    @ObservedObject private var nav = SettingsNav.shared
    @ObservedObject private var records = Records.shared
    @AppStorage("dismissedFixes") private var dismissed = ""

    var body: some View {
        NavigationSplitView {
            List(selection: $nav.page) {
                Section {
                    ForEach(SettingsPage.allCases) { p in
                        Label(p.title, systemImage: p.icon).tag(p)
                            .badge(p == .dictionary ? pendingFixes(records, dismissed: dismissed).count : 0)
                    }
                } header: {
                    HStack(spacing: 8) {
                        Image(nsImage: NSApp.applicationIconImage).resizable().frame(width: 28, height: 28)
                        VStack(alignment: .leading, spacing: 0) {
                            Text("Plaid").font(.headline).foregroundStyle(.primary)
                            Text("v" + (Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "dev"))
                                .font(.caption).foregroundStyle(.secondary)
                        }
                    }
                    .padding(.bottom, 10)
                }
                .collapsible(false)
            }
            .navigationSplitViewColumnWidth(180)
            .toolbar(removing: .sidebarToggle)
        } detail: {
            Group {
                switch nav.page ?? .overview {
                case .overview: OverviewPage()
                case .history: HistoryPage()
                case .dictionary: DictionaryPage()
                case .refine: RefinePage()
                case .actions: ActionsPage()
                case .perf: PerfPage()
                case .general: GeneralPage()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(Color(nsColor: .windowBackgroundColor))
        }
        .frame(minWidth: 900, minHeight: 640)
    }
}
