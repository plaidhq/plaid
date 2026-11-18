import SwiftUI

/// Pending suggestions, editable dictionary entries, automatic rules, and the two learning switches.
struct DictionaryPage: View {
    @ObservedObject private var records = Records.shared
    @AppStorage("dismissedFixes") private var dismissed = ""
    @AppStorage("autoLearn") private var autoLearn = "1"
    @AppStorage("learnEdits") private var learnEdits = "1"
    @ObservedObject private var editor = DictionaryDraft.shared
    private var text: String {
        get { editor.text }
        nonmutating set { editor.text = newValue }
    }
    @State private var raw = false
    @State private var query = ""
    @State private var editing: DictionaryEdit?
    @State private var confirmIgnore = false

    var body: some View {
        let auto = records.autoRules()
        let fixes = pendingFixes(records, dismissed: dismissed)
        Page(title: "词典", subtitle: "热词让识别更准；建议来自你的手动修改和 LLM 校对") {
            if let error = editor.error {
                Callout(symbol: "exclamationmark.triangle", color: .orange, text: "热词保存失败：\(error)。草稿已保留。") {
                    Button("重试") { editor.flush() }
                }
            }
            if !fixes.isEmpty { suggestions(fixes) }
            hotwords
            if !auto.isEmpty { autoRules(auto) }
            learning
        }
        .onAppear { editor.reload() }
        .onDisappear { editor.flush() }
        .sheet(item: $editing) { entry in
            DictionaryEntryForm(entry: entry) { line in save(line, editing: entry) }
        }
        .confirmationDialog("忽略全部 \(fixes.count) 条待处理建议？", isPresented: $confirmIgnore) {
            Button("全部忽略", role: .destructive) {
                dismissed += fixes.map { $0.from + " -> " + $0.to + "\n" }.joined()
            }
        } message: {
            Text("这些建议将不再出现在待处理列表中。已有热词和替换规则不会删除。")
        }
    }

    // MARK: Suggestions

    private func suggestions(_ fixes: [Fix]) -> some View {
        SectionBlock("待处理建议", count: fixes.count, footnote: "别名会将读音相近的错法改为目标词；最近 10 条建议也会作为 {{corrections}} 交给优化。", trailing: {
            if fixes.count > 1 {
                Menu("批量处理") {
                    Button("全部加为热词") { accept(fixes, asAliases: false) }
                    Button("全部加为别名") { accept(fixes, asAliases: true) }
                    Divider()
                    Button("全部忽略", role: .destructive) { confirmIgnore = true }
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
            }
        }) {
            RowGroup {
                ForEach(fixes) { f in
                    HStack(alignment: .center, spacing: 12) {
                        VStack(alignment: .leading, spacing: 3) {
                            HStack(spacing: 8) {
                                Text(f.from).strikethrough().foregroundStyle(.secondary)
                                Image(systemName: "arrow.right").font(.caption.weight(.bold)).foregroundStyle(.green)
                                Text(f.to).fontWeight(.medium)
                                Text("\(f.count) 次 · \(f.last.formatted(.dateTime.month().day()))").font(.caption).foregroundStyle(.tertiary)
                            }
                            // context is the whole edited field; it can start with blank lines
                            let ctx = f.context.split(whereSeparator: \.isNewline).joined(separator: " ").trimmingCharacters(in: .whitespaces)
                            if !ctx.isEmpty { Text(ctx).font(.callout).foregroundStyle(.secondary).lineLimit(1).help(f.context) }
                        }
                        Spacer()
                        Button("加热词") { editor.append(f.to) }.disabled(text.components(separatedBy: "\n").contains(f.to))
                        Button("加别名") { editor.append(f.to + " | " + f.from) }
                            .help("把「\(f.from)」作为「\(f.to)」的读音别名")
                        Button { dismissed += f.from + " -> " + f.to + "\n" } label: { Image(systemName: "xmark") }
                            .buttonStyle(.borderless).help("忽略").accessibilityLabel("忽略建议：\(f.from) → \(f.to)")
                    }
                    .controlSize(.small)
                    .row()
                }
            }
        }
    }

    private func accept(_ fixes: [Fix], asAliases: Bool) {
        var existing = Set(text.components(separatedBy: "\n").map { $0.trimmingCharacters(in: .whitespaces) })
        let additions = fixes.map { asAliases ? $0.to + " | " + $0.from : $0.to }
            .filter { existing.insert($0).inserted }
        guard !additions.isEmpty else { return }
        text += (text.isEmpty || text.hasSuffix("\n") ? "" : "\n") + additions.joined(separator: "\n") + "\n"
        editor.flush()
    }

    // MARK: Dictionary entries (each entry is one original line; unrelated lines and comments are never rewritten)

    private var hotwords: some View {
        let lines = text.components(separatedBy: "\n")
        let entries = lines.indices.filter {
            let line = lines[$0].trimmingCharacters(in: .whitespaces)
            return !line.isEmpty && !line.hasPrefix("#")
        }
        let filtered = entries.filter { query.isEmpty || lines[$0].localizedCaseInsensitiveContains(query) }
        return SectionBlock("热词与替换", count: entries.count,
                            footnote: "热词用于识别和优化；替换规则按原文匹配，不区分大小写。改动自动保存，下次录音生效。",
                            trailing: {
            HStack(spacing: 12) {
                Button(raw ? "完成" : "编辑原文") { raw.toggle() }
                if !raw {
                    Button { editing = DictionaryEdit() } label: { Label("添加", systemImage: "plus") }
                }
            }
        }) {
            if raw {
                VStack(alignment: .leading, spacing: 10) {
                    TextEditor(text: $editor.text)
                        .font(.body.monospaced())
                        .scrollContentBackground(.hidden)
                        .frame(minHeight: 320)
                        .inputField()
                    legend
                }
                .card()
            } else if entries.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    Label("还没有词典条目", systemImage: "text.book.closed").font(.headline)
                    Text("添加人名、项目名和术语，或将常见错法设为替换规则。")
                        .font(.callout).foregroundStyle(.secondary)
                    Button("添加第一个条目") { editing = DictionaryEdit() }
                        .padding(.top, 4)
                }
                .card()
            } else {
                VStack(alignment: .leading, spacing: 10) {
                    HStack(spacing: 6) {
                        Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                        TextField("搜索热词、错法或替换内容", text: $query).textFieldStyle(.plain)
                            .accessibilityLabel("搜索词典")
                        if !query.isEmpty {
                            Text("\(filtered.count) 条").font(.caption).foregroundStyle(.secondary)
                            Button { query = "" } label: { Image(systemName: "xmark.circle.fill") }
                                .buttonStyle(.plain).foregroundStyle(.secondary).accessibilityLabel("清除搜索")
                        }
                    }
                    .inputField()
                    if filtered.isEmpty {
                        Text("没有匹配的条目，试试其他关键词。")
                            .foregroundStyle(.secondary).card()
                    } else {
                        RowGroup {
                            ForEach(filtered, id: \.self) { i in
                                DictionaryEntryRow(line: lines[i], edit: {
                                    editing = DictionaryEdit(index: i, original: lines[i])
                                }, remove: { remove(i) })
                                .row()
                            }
                        }
                    }
                }
            }
        }
    }

    /// hotwords.txt syntax, shown only next to the raw editor.
    private var legend: some View {
        VStack(alignment: .leading, spacing: 6) {
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 4) {
                GridRow {
                    Text("想要的 | 常见错法 | 错法 ~~~ 排除词").monospaced()
                    Text("别名：读音相近的片段改成想要的；附近出现排除词时不改")
                }
                GridRow {
                    Text("识别成 -> 想要的").monospaced()
                    Text("原文匹配，不区分大小写；右侧留空表示删除")
                }
                GridRow {
                    Text("# 注释").monospaced()
                    Text("不发送")
                }
            }
            Text("最多约 100 token 发给语音识别。")
        }
        .font(.callout).foregroundStyle(.secondary)
    }

    private func save(_ line: String, editing entry: DictionaryEdit) -> String? {
        var lines = text.components(separatedBy: "\n")
        if let index = entry.index {
            guard lines.indices.contains(index), lines[index] == entry.original else {
                return "原文已发生变化。请取消后重新打开这个条目。"
            }
            if line == entry.original { return nil }
        }
        guard !lines.indices.contains(where: {
            $0 != entry.index && lines[$0].trimmingCharacters(in: .whitespaces) == line.trimmingCharacters(in: .whitespaces)
        }) else { return "已存在相同的条目。" }
        if let index = entry.index {
            lines[index] = line
            text = lines.joined(separator: "\n")
            editor.flush()
        } else {
            editor.append(line)
        }
        return nil
    }

    private func remove(_ i: Int) {
        var lines = text.components(separatedBy: "\n")
        guard lines.indices.contains(i) else { return }
        lines.remove(at: i)
        text = lines.joined(separator: "\n")
    }

    // MARK: Auto rules

    private func autoRules(_ auto: [(from: String, to: String, count: Int)]) -> some View {
        SectionBlock("自动规则", count: auto.count, footnote: "同一个改法出现 2 次以上后自动生效，存放在 hotwords.auto.txt；忽略后不再出现。") {
            RowGroup {
                ForEach(auto, id: \.from) { r in
                    HStack(spacing: 8) {
                        Text(r.from).foregroundStyle(.secondary)
                        Image(systemName: "arrow.right").font(.caption.weight(.bold)).foregroundStyle(.purple)
                        Text(r.to).fontWeight(.medium)
                        Text("\(r.count) 次").font(.caption).foregroundStyle(.tertiary)
                        Spacer()
                        Button { dismissed += r.from + " -> " + r.to + "\n"; records.writeAutoRules() } label: { Image(systemName: "xmark") }
                            .buttonStyle(.borderless).help("忽略").accessibilityLabel("忽略规则：\(r.from) → \(r.to)")
                    }
                    .row()
                }
            }
        }
    }

    // MARK: Learning

    private var learning: some View {
        let noLLM = autoLearn != "0" && Refine.learner == nil
        return SectionBlock("学习") {
            RowGroup {
                SettingRow("从手动修改中学习", subtitle: "通过辅助功能读取你改过的输入框，和粘贴内容对比得出建议") {
                    Toggle("从手动修改中学习", isOn: Binding(get: { learnEdits != "0" }, set: { learnEdits = $0 ? "1" : "0" }))
                        .toggleStyle(.switch).labelsHidden()
                }
                SettingRow("后台校对",
                           subtitle: noLLM ? "没有可用的 LLM：在「优化」里填好接口和 key" : "独立于优化开关：优化关闭时，仍将每句文本发送到配置的 LLM 校对，只用来发现规则，不改粘贴结果",
                           tint: noLLM ? .orange : nil) {
                    HStack(spacing: 12) {
                        if noLLM { Button("打开优化") { SettingsNav.shared.page = .refine } }
                        Toggle("后台校对", isOn: Binding(get: { autoLearn != "0" }, set: { autoLearn = $0 ? "1" : "0" }))
                            .toggleStyle(.switch).labelsHidden()
                    }
                }
            }
        }
    }
}

private struct DictionaryEntryRow: View {
    let line: String
    let edit: () -> Void
    let remove: () -> Void

    var body: some View {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        let replacement = trimmed.components(separatedBy: "->").map { $0.trimmingCharacters(in: .whitespaces) }
        let parts = trimmed.components(separatedBy: "~~~")
        let words = parts[0].components(separatedBy: "|").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        let blocks = parts.count > 1 ? parts[1].components(separatedBy: "|").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty } : []
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                if replacement.count == 2, !replacement[0].isEmpty {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(replacement[0]).foregroundStyle(.secondary)
                        Image(systemName: "arrow.right").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                        Text(replacement[1].isEmpty ? "（删除）" : replacement[1]).fontWeight(.medium)
                    }
                    Text("原样替换 · 不区分大小写").font(.caption).foregroundStyle(.secondary)
                } else if !trimmed.contains("->"), let target = words.first {
                    if words.count > 1 {
                        HStack(alignment: .firstTextBaseline, spacing: 8) {
                            Text(words.dropFirst().joined(separator: " / ")).foregroundStyle(.secondary)
                            Image(systemName: "arrow.right").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                            Text(target).fontWeight(.medium)
                        }
                        Text("读音别名").font(.caption).foregroundStyle(.secondary)
                    } else {
                        Text(target).fontWeight(.medium)
                        Text("热词").font(.caption).foregroundStyle(.secondary)
                    }
                    if !blocks.isEmpty {
                        Text("附近出现这些词时不改：" + blocks.joined(separator: "、"))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                } else {
                    Text(trimmed).monospaced()
                    Text("高级原文").font(.caption).foregroundStyle(.secondary)
                }
            }
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .leading)
            Button(action: edit) { Image(systemName: "square.and.pencil") }
                .help("编辑条目").accessibilityLabel("编辑条目：\(trimmed)")
            Button(action: remove) { Image(systemName: "xmark") }
                .help("删除这一行").accessibilityLabel("删除条目：\(trimmed)")
        }
        .buttonStyle(.borderless)
    }
}

private struct DictionaryEdit: Identifiable {
    let id = UUID()
    var index: Int? = nil
    var original = ""
}

private enum DictionaryEntryKind: String, CaseIterable {
    case hotword = "热词"
    case replacement = "替换规则"
    case advanced = "高级原文"
}

private struct DictionaryEntryForm: View {
    let entry: DictionaryEdit
    let save: (String) -> String?
    @Environment(\.dismiss) private var dismiss
    @State private var kind: DictionaryEntryKind
    @State private var word: String
    @State private var from: String
    @State private var to: String
    @State private var rawLine: String
    @State private var error: String?

    init(entry: DictionaryEdit, save: @escaping (String) -> String?) {
        self.entry = entry
        self.save = save
        let line = entry.original.trimmingCharacters(in: .whitespaces)
        let parts = line.components(separatedBy: "->").map { $0.trimmingCharacters(in: .whitespaces) }
        let replacement = parts.count == 2 && !parts[0].isEmpty
        let advanced = line.contains("->") || line.contains("|") || line.contains("~~~") || line.hasPrefix("#")
        _kind = State(initialValue: replacement ? .replacement : advanced ? .advanced : .hotword)
        _word = State(initialValue: advanced ? "" : line)
        _from = State(initialValue: replacement ? parts[0] : "")
        _to = State(initialValue: replacement ? parts[1] : "")
        _rawLine = State(initialValue: entry.original)
    }

    private var line: String {
        switch kind {
        case .hotword: return word.trimmingCharacters(in: .whitespaces)
        case .replacement: return from.trimmingCharacters(in: .whitespaces) + " -> " + to.trimmingCharacters(in: .whitespaces)
        case .advanced: return rawLine
        }
    }

    private var validation: String? {
        switch kind {
        case .hotword:
            if word.contains(where: \.isNewline) { return "每个条目只能有一行。" }
            if line.isEmpty { return "请输入想要识别的词。" }
            if line.hasPrefix("#") || line.contains("->") || line.contains("|") || line.contains("~~~") {
                return "替换请选「替换规则」；别名、排除词和注释请用「高级原文」。"
            }
        case .replacement:
            if from.contains(where: \.isNewline) || to.contains(where: \.isNewline) { return "原文和替换内容都只能有一行。" }
            if from.trimmingCharacters(in: .whitespaces).isEmpty { return "请输入要匹配的原文。" }
            if from.trimmingCharacters(in: .whitespaces).hasPrefix("#") || from.contains("->") || to.contains("->") {
                return "这些字符会改变规则语法，请改用「高级原文」。"
            }
        case .advanced:
            if rawLine.contains(where: \.isNewline) { return "这里只编辑一行，多行内容请用词典页的「编辑原文」。" }
            if rawLine.trimmingCharacters(in: .whitespaces).isEmpty { return "请输入一行内容。" }
        }
        return nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text(entry.index == nil ? "添加词典条目" : "编辑词典条目").font(.title2.weight(.semibold))
            Picker("条目类型", selection: $kind) {
                ForEach(DictionaryEntryKind.allCases, id: \.self) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented).labelsHidden()
            VStack(alignment: .leading, spacing: 12) {
                switch kind {
                case .hotword:
                    VStack(alignment: .leading, spacing: 6) {
                        Text("目标词").font(.callout.weight(.medium))
                        TextField("例如：Plaid、张小明", text: $word).textFieldStyle(.roundedBorder)
                            .accessibilityLabel("目标词")
                    }
                    Text("用于语音识别和优化，也参与读音相近的纠错。")
                        .font(.callout).foregroundStyle(.secondary)
                case .replacement:
                    VStack(alignment: .leading, spacing: 6) {
                        Text("识别到的原文").font(.callout.weight(.medium))
                        TextField("例如：克劳德", text: $from).textFieldStyle(.roundedBorder)
                            .accessibilityLabel("识别到的原文")
                    }
                    VStack(alignment: .leading, spacing: 6) {
                        Text("替换为").font(.callout.weight(.medium))
                        TextField("例如：Claude；留空表示删除", text: $to).textFieldStyle(.roundedBorder)
                            .accessibilityLabel("替换为")
                    }
                    Text("不区分大小写，替换文本中所有匹配的原文；不是读音匹配。")
                        .font(.callout).foregroundStyle(.secondary)
                    if !from.trimmingCharacters(in: .whitespaces).isEmpty, to.trimmingCharacters(in: .whitespaces).isEmpty {
                        Label("替换内容为空，匹配到的原文会被删除。", systemImage: "exclamationmark.triangle")
                            .font(.callout).foregroundStyle(.orange)
                    }
                case .advanced:
                    TextField("目标词 | 别名 ~~~ 排除词", text: $rawLine)
                        .font(.body.monospaced()).textFieldStyle(.roundedBorder)
                        .accessibilityLabel("词典单行原文")
                    Text("保留单行语法。别名示例：Claude | 克劳德；排除词示例：Plaid | 普莱德 ~~~ 格纹。多行和注释也可在词典页的「编辑原文」中修改。")
                        .font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
            }
            if let error {
                Text(error).font(.callout).foregroundStyle(.orange).fixedSize(horizontal: false, vertical: true)
            } else if let validation {
                Text(validation).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            Divider()
            HStack {
                Spacer()
                Button("取消") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("保存") {
                    error = save(line)
                    if error == nil { dismiss() }
                }
                .buttonStyle(.borderedProminent).keyboardShortcut(.defaultAction)
                .disabled(validation != nil)
            }
        }
        .padding(24)
        .frame(width: 520)
        .onChange(of: line) { error = nil }
    }
}

/// Keeps unsaved edits across page switches, including a failed write. All access is on the main thread.
final class DictionaryDraft: ObservableObject {
    static let shared = DictionaryDraft()
    @Published var text: String { didSet { schedule() } }
    @Published private(set) var error: String?
    private var saved: String
    private var pending: DispatchWorkItem?
    private var writing: (id: UUID, text: String)?

    private init() {
        let initial = (try? String(contentsOf: Hotwords.url, encoding: .utf8)) ?? ""
        text = initial
        saved = initial
    }

    func reload() {
        guard text == saved, writing == nil else { return }
        let current = (try? String(contentsOf: Hotwords.url, encoding: .utf8)) ?? ""
        saved = current
        text = current
    }

    func append(_ line: String) {
        guard !text.components(separatedBy: "\n").contains(line) else { return }
        text += (text.isEmpty || text.hasSuffix("\n") ? "" : "\n") + line + "\n"
        flush()
    }

    private func schedule() {
        pending?.cancel()
        guard text != saved else { return }
        let work = DispatchWorkItem { [weak self] in self?.flush() }
        pending = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5, execute: work)
    }

    /// Enqueue the latest draft immediately; disk I/O stays on Records' ordered queue.
    func flush() {
        pending?.cancel()
        pending = nil
        guard (text != saved || writing != nil), writing?.text != text else { return }
        let snapshot = text, id = UUID()
        writing = (id, snapshot)
        Records.saveHotwords(snapshot) { result in
            if self.writing?.id == id { self.writing = nil }
            switch result {
            case .success:
                self.saved = snapshot
                self.error = nil
                Records.shared.writeAutoRules()
                if self.writing == nil, self.text != self.saved { self.schedule() }
            case .failure(let failure):
                self.error = failure.localizedDescription
            }
        }
    }
}

/// Edit suggestions not yet accepted, dismissed, or covered by a hotword or auto rule.
func pendingFixes(_ records: Records, dismissed: String) -> [Fix] {
    let auto = records.autoRules(), entries = Hotwords.entries()
    let gone = Set(dismissed.split(separator: "\n").map(String.init))
    return records.fixes().map { Fix(from: $0.from, to: $0.to, count: $0.count, last: $0.last, context: $0.context) }.filter { f in
        !gone.contains(f.from + " -> " + f.to) && !Hotwords.contains(f.from + " -> " + f.to) && !auto.contains { $0.from == f.from && $0.to == f.to }
            && !entries.contains { $0.target == f.to && $0.aliases.contains(f.from) }
    }
}

struct Fix: Identifiable {
    let from: String, to: String, count: Int, last: Date, context: String
    var id: String { from + "\u{0}" + to }
}
