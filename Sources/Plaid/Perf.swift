import AppKit
import Charts
import SwiftUI

/// Performance and raw-data page: latency percentiles, per-utterance stage breakdown, breakdowns by engine/app/mic,
/// the raw records table with an inspector, and events.log. Read-only over records.jsonl and events.log.
struct PerfPage: View {
    @ObservedObject private var records = Records.shared
    @AppStorage("perfRange") private var range = 200  // 50 | 200 | 0 = all
    @State private var engine: String?  // nil = all, "volc", "local"
    @State private var disk: (jsonl: String, audio: String)?
    @State private var day: Date?
    @State private var sel: Int?
    @State private var sortOrder = [KeyPathComparator(\Row.r.at, order: .reverse)]
    @State private var selected: Date?
    @State private var showJSON = false
    @State private var events: [Event] = []  // newest first
    @State private var loaded = false
    @State private var kinds = Set(Kind.allCases)
    @State private var logQuery = ""
    @State private var tick = 0

    private var rows: [Record] {
        let rs = records.all.suffix(range == 0 ? records.all.count : range)
        guard let engine else { return Array(rs) }
        return rs.filter { ($0.engine == "local") == (engine == "local") }
    }

    var body: some View {
        let rows = rows
        let samples = rows.filter { !$0.isFailure && $0.outcome != "saved" }
        let hasBoth = records.all.contains { $0.engine == "local" } && records.all.contains { $0.engine != "local" }
        Page(title: "性能", subtitle: "最近 \(rows.count) 句的延迟、电平、LLM 结果和原始数据，用来排查慢和漏字") {
            HStack(spacing: 8) {
                if hasBoth {
                    Picker("", selection: $engine) {
                        Text("全部引擎").tag(String?.none)
                        Text("火山引擎").tag(Optional("volc"))
                        Text("本地").tag(Optional("local"))
                    }
                    .labelsHidden().fixedSize()
                }
                Picker("", selection: $range) {
                    Text("最近 50 句").tag(50)
                    Text("最近 200 句").tag(200)
                    Text("全部").tag(0)
                }
                .pickerStyle(.segmented).labelsHidden().fixedSize()
            }
        } content: {
            status
            if records.all.isEmpty {
                VStack(spacing: 10) {
                    IconBadge(symbol: "waveform", color: .accentColor, size: 44)
                    Text("还没有听写记录").font(.headline)
                    Text("说几句之后，这里会显示每句的延迟、电平和原始记录。").foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, minHeight: 220)
                .card()
            } else {
                if !samples.isEmpty {
                    latency(samples)
                    trend(samples)
                    stages(samples)
                    groups(samples)
                    levels(samples)
                }
                if samples.count != rows.count {
                    Text("延迟和电平统计只包含已粘贴的成功听写；失败及仅保存的结果仍在原始记录中。")
                        .font(.callout).foregroundStyle(.secondary)
                }
                raw(rows)
            }
            eventLog
        }
        .task(id: records.all.count) { await refreshSizes() }
        .task(id: tick) { await loadEvents() }
    }

    // MARK: 运行状态

    private var status: some View {
        SectionBlock("运行状态") {
            RowGroup {
                SettingRow("内存占用", subtitle: "进程 phys_footprint") {
                    TimelineView(.periodic(from: .now, by: 2)) { _ in
                        Text(Self.footprint().map { ByteCountFormatter.string(fromByteCount: Int64($0), countStyle: .memory) } ?? "–")
                            .monospacedDigit()
                    }
                }
                SettingRow("数据", subtitle: "~/Library/Application Support/Plaid") {
                    Text("\(records.all.count) 句" + (disk.map { " · records.jsonl \($0.jsonl) · 录音 \($0.audio)" } ?? ""))
                        .font(.callout.monospacedDigit()).foregroundStyle(.secondary)
                }
            }
        }
    }

    private func refreshSizes() async {
        let s = await Task.detached(priority: .utility) { () -> (String, String) in
            let fm = FileManager.default
            let jsonl = (try? fm.attributesOfItem(atPath: Records.url.path)[.size] as? Int64) ?? 0
            let audio = ((try? fm.contentsOfDirectory(at: Records.audioDir, includingPropertiesForKeys: [.fileSizeKey])) ?? [])
                .reduce(Int64(0)) { $0 + Int64((try? $1.resourceValues(forKeys: [.fileSizeKey]).fileSize) ?? 0) }
            return (ByteCountFormatter.string(fromByteCount: jsonl, countStyle: .file), ByteCountFormatter.string(fromByteCount: audio, countStyle: .file))
        }.value
        disk = s
    }

    private static func footprint() -> Int? {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size)
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) { task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count) }
        }
        return kr == KERN_SUCCESS ? Int(info.phys_footprint) : nil
    }

    // MARK: 延迟

    private func latency(_ rows: [Record]) -> some View {
        let calls = rows.filter { $0.llm != "off" }
        let ok = calls.filter { $0.llm == "ok" }.count
        let timeouts = calls.filter { $0.llm == "timeout" }.count, skipped = calls.filter { $0.llm == "skipped" }.count
        let failed = calls.count - ok
        return SectionBlock("延迟", footnote: "中位数；小字为 p90 和句数。首字：按下到第一个字。识别：松键到最终结果（按 esc 跳过优化时含等待）。LLM：优化耗时，只算成功的。粘贴：松键到粘贴完成。") {
            HStack(spacing: 12) {
                Self.stat("首字", "text.cursor", .blue, rows.compactMap(\.firstTextMs))
                Self.stat("松键 → 识别", "waveform", .blue, rows.map(\.asrMs))
                Self.stat("LLM", "sparkles", .purple, rows.filter { $0.llm == "ok" }.compactMap(\.llmMs))
                Self.stat("松键 → 粘贴", "doc.on.clipboard", .gray, rows.map(\.totalMs))
                Stat(symbol: "exclamationmark.triangle", color: .orange, name: "LLM 回退",
                     value: calls.isEmpty ? "–" : "\(failed)/\(calls.count)",
                     sub: calls.isEmpty ? "优化未启用" : "超时 \(timeouts) · 跳过 \(skipped) · 出错 \(failed - timeouts - skipped)")
            }
        }
    }

    /// Median / p90 tile over `values`.
    private static func stat(_ name: String, _ symbol: String, _ color: Color, _ values: [Int]) -> Stat {
        Stat(symbol: symbol, color: color, name: name, value: pct(values, 0.5).map(String.init) ?? "–",
             unit: values.isEmpty ? nil : "ms", sub: "p90 \(ms(pct(values, 0.9))) · \(values.count) 句")
    }

    // MARK: 趋势

    private struct DayPoint: Identifiable {
        let day: Date, series: String, segment: Int, p50: Int, p90: Int, n: Int
        var id: String { "\(series)-\(day.timeIntervalSince1970)" }
    }

    /// Daily p50/p90 per series. `segment` increments across a day without data so the line breaks there.
    private static func dayPoints(_ rows: [Record]) -> [DayPoint] {
        let cal = Calendar.current
        var out: [DayPoint] = [], segment = 0, prev: Date?
        for (d, g) in Dictionary(grouping: rows, by: { cal.startOfDay(for: $0.at) }).sorted(by: { $0.key < $1.key }) {
            if let prev, (cal.dateComponents([.day], from: prev, to: d).day ?? 0) > 1 { segment += 1 }
            prev = d
            let asr = g.map(\.asrMs), total = g.map(\.totalMs)
            out.append(DayPoint(day: d, series: "识别", segment: segment, p50: pct(asr, 0.5)!, p90: pct(asr, 0.9)!, n: g.count))
            out.append(DayPoint(day: d, series: "粘贴", segment: segment, p50: pct(total, 0.5)!, p90: pct(total, 0.9)!, n: g.count))
        }
        return out
    }

    @ViewBuilder
    private func trend(_ rows: [Record]) -> some View {
        let pts = Self.dayPoints(rows)
        let days = Set(pts.map(\.day)).count
        if days >= 2 {
            let near = day.flatMap { h in pts.min { abs($0.day.timeIntervalSince(h)) < abs($1.day.timeIntervalSince(h)) }?.day }
            let picked = pts.filter { $0.day == near }
            SectionBlock("趋势", footnote: "每天的中位数；悬停看 p90 和句数。") {
                VStack(alignment: .leading, spacing: 12) {
                    Group {
                        if let a = picked.first(where: { $0.series == "识别" }), let t = picked.first(where: { $0.series == "粘贴" }) {
                            Text("\(a.day.formatted(.dateTime.month(.defaultDigits).day())) · \(a.n) 句 · 识别 \(a.p50)/\(a.p90) ms · 粘贴 \(t.p50)/\(t.p90) ms")
                        } else {
                            Text("\(days) 天 · \(rows.count) 句")
                        }
                    }
                    .font(.callout).monospacedDigit().foregroundStyle(.secondary)
                    Chart {
                        ForEach(pts) { p in
                            // Plotted at midnight (no unit) so points sit on the day labels; the domain pads half a day.
                            LineMark(x: .value("日期", p.day), y: .value("ms", p.p50), series: .value("段", "\(p.series)-\(p.segment)"))
                                .foregroundStyle(by: .value("指标", p.series))
                                .lineStyle(StrokeStyle(lineWidth: 2))
                            PointMark(x: .value("日期", p.day), y: .value("ms", p.p50))
                                .foregroundStyle(by: .value("指标", p.series))
                                .symbolSize(p.day == near ? 60 : 30)
                        }
                        if let near {
                            RuleMark(x: .value("日期", near)).foregroundStyle(Color.secondary.opacity(0.4)).lineStyle(StrokeStyle(lineWidth: 1))
                        }
                    }
                    .chartForegroundStyleScale(["识别": Color.blue, "粘贴": Color.primary.opacity(0.7)])
                    .chartLegend(position: .top, alignment: .leading, spacing: 12)
                    .chartXScale(domain: pts.first!.day.addingTimeInterval(-43200)...pts.last!.day.addingTimeInterval(43200))
                    .chartXSelection(value: $day)
                    .chartXAxis {
                        AxisMarks(values: .stride(by: .day, count: days > 30 ? 7 : 1)) { _ in
                            AxisValueLabel(format: .dateTime.month(.defaultDigits).day())
                        }
                    }
                    .chartYAxis { Self.yAxis }
                    .frame(height: 180)
                }
                .card(padding: 18)
            }
        }
    }

    private static var yAxis: some AxisContent {
        AxisMarks(position: .leading, values: .automatic(desiredCount: 4)) { _ in
            AxisGridLine(stroke: StrokeStyle(lineWidth: 0.5, dash: [3, 3])).foregroundStyle(.quaternary)
            AxisValueLabel()
        }
    }

    // MARK: 阶段拆分

    private struct Seg: Identifiable {
        let i: Int, stage: String, ms: Int
        var id: String { "\(i)-\(stage)" }
    }

    private func stages(_ rows: [Record]) -> some View {
        let last = Array(rows.suffix(60))
        let segs: [Seg] = last.enumerated().flatMap { i, r -> [Seg] in
            var s = [Seg(i: i, stage: "识别", ms: r.asrMs)]
            if let l = r.llmMs, r.llm == "ok" { s.append(Seg(i: i, stage: "LLM", ms: l)) }
            if let l = r.llmMs, !["ok", "off", "skipped"].contains(r.llm) { s.append(Seg(i: i, stage: "LLM 失败", ms: l)) }
            s.append(Seg(i: i, stage: "其余", ms: max(0, r.totalMs - r.asrMs - (r.llmMs ?? 0))))
            return s
        }
        // Fixed order keeps each stage's colour stable; only stages present in the data get a legend entry.
        let order: [(String, Color)] = [("识别", .blue), ("LLM", .purple), ("LLM 失败", .orange), ("其余", Color.gray.opacity(0.45))]
        let present = Set(segs.map(\.stage))
        let used = order.filter { present.contains($0.0) }
        let picked = sel.flatMap { last.indices.contains($0) ? last[$0] : nil }
        return SectionBlock("阶段拆分", footnote: "每根柱是一句，从左到右按时间，最多 60 句。灰色是识别和 LLM 之外的时间（热词替换、粘贴）。悬停看具体值。") {
            VStack(alignment: .leading, spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    if let r = picked {
                        Text("\(r.at.formatted(.dateTime.hour(.twoDigits(amPM: .omitted)).minute(.twoDigits).second(.twoDigits))) · \(r.app ?? "未知应用") · 识别 \(r.asrMs) ms · LLM \(r.llm == "ok" ? ms(r.llmMs) : r.llm) · 粘贴 \(r.totalMs) ms")
                            .font(.callout).monospacedDigit().foregroundStyle(.secondary).lineLimit(1)
                        Text(r.text).font(.caption).foregroundStyle(.secondary).lineLimit(1)
                    } else {
                        Text("\(last.count) 句 · 识别中位 \(ms(pct(last.map(\.asrMs), 0.5))) · 粘贴中位 \(ms(pct(last.map(\.totalMs), 0.5)))")
                            .font(.callout).monospacedDigit().foregroundStyle(.secondary)
                        Text(" ").font(.caption)  // keeps the plot from jumping when the hover line adds the text row
                    }
                }
                Chart(segs) { s in
                    // Categorical x (index as string) so the bar width ratio applies.
                    BarMark(x: .value("句", String(s.i)), y: .value("ms", s.ms), width: .ratio(0.7))
                        .foregroundStyle(by: .value("阶段", s.stage))
                        .opacity(sel == nil || sel == s.i ? 1 : 0.35)
                }
                .chartForegroundStyleScale(domain: used.map(\.0), range: used.map(\.1))
                .chartXSelection(value: Binding(get: { sel.map(String.init) }, set: { sel = $0.flatMap { Int($0) } }))
                .chartXAxis(.hidden)
                .chartYAxis { Self.yAxis }
                .chartLegend(position: .top, alignment: .leading, spacing: 12)
                .frame(height: 200)
            }
            .card(padding: 18)
        }
    }

    // MARK: 分组

    private func groups(_ rows: [Record]) -> some View {
        let volc = rows.filter { $0.engine != "local" }, loc = rows.filter { $0.engine == "local" }
        let byApp = Dictionary(grouping: rows) { $0.app ?? "未知应用" }.sorted { ($0.value.count, $0.key) > ($1.value.count, $1.key) }
        let other = byApp.dropFirst(6).flatMap(\.value)
        let columns = ["句数", "识别 p50", "粘贴 p50", "粘贴 p90", "回退"]
        return SectionBlock("分组", footnote: "回退 = 超时、跳过或出错 / 调用次数。") {
            RowGroup {
                GroupHeader(name: "引擎", columns: columns)
                if !volc.isEmpty { GroupRow(icon: nil, name: "火山引擎", rs: volc) }
                if !loc.isEmpty { GroupRow(icon: nil, name: "本地", rs: loc) }
                GroupHeader(name: "应用", columns: columns)
                ForEach(byApp.prefix(6), id: \.key) { GroupRow(icon: AppIcon.image($0.key), name: $0.key, rs: $0.value) }
                if !other.isEmpty { GroupRow(icon: nil, name: "其他", rs: other) }
            }
        }
    }

    // MARK: 电平

    private struct Bin: Identifiable {
        let start: Int, kind: String, n: Int
        var id: String { "\(kind)-\(start)" }
    }

    @ViewBuilder
    private func levels(_ rows: [Record]) -> some View {
        let leveled = rows.filter { $0.noiseRMS != nil && $0.speechRMS != nil }
        if !leveled.isEmpty {
            let silence = Int(Double(Config.value("silenceRMS") ?? "") ?? Config.silenceDefault)
            let bins: [Bin] = [("噪声", leveled.map { $0.noiseRMS! }), ("语音", leveled.map { $0.speechRMS! })].flatMap { kind, vs in
                let counts = vs.reduce(into: [Int](repeating: 0, count: 30)) { $0[min($1 / 50, 29)] += 1 }
                return counts.enumerated().map { Bin(start: $0.offset * 50, kind: kind, n: $0.element) }
            }
            let byMic = Dictionary(grouping: rows) { $0.mic ?? "未记录设备" }.sorted { ($0.value.count, $0.key) > ($1.value.count, $1.key) }
            SectionBlock("电平", footnote: "噪声 = 每句 100 ms 片段 RMS 的 p10，语音 = p90。静音阈值在「设置 → 松键收尾」；语音低于 \(Mic.quietRMS) 时识别容易漏字。") {
                VStack(alignment: .leading, spacing: 12) {
                    Text("静音阈值 \(silence)（橙线）· 语音过低 \(Mic.quietRMS)（灰线）").font(.callout).monospacedDigit().foregroundStyle(.secondary)
                    Chart {
                        ForEach(bins) { b in
                            // Side by side within the 50-wide bin: noise on the left half, speech on the right.
                            let x0 = b.start + (b.kind == "噪声" ? 4 : 25)
                            RectangleMark(xStart: .value("RMS", x0), xEnd: .value("RMS", x0 + 21), yStart: .value("句数", 0), yEnd: .value("句数", b.n))
                                .foregroundStyle(by: .value("类型", b.kind))
                        }
                        RuleMark(x: .value("静音阈值", silence)).foregroundStyle(.orange).lineStyle(StrokeStyle(lineWidth: 1))
                        RuleMark(x: .value("电平过低", Mic.quietRMS)).foregroundStyle(.secondary).lineStyle(StrokeStyle(lineWidth: 1))
                    }
                    .chartForegroundStyleScale(["噪声": Color.gray.opacity(0.45), "语音": Color.blue])
                    .chartLegend(position: .top, alignment: .leading, spacing: 12)
                    .chartXScale(domain: 0...1500)
                    .chartYAxis { Self.yAxis }
                    .frame(height: 160)
                }
                .card(padding: 18)
                RowGroup {
                    ForEach(byMic, id: \.key) { name, g in
                        let noise = pct(g.compactMap(\.noiseRMS), 0.5), speech = pct(g.compactMap(\.speechRMS), 0.5)
                        let low = speech.map { $0 < Mic.quietRMS } ?? false
                        SettingRow(name, subtitle: "\(g.count) 句 · 噪声中位 \(noise.map(String.init) ?? "–") · 语音中位 \(speech.map(String.init) ?? "–")"
                            + (low ? " · 语音电平偏低" : ""), tint: low ? .orange : nil)
                    }
                }
            }
        }
    }

    // MARK: 原始记录

    private struct Row: Identifiable {
        let r: Record
        var id: Date { r.at }
        var app: String { r.app ?? "" }
        var firstText: Int { r.firstTextMs ?? -1 }
        var llm: Int { r.llmMs ?? -1 }
        var noise: Int { r.noiseRMS ?? -1 }
        var speech: Int { r.speechRMS ?? -1 }
    }

    private func raw(_ rows: [Record]) -> some View {
        let sorted = rows.map(Row.init).sorted(using: sortOrder)
        let picked = selected.flatMap { id in rows.last { $0.id == id } }
        return SectionBlock("原始记录", count: rows.count, footnote: "点一行看三个文本版本的差异和完整 JSON。列标题可排序；时长单位为秒，其余为 ms。") {
            Menu {
                Button("复制为 CSV") { Self.copy(Self.csv(sorted.map(\.r))) }
                Button("在访达中显示 records.jsonl") { NSWorkspace.shared.activateFileViewerSelecting([Records.url]) }
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
        } content: {
            Table(sorted, selection: $selected, sortOrder: $sortOrder) {
                TableColumn("时间", value: \Row.r.at) {
                    Text($0.r.at.formatted(.dateTime.month(.twoDigits).day(.twoDigits).hour(.twoDigits(amPM: .omitted)).minute(.twoDigits).second(.twoDigits)))
                        .monospacedDigit()
                }
                .width(108)
                TableColumn("应用", value: \Row.app).width(min: 60, ideal: 68)
                TableColumn("时长", value: \Row.r.audioMs) { Num(String(format: "%.1f", Double($0.r.audioMs) / 1000)) }.width(44)
                TableColumn("首字", value: \Row.firstText) { Num($0.firstText) }.width(44)
                TableColumn("识别", value: \Row.r.asrMs) { Num($0.r.asrMs) }.width(44)
                TableColumn("LLM", value: \Row.llm) { r in
                    if r.r.llm == "ok" { Num(r.llm) } else if r.r.llm == "off" { Num(-1) } else {
                        Text(r.r.llm).foregroundStyle(.orange).lineLimit(1).help(r.r.llm)
                    }
                }
                .width(50)
                TableColumn("粘贴", value: \Row.r.totalMs) { Num($0.r.totalMs) }.width(44)
                TableColumn("噪声", value: \Row.noise) { Num($0.noise) }.width(38)
                TableColumn("语音", value: \Row.speech) { Num($0.speech) }.width(38)
                TableColumn("文本", value: \Row.r.text) { row in
                    Text((row.r.isFailure ? "[失败] " : row.r.outcome == "saved" ? "[未粘贴] " : "") + (row.r.text.isEmpty ? row.r.error ?? "" : row.r.text))
                        .foregroundStyle(row.r.isFailure ? Color.orange : Color.primary)
                        .lineLimit(1).help(row.r.error ?? row.r.text)
                }.width(min: 90)
            }
            .frame(height: 360)
            .card(padding: 0)
            if let r = picked { inspector(r) }
        }
    }

    private func inspector(_ r: Record) -> some View {
        let json = Self.json(r)
        let wav = r.audio.map { Records.audioDir.appendingPathComponent($0) }.flatMap { FileManager.default.fileExists(atPath: $0.path) ? $0 : nil }
        let pasted = Self.changes(r.raw, r.text)
        return VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(nsImage: AppIcon.image(r.app)).resizable().frame(width: 18, height: 18)
                Text(r.app ?? "未知应用").font(.headline)
                Text("\(r.at.formatted(.dateTime.year().month().day().hour().minute().second())) · \(String(format: "%.1f 秒", Double(r.audioMs) / 1000)) · \(r.engine == "local" ? "本地" : "火山引擎") · \(r.mic ?? "未记录设备")")
                    .foregroundStyle(.secondary).lineLimit(1)
                Spacer()
                Button("复制 JSON") { Self.copy(json) }
                if let wav {
                    Button { NSWorkspace.shared.activateFileViewerSelecting([wav]) } label: { Image(systemName: "folder") }.help("在访达中显示录音")
                }
            }
            Grid(alignment: .topLeading, horizontalSpacing: 12, verticalSpacing: 6) {
                if r.isFailure || r.outcome == "saved" || r.error != nil {
                    GridRow {
                        Text("状态").foregroundStyle(.secondary)
                        Text((r.isFailure ? "识别失败" : "已保存，未自动粘贴") + (r.error.map { "：" + $0 } ?? ""))
                            .foregroundStyle(r.isFailure ? Color.orange : Color.secondary).textSelection(.enabled)
                    }
                }
                if r.raw != r.text {
                    GridRow {
                        Text("识别").foregroundStyle(.secondary)
                        Self.mark(r.raw, pasted.removed, strike: true).textSelection(.enabled)
                    }
                }
                GridRow {
                    Text("粘贴").foregroundStyle(.secondary)
                    Self.mark(r.text, pasted.inserted, strike: false).textSelection(.enabled)
                }
                if let e = r.edited {
                    GridRow {
                        Text("修改后").foregroundStyle(.secondary)
                        Self.mark(e, Self.changes(r.pasted ?? r.text, e).inserted, strike: false).textSelection(.enabled)
                    }
                }
                if r.llm != "ok" && r.llm != "off" {
                    GridRow {
                        Text("LLM").foregroundStyle(.secondary)
                        Text(r.llm).foregroundStyle(.orange).textSelection(.enabled)
                    }
                }
            }
            DisclosureGroup("原始 JSON", isExpanded: $showJSON) {
                ScrollView {
                    Text(json).font(.callout.monospaced()).textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 260)
                .inputField()
            }
        }
        .card()
    }

    /// Character offsets removed from `a` and inserted into `b`.
    private static func changes(_ a: String, _ b: String) -> (removed: Set<Int>, inserted: Set<Int>) {
        var removed = Set<Int>(), inserted = Set<Int>()
        for c in Array(b).difference(from: Array(a)) {
            switch c {
            case .remove(let o, _, _): removed.insert(o)
            case .insert(let o, _, _): inserted.insert(o)
            }
        }
        return (removed, inserted)
    }

    /// Styles the characters at `changed`: strikethrough + secondary for removals, green + medium for insertions.
    private static func mark(_ s: String, _ changed: Set<Int>, strike: Bool) -> Text {
        var out = Text(""), run = "", on = false
        func flush() {
            guard !run.isEmpty else { return }
            let t = Text(run)
            let styled = !on ? t : strike ? t.strikethrough(true, color: .secondary).foregroundColor(.secondary) : t.foregroundColor(.green).fontWeight(.medium)
            out = Text("\(out)\(styled)")
            run = ""
        }
        for (i, c) in s.enumerated() {
            if changed.contains(i) != on { flush(); on.toggle() }
            run.append(c)
        }
        flush()
        return out
    }

    /// Records.export encoder settings plus pretty print.
    private static func json(_ r: Record) -> String {
        let enc = JSONEncoder()
        enc.dateEncodingStrategy = .iso8601
        enc.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        return (try? enc.encode(r)).map { String(decoding: $0, as: UTF8.self) } ?? ""
    }

    private static func csv(_ rs: [Record]) -> String {
        let iso = ISO8601DateFormatter()
        func q(_ s: String?) -> String { s.map { "\"" + $0.replacingOccurrences(of: "\"", with: "\"\"") + "\"" } ?? "" }
        func n(_ v: Int?) -> String { v.map(String.init) ?? "" }
        let lines = rs.map { r in
            [iso.string(from: r.at), q(r.app), q(r.engine == "local" ? "local" : "volc"), n(r.audioMs), n(r.firstTextMs), n(r.asrMs),
             n(r.llmMs), q(r.llm), n(r.totalMs), n(r.noiseRMS), n(r.speechRMS), q(r.mic), r.vp.map { $0 ? "true" : "false" } ?? "",
             q(r.edited), q(r.text)].joined(separator: ",")
        }
        return (["at,app,engine,audioMs,firstTextMs,asrMs,llmMs,llm,totalMs,noiseRMS,speechRMS,mic,vp,edited,text"] + lines)
            .joined(separator: "\n") + "\n"
    }

    private static func copy(_ s: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(s, forType: .string)
    }

    // MARK: 事件日志

    private enum Kind: CaseIterable {
        case error, key, record, asr, other

        var title: String {
            switch self {
            case .error: "错误"
            case .key: "按键"
            case .record: "录音"
            case .asr: "识别"
            case .other: "其他"
            }
        }

        static func of(_ m: String) -> Kind {
            // Errors first: "finished error:", "start failed:", "key tap unavailable".
            if m.contains("failed") || m.contains("error") || m.contains("unavailable") { return .error }
            if m.hasPrefix("key ") { return .key }
            if m.hasPrefix("start") || m.hasPrefix("stop") || m.hasPrefix("end ") || m == "return" || m == "esc" { return .record }
            if m.hasPrefix("finished") || m.hasPrefix("empty result") { return .asr }
            return .other
        }
    }

    private struct Event: Identifiable {
        let id: Int, at: Date?, msg: String, kind: Kind
    }

    /// Last 500 lines of events.log, parsed off the main actor, newest first.
    private func loadEvents() async {
        events = await Task.detached(priority: .utility) { () -> [Event] in
            let f = ISO8601DateFormatter()
            f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            let text = (try? String(contentsOf: Trace.url, encoding: .utf8)) ?? ""
            let lines = text.split(separator: "\n", omittingEmptySubsequences: true).suffix(500)
            return lines.enumerated().map { i, l in
                let parts = l.split(separator: " ", maxSplits: 1)
                if parts.count == 2, let at = f.date(from: String(parts[0])) {
                    return Event(id: i, at: at, msg: String(parts[1]), kind: .of(String(parts[1])))
                }
                return Event(id: i, at: nil, msg: String(l), kind: .of(String(l)))
            }
            .reversed()
        }.value
        loaded = true
    }

    private var eventLog: some View {
        let shown = events.filter { kinds.contains($0.kind) && (logQuery.isEmpty || $0.msg.localizedCaseInsensitiveContains(logQuery)) }
        return SectionBlock("事件日志", count: shown.count, footnote: "只显示最近 500 行；events.log 超过 256 KB 时自动截掉前一半。") {
            HStack(spacing: 8) {
                TextField("筛选", text: $logQuery).textFieldStyle(.roundedBorder).frame(width: 160)
                Menu {
                    ForEach(Kind.allCases, id: \.self) { k in
                        Toggle(k.title, isOn: Binding(get: { kinds.contains(k) }, set: { if $0 { kinds.insert(k) } else { kinds.remove(k) } }))
                    }
                } label: {
                    Label("类型", systemImage: "line.3.horizontal.decrease.circle")
                }
                .menuIndicator(.hidden).fixedSize()
                Button { tick += 1 } label: { Image(systemName: "arrow.clockwise") }.help("刷新")
                Button { NSWorkspace.shared.activateFileViewerSelecting([Trace.url]) } label: { Image(systemName: "folder") }
                    .help("在访达中显示 events.log")
            }
        } content: {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(shown.enumerated()), id: \.element.id) { i, e in
                        if i == 0 || !Calendar.current.isDate(e.at ?? .distantPast, inSameDayAs: shown[i - 1].at ?? .distantPast) {
                            Text(Self.dayTitle(e.at)).font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                                .padding(.horizontal, Metrics.rowInset).padding(.top, i == 0 ? 8 : 12).padding(.bottom, 2)
                        }
                        HStack(alignment: .firstTextBaseline, spacing: 10) {
                            Text(e.at.map { $0.formatted(.dateTime.hour(.twoDigits(amPM: .omitted)).minute(.twoDigits).second(.twoDigits).secondFraction(.fractional(3))) } ?? "")
                                .foregroundStyle(.secondary).frame(width: 92, alignment: .leading)
                            Text(e.msg).foregroundStyle(e.kind == .error ? Color.orange : Color.primary).textSelection(.enabled)
                        }
                        .font(.callout.monospaced())
                        .padding(.horizontal, Metrics.rowInset).padding(.vertical, 3)
                    }
                }
                .padding(.bottom, 8)
            }
            .frame(height: shown.isEmpty ? 140 : 300)
            .overlay {
                if shown.isEmpty && loaded {
                    VStack(spacing: 6) {
                        Image(systemName: "doc.text.magnifyingglass").font(.title2).foregroundStyle(.tertiary)
                        Text(events.isEmpty ? "还没有事件记录" : "没有匹配的事件").font(.headline)
                        if events.isEmpty { Text("按键、录音和识别会记在 events.log。").foregroundStyle(.secondary) }
                    }
                }
            }
            .card(padding: 0)
        }
    }

    private static func dayTitle(_ d: Date?) -> String {
        guard let d else { return "未知日期" }
        let cal = Calendar.current
        if cal.isDateInToday(d) { return "今天" }
        if cal.isDateInYesterday(d) { return "昨天" }
        return d.formatted(.dateTime.month().day())
    }
}

/// Right-aligned monospaced number cell; negative means missing.
private struct Num: View {
    let text: String
    init(_ text: String) { self.text = text }
    init(_ v: Int) { text = v < 0 ? "–" : String(v) }

    var body: some View {
        Text(text).monospacedDigit().frame(maxWidth: .infinity, alignment: .trailing)
    }
}

/// 概览 Tile proportions with an icon badge; Overview's Tile stays private there.
private struct Stat: View {
    let symbol: String, color: Color, name: String, value: String
    var unit: String?
    let sub: String

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                IconBadge(symbol: symbol, color: color, size: 24)
                Text(name).font(.callout).foregroundStyle(.secondary).lineLimit(1)
            }
            HStack(alignment: .firstTextBaseline, spacing: 3) {
                Text(value).font(.system(size: 26, weight: .semibold, design: .rounded)).monospacedDigit()
                if let unit { Text(unit).font(.callout).foregroundStyle(.secondary) }
            }
            .lineLimit(1).minimumScaleFactor(0.6)
            Text(sub).font(.caption).monospacedDigit().foregroundStyle(.secondary).lineLimit(1).minimumScaleFactor(0.8)
        }
        .card()
    }
}

private let groupColumn: CGFloat = 72

private struct GroupHeader: View {
    let name: String, columns: [String]

    var body: some View {
        HStack(spacing: 8) {
            Text(name).frame(maxWidth: .infinity, alignment: .leading)
            ForEach(columns, id: \.self) { Text($0).frame(width: groupColumn, alignment: .trailing) }
        }
        .font(.caption).foregroundStyle(.secondary)
        .padding(.horizontal, Metrics.rowInset).padding(.vertical, 6)
        .frame(maxWidth: .infinity, minHeight: 28, alignment: .leading)
        .background(Color.cardFill)
    }
}

/// label | 句数 | 识别 p50 | 粘贴 p50 | 粘贴 p90 | 回退
private struct GroupRow: View {
    let icon: NSImage?, name: String, rs: [Record]

    var body: some View {
        let calls = rs.filter { $0.llm != "off" }
        let failed = calls.filter { $0.llm != "ok" }.count
        let values = [rs.count.formatted(), ms(pct(rs.map(\.asrMs), 0.5)), ms(pct(rs.map(\.totalMs), 0.5)), ms(pct(rs.map(\.totalMs), 0.9)),
                      calls.isEmpty ? "–" : "\(failed)/\(calls.count)"]
        HStack(spacing: 8) {
            if let icon { Image(nsImage: icon).resizable().frame(width: 18, height: 18) }
            Text(name).lineLimit(1)
            Spacer(minLength: 8)
            ForEach(values.indices, id: \.self) { Text(values[$0]).monospacedDigit().frame(width: groupColumn, alignment: .trailing) }
        }
        .row()
    }
}
