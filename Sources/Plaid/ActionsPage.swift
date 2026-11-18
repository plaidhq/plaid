import AppKit
import SwiftUI

struct ActionsPage: View {
    @ObservedObject private var store = Actions.shared
    @AppStorage("commandMode") private var mode = "wake"
    @AppStorage("wakeWord") private var wakeWord = "Plaid"

    var body: some View {
        Page(title: "命令", subtitle: "说出命令名执行自定义 shell 命令，代替粘贴") {
            Button("打开 actions.json") {
                if !FileManager.default.fileExists(atPath: Actions.url.path) {
                    try? FileManager.default.createDirectory(at: Actions.url.deletingLastPathComponent(), withIntermediateDirectories: true)
                    try? Data("[]\n".utf8).write(to: Actions.url)
                }
                NSWorkspace.shared.open(Actions.url)
            }
        } content: {
            if let e = store.loadError {
                Callout(symbol: "xmark.octagon.fill", color: .red, text: "actions.json 解析失败，修好文件前这里只读：\(e)")
            }
            SectionBlock("触发") {
                RowGroup {
                    SettingRow("触发方式", subtitle: modeSubtitle, tint: mode == "bare" ? .orange : nil) {
                        Picker("", selection: $mode) {
                            Text("唤醒词").tag("wake")
                            Text("直接说命令名").tag("bare")
                        }
                        .pickerStyle(.segmented).labelsHidden().fixedSize()
                    }
                    if mode != "bare" {
                        SettingRow("唤醒词", subtitle: wakeWord.trimmingCharacters(in: .whitespaces).isEmpty ? "为空时等同直接说命令名" : "例如「\(wakeWord)，搜索 Swift 并发」") {
                            TextField("", text: $wakeWord, prompt: Text("Plaid")).textFieldStyle(.roundedBorder).frame(width: 200)
                        }
                    }
                }
            }
            SectionBlock("命令", count: store.list.count,
                         footnote: "命令用 /bin/zsh -c 执行，PATH 取自登录 shell，超时 \(Int(Actions.timeout)) 秒。环境变量：PLAID_ARGS（命令名之后的内容，同时写入 stdin）、PLAID_TEXT（整句）、PLAID_APP、PLAID_BUNDLE、PLAID_WINDOW、PLAID_BEFORE（光标前文本）。引用时加双引号，如 \"$PLAID_ARGS\"；AppleScript 里别用 system attribute 读中文（编码会乱），改为 osascript … \"$PLAID_ARGS\" 经 argv 传入。") {
                Menu("添加") {
                    Button("空白命令") { store.list.append(Action()) }
                    Divider()
                    ForEach(Actions.examples, id: \.name) { ex in
                        Button("示例：\(ex.name)") { var a = ex; a.id = UUID(); store.list.append(a) }
                    }
                }
                .fixedSize()
                .disabled(store.loadError != nil)
            } content: {
                if store.list.isEmpty {
                    Text("还没有命令。从「添加」里选一个示例开始。").foregroundStyle(.secondary).card()
                }
                ForEach($store.list) { $a in
                    ActionCard(action: $a, conflicts: conflicts(a)) { store.list.removeAll { $0.id == a.id } }
                }
                .disabled(store.loadError != nil)
            }
        }
        .onAppear { store.reload() }
    }

    private var modeSubtitle: String {
        mode == "bare"
            ? "以命令名开头的句子都会执行。口述内容碰巧以命令名开头也会触发，例如「搜索引擎优化……」会执行「搜索」"
            : "只有以唤醒词开头的句子当作命令；唤醒词后没有匹配的命令时提示，不粘贴"
    }

    /// Other actions sharing one of `a`'s triggers (case-insensitive).
    private func conflicts(_ a: Action) -> [String] {
        let mine = Set(a.triggers.map { $0.lowercased() })
        return store.list.filter { $0.id != a.id && !mine.isDisjoint(with: $0.triggers.map { $0.lowercased() }) }.map(\.name)
    }
}

private struct ActionCard: View {
    @Binding var action: Action
    let conflicts: [String]
    let delete: () -> Void
    @ObservedObject private var store = Actions.shared
    @State private var aliases = ""
    @State private var sample = ""

    var body: some View {
        RowGroup {
            HStack(spacing: 10) {
                TextField("", text: $action.name, prompt: Text("命令名")).textFieldStyle(.roundedBorder).frame(width: 160)
                TextField("", text: $aliases, prompt: Text("别名，用逗号分隔")).textFieldStyle(.roundedBorder)
                    .onChange(of: aliases) { _, v in
                        let parsed = v.split(whereSeparator: { ",，、".contains($0) }).map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
                        if parsed != action.aliases { action.aliases = parsed }
                    }
                Button(role: .destructive, action: delete) { Image(systemName: "trash") }.buttonStyle(.borderless).help("删除")
            }
            .row()
            if let warning {
                Label(warning, systemImage: "exclamationmark.triangle.fill").font(.callout).foregroundStyle(.orange).row()
            }
            StackedRow("命令") {
                CodeEditor(text: $action.run).frame(height: 88).inputField()
            }
            SettingRow("输出") {
                Picker("", selection: $action.output) {
                    Text("不处理").tag(Action.Output.none)
                    Text("HUD 显示").tag(Action.Output.hud)
                    Text("粘贴到光标").tag(Action.Output.paste)
                }
                .pickerStyle(.segmented).labelsHidden().fixedSize()
            }
            SettingRow("执行前确认", subtitle: "识别后按 ⏎ 才执行，esc 取消") {
                Toggle("", isOn: $action.confirm).toggleStyle(.switch).labelsHidden()
            }
            testRow
        }
        .onAppear { aliases = action.aliases.joined(separator: ", ") }
    }

    private var warning: String? {
        if action.name.trimmingCharacters(in: .whitespaces).isEmpty || action.run.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return "命令名或命令为空，不会触发"
        }
        return conflicts.isEmpty ? nil : "触发词与「\(conflicts.joined(separator: "」「"))」重复，较长的先匹配，相同时取靠前的"
    }

    private var testRow: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                TextField("", text: $sample, prompt: Text("参数（PLAID_ARGS）")).textFieldStyle(.roundedBorder)
                Button(store.running.contains(action.id) ? "运行中…" : "试运行") {
                    let text = [action.name, sample].filter { !$0.isEmpty }.joined(separator: " ")
                    let env = Actions.env(text: text, args: sample, app: "Plaid", bundle: Bundle.main.bundleIdentifier, window: "Plaid 设置", before: nil)
                    store.run(action, args: sample, env: env) { _ in }
                }
                .disabled(store.running.contains(action.id) || action.run.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            if let r = store.lastRun[action.id] {
                Text(r.ok ? "完成 · \(r.ms) ms" + (action.output == .paste ? " · 试运行不粘贴" : "") : "失败 · \(r.failure) · \(r.ms) ms")
                    .font(.callout.monospacedDigit()).foregroundStyle(r.ok ? Color.secondary : Color.red)
                let shown = [r.out, r.err].map { $0.trimmingCharacters(in: .newlines) }.filter { !$0.isEmpty }.joined(separator: "\n")
                if !shown.isEmpty {
                    Text(shown).font(.callout.monospaced()).textSelection(.enabled).lineLimit(12)
                        .frame(maxWidth: .infinity, alignment: .leading).inputField()
                }
            }
        }
        .row()
    }
}

/// Plain-text editor for shell commands: TextEditor inherits smart quotes/dashes (on by default), which turn ' into ’ and -- into —.
private struct CodeEditor: NSViewRepresentable {
    @Binding var text: String

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSTextView.scrollableTextView()
        scroll.drawsBackground = false
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        let tv = scroll.documentView as! NSTextView
        tv.isRichText = false
        tv.drawsBackground = false
        tv.font = .monospacedSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
        tv.textContainerInset = .zero
        tv.textContainer?.lineFragmentPadding = 0
        tv.isAutomaticQuoteSubstitutionEnabled = false
        tv.isAutomaticDashSubstitutionEnabled = false
        tv.isAutomaticTextReplacementEnabled = false
        tv.isAutomaticSpellingCorrectionEnabled = false
        tv.isContinuousSpellCheckingEnabled = false
        tv.allowsUndo = true
        tv.string = text
        tv.delegate = context.coordinator
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.text = $text
        let tv = scroll.documentView as! NSTextView
        if tv.string != text { tv.string = text }
    }

    func makeCoordinator() -> Coordinator { Coordinator(text: $text) }

    final class Coordinator: NSObject, NSTextViewDelegate {
        var text: Binding<String>
        init(text: Binding<String>) { self.text = text }
        func textDidChange(_ n: Notification) {
            guard let tv = n.object as? NSTextView else { return }
            text.wrappedValue = tv.string
        }
    }
}
