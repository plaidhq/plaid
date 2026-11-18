import AppKit
import SwiftUI

/// Dictation records, newest first, grouped by day. A single selected row reveals recovery and details.
struct HistoryPage: View {
    @ObservedObject private var records = Records.shared
    @ObservedObject private var recovery = Recovery.shared
    @State private var exporting = false
    @State private var selection = Set<Record.ID>()
    @State private var query = ""
    @State private var filter = HistoryFilter.all
    @AppStorage("historyLatency") private var latency = false
    @State private var confirmClear = false
    @State private var exportError: String?

    private struct DayGroup: Identifiable {
        let day: Date
        let rows: [Record]
        var id: Date { day }
    }

    private enum HistoryFilter: CaseIterable, Hashable {
        case all, saved, failed

        var title: String {
            switch self {
            case .all: "全部"
            case .saved: "未粘贴"
            case .failed: "失败"
            }
        }

        func includes(_ record: Record) -> Bool {
            switch self {
            case .all: true
            case .saved: record.outcome == "saved"
            case .failed: record.isFailure
            }
        }
    }

    var body: some View {
        let cal = Calendar.current
        let matching = records.all.reversed().filter {
            query.isEmpty || $0.text.localizedCaseInsensitiveContains(query) || $0.raw.localizedCaseInsensitiveContains(query)
                || ($0.app ?? "").localizedCaseInsensitiveContains(query)
        }
        let rows = matching.filter { filter.includes($0) }
        let groups = Dictionary(grouping: rows) { cal.startOfDay(for: $0.at) }
            .map { DayGroup(day: $0.key, rows: $0.value) }.sorted { $0.day > $1.day }
        VStack(alignment: .leading, spacing: 12) {
            PageHeader(title: "历史", subtitle: "最近 \(records.all.count) 条，最多保留 1000 条，只保存在本机") { menu }
            if exporting { ProgressView("正在导出…").controlSize(.small) }
            if let error = recovery.error {
                DisclosureGroup("重试失败") {
                    Text(error).font(.callout).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .font(.callout).foregroundStyle(.orange)
            } else if let status = recovery.status {
                HStack {
                    Text(status).font(.callout).foregroundStyle(.secondary)
                    if recovery.busy { Button("取消") { recovery.cancel() }.controlSize(.small) }
                }
            }
            Picker("记录状态", selection: $filter) {
                ForEach(HistoryFilter.allCases, id: \.self) { item in
                    Text("\(item.title) \(matching.filter { item.includes($0) }.count)").tag(item)
                }
            }
            .pickerStyle(.segmented).labelsHidden().fixedSize()
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("搜索文本或应用", text: $query).textFieldStyle(.plain)
                    .accessibilityLabel("搜索历史")
                if !query.isEmpty {
                    Text("\(rows.count) 条").font(.caption).foregroundStyle(.secondary)
                    Button { query = "" } label: { Image(systemName: "xmark.circle.fill") }.buttonStyle(.plain).foregroundStyle(.tertiary)
                }
            }
            .inputField()
            List(selection: $selection) {
                ForEach(groups) { g in
                    HStack(spacing: 6) {
                        Text(dayTitle(g.day)).font(.subheadline.weight(.semibold)).foregroundStyle(.secondary)
                        Text("\(g.rows.count) 句").font(.subheadline).foregroundStyle(.tertiary)
                    }
                    .padding(.top, g.id == groups.first?.id ? 2 : 14).padding(.bottom, 2)
                    .selectionDisabled()
                    .listRowSeparator(.hidden)
                    ForEach(g.rows) { r in
                        HistoryRow(r: r, latency: latency, selected: selection.contains(r.id), expanded: selection == [r.id]).tag(r.id)
                    }
                }
            }
            .listStyle(.inset)
            .accessibilityIdentifier("history.list")
            .scrollContentBackground(.hidden)
            .contextMenu(forSelectionType: Record.ID.self) { ids in
                if !ids.isEmpty {   // right-click on blank space passes an empty set
                    let rs = records.all.filter { ids.contains($0.id) }
                    Button(ids.count > 1 ? "复制 \(ids.count) 条" : "复制") { copy(rs.map(\.text).joined(separator: "\n")) }
                    Button("导出选中的 \(ids.count) 条…") { export(rs) }.disabled(exporting)
                }
            }
            .card(padding: 0)
            .overlay { if rows.isEmpty { emptyState } }
        }
        .pageColumn()
        .onChange(of: filter) { selection.removeAll() }
        .onChange(of: query) { selection.removeAll() }
        .confirmationDialog("删除全部 \(records.all.count) 条记录和录音？", isPresented: $confirmClear) {
            Button("删除", role: .destructive) { records.clear() }
        } message: {
            Text("概览里的用量统计会保留。")
        }
        .alert("导出失败", isPresented: Binding(get: { exportError != nil }, set: { if !$0 { exportError = nil } })) {
            Button("好") {}
        } message: {
            Text(exportError ?? "")
        }
    }

    private var menu: some View {
        Menu {
            Toggle("显示延迟", isOn: $latency)
            Button("性能统计…") { SettingsNav.shared.page = .perf }
            Divider()
            Button("导出最近 20 条…") { export(Array(records.all.suffix(20))) }.disabled(records.all.isEmpty || exporting)
            Button("显示数据文件夹") { NSWorkspace.shared.activateFileViewerSelecting([Records.url]) }
            Divider()
            Button("清空历史…", role: .destructive) { confirmClear = true }.disabled(records.all.isEmpty)
        } label: { Image(systemName: "ellipsis.circle") }
        .menuIndicator(.hidden).fixedSize()
    }

    private var emptyState: some View {
        VStack(spacing: 10) {
            IconBadge(symbol: "waveform", color: .accentColor, size: 44)
            Text(records.all.isEmpty ? "还没有听写记录" : "没有匹配的记录").font(.headline)
            if records.all.isEmpty {
                Text("按住快捷键说话，松开后文字会粘贴到光标处。").foregroundStyle(.secondary)
            } else if query.isEmpty {
                Text(filter == .saved ? "没有已保存但未粘贴的记录。" : "没有识别失败的记录。")
                    .foregroundStyle(.secondary)
            } else {
                Text("试试其他关键词，或切换记录状态。").foregroundStyle(.secondary)
            }
        }
    }

    private func dayTitle(_ d: Date) -> String {
        let cal = Calendar.current
        if cal.isDateInToday(d) { return "今天" }
        if cal.isDateInYesterday(d) { return "昨天" }
        return d.formatted(.dateTime.month().day().weekday())
    }

    private func export(_ rs: [Record]) {
        let panel = NSSavePanel()
        let f = DateFormatter()
        f.dateFormat = "yyyyMMdd-HHmm"
        panel.nameFieldStringValue = "plaid-feedback-\(f.string(from: Date())).zip"
        panel.allowedContentTypes = [.zip]
        panel.message = "包含识别文本、对应录音、热词和非敏感设置，不含 API Key。"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        DictionaryDraft.shared.flush()
        let info = Records.exportInfo()
        exporting = true
        Records.export(rs, to: url, info: info) { result in
            exporting = false
            switch result {
            case .success: NSWorkspace.shared.activateFileViewerSelecting([url])
            case .failure(let error): exportError = error.localizedDescription
            }
        }
    }
}

private func copy(_ s: String) {
    NSPasteboard.general.clearContents()
    NSPasteboard.general.setString(s, forType: .string)
}

private struct HistoryRow: View {
    let r: Record
    let latency: Bool
    let selected: Bool
    let expanded: Bool
    @ObservedObject private var recovery = Recovery.shared
    @State private var hovering = false
    @State private var confirmRetry = false
    @State private var showDetails = false

    private var wav: URL? {
        r.audio.map { Records.audioDir.appendingPathComponent($0) }
            .flatMap { FileManager.default.fileExists(atPath: $0.path) ? $0 : nil }
    }

    private var retrying: Bool { recovery.busy && recovery.activeRecordID == r.id }

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Text(r.at.formatted(.dateTime.hour(.twoDigits(amPM: .omitted)).minute(.twoDigits)))
                .font(.callout.monospacedDigit()).foregroundStyle(.secondary)
                .fixedSize().frame(width: 44, alignment: .leading)
            Image(nsImage: AppIcon.image(r.app)).resizable().frame(width: 18, height: 18).help(r.app ?? "未知应用")
            VStack(alignment: .leading, spacing: 6) {
                Text(r.text.isEmpty ? "没有识别结果" : r.text).lineLimit(expanded ? nil : 2)
                    .frame(maxWidth: .infinity, alignment: .leading)
                HStack(spacing: 8) {
                    if retrying {
                        ProgressView().controlSize(.mini)
                        Text("重新识别中…").font(.caption).foregroundStyle(.secondary)
                    } else if r.isFailure || r.outcome == "saved" {
                        Label(r.isFailure ? "识别失败" : "未粘贴", systemImage: r.isFailure ? "exclamationmark.triangle" : "tray.and.arrow.down")
                            .font(.caption).foregroundStyle(r.isFailure ? Color.orange : Color.secondary)
                            .help(r.isFailure ? "原记录和保留的录音可用于排查或重试。" : "文字已保存，需手动复制；不会自动粘贴。")
                    }
                    if r.edited != nil {
                        Image(systemName: "pencil.line").font(.caption).foregroundStyle(.green).help("你修改过这句")
                    }
                    Text(String(format: "%.0f″", Double(r.audioMs) / 1000))
                        .font(.caption.monospacedDigit()).foregroundStyle(.tertiary)
                    Spacer(minLength: 4)
                    if selected || hovering {
                        rowActions
                    }
                }
                .frame(minHeight: 22)
                if latency && !r.isFailure && r.outcome != "saved" {
                    Text("首字 \(ms(r.firstTextMs)) · 识别 \(ms(r.asrMs)) · LLM \(r.llm == "ok" ? ms(r.llmMs) : r.llm) · 粘贴 \(ms(r.totalMs))")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(r.llm == "ok" || r.llm == "off" ? Color.secondary : Color.orange)
                }
                if expanded { detail }
            }
        }
        .padding(.vertical, 5)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .onChange(of: expanded) { if !expanded { showDetails = false } }
        .confirmationDialog("重新识别这段录音？", isPresented: $confirmRetry) {
            Button("重新识别") { recovery.retry(recordID: r.id) }
        } message: {
            Text("这段录音将上传到当前云端识别服务。结果保存到原记录，不自动粘贴，也不运行优化或命令。")
        }
    }

    private var rowActions: some View {
        HStack(spacing: 12) {
            Button { copy(r.text) } label: { Label("复制", systemImage: "doc.on.doc") }
                .disabled(r.text.isEmpty)
            if wav != nil {
                if retrying {
                    Button("取消") { recovery.cancel() }
                } else {
                    Button { confirmRetry = true } label: { Label("重试…", systemImage: "arrow.clockwise") }
                        .disabled(recovery.busy || recovery.recording)
                        .help("使用当前识别引擎；云端引擎会上传这段录音。结果只保存，不自动粘贴。")
                }
            }
        }
        .buttonStyle(.borderless).controlSize(.small)
    }

    /// Keep raw errors and diagnostic fields behind an explicit disclosure, not in the record preview.
    private var detail: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let wav {
                HStack(spacing: 12) {
                    Button { NSSound(contentsOf: wav, byReference: true)?.play() } label: { Label("播放录音", systemImage: "play.fill") }
                    Button { NSWorkspace.shared.activateFileViewerSelecting([wav]) } label: { Label("在访达中显示", systemImage: "folder") }
                }
                .buttonStyle(.borderless).controlSize(.small)
            } else {
                Text("没有可用录音，无法重新识别。").font(.caption).foregroundStyle(.secondary)
            }
            DisclosureGroup(r.error == nil ? "详情" : "错误与详情", isExpanded: $showDetails) {
                Grid(alignment: .topLeading, horizontalSpacing: 12, verticalSpacing: 6) {
                    if let error = r.error { field("错误", error) }
                    if let app = r.app { field("应用", app) }
                    if let triggerMs = r.triggerMs { field("按键 → 启动", "\(triggerMs) ms") }
                    if let micStartMs = r.micStartMs { field("麦克风启动", "\(micStartMs) ms") }
                    if r.raw != r.text { field("识别", r.raw) }
                    if let e = r.edited { field("修改后", e) }
                    if let n = r.noiseRMS, let sp = r.speechRMS {
                        field("电平", "噪声约 \(n)，语音约 \(sp)（RMS）\(r.mic.map { "，\($0)" } ?? "")")
                    }
                    if r.llm != "ok" && r.llm != "off" { field("LLM", r.llm) }
                }
                .padding(.top, 6)
            }
            .font(.callout)
        }
        .padding(.top, 4)
    }

    private func field(_ k: String, _ v: String) -> some View {
        GridRow {
            Text(k).foregroundStyle(.secondary)
            Text(v).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}
