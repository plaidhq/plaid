import Charts
import SwiftUI

/// Usage dashboard: totals from usage.json, a daily chart, per-app share, and what Plaid has learned.
struct OverviewPage: View {
    @ObservedObject private var records = Records.shared
    @AppStorage("dismissedFixes") private var dismissed = ""
    @AppStorage("llmEnabled") private var llmEnabled = false
    @AppStorage("overviewRange") private var range = 30
    @State private var hover: Date?

    /// Typing speed the "time saved" estimate assumes, characters per minute (pinyin input, mixed text).
    static let typingCPM = 50.0

    private struct DayBar: Identifiable {
        let date: Date
        let u: Usage
        var id: Date { date }
    }

    /// The last `n` local days, oldest first.
    private func days(_ n: Int) -> [DayBar] {
        let cal = Calendar.current, start = cal.startOfDay(for: Date())
        return (0..<n).reversed().map { i in
            let d = cal.date(byAdding: .day, value: -i, to: start)!
            return DayBar(date: d, u: records.usage[Records.day(d)] ?? Usage())
        }
    }

    var body: some View {
        let total = records.usage.values.reduce(Usage(), +)
        Page(title: "概览", subtitle: headline, trailing: { status }) {
            if total.n == 0 {
                VStack(spacing: 10) {
                    IconBadge(symbol: "waveform", color: .accentColor, size: 44)
                    Text("还没有听写记录").font(.headline)
                    Text("按住快捷键说话，松开后文字会粘贴到光标处。").foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, minHeight: 220)
                .card()
            } else {
                tiles(total)
                chart
                // fixedSize lets both cards stretch to the taller one's height inside the ScrollView.
                HStack(alignment: .top, spacing: 16) {
                    apps(total)
                    learned
                }
                .fixedSize(horizontal: false, vertical: true)
                Text("节省时间按打字 \(Int(Self.typingCPM)) 字/分钟估算，减去说话时长。字数：汉字每字计 1，英文和数字每词计 1。清空历史不影响统计。")
                    .font(.callout).foregroundStyle(.secondary)
            }
        }
    }

    /// Rolling last 7 days vs the 7 before, plus the current streak.
    private var headline: String {
        let d = days(14)
        let now = d.suffix(7).reduce(0) { $0 + $1.u.chars }, before = d.prefix(7).reduce(0) { $0 + $1.u.chars }
        var s = "近 7 天听写 \(now.formatted()) 字"
        if before > 0 {
            let p = Int((Double(now - before) / Double(before) * 100).rounded())
            s += p >= 0 ? "，比前 7 天多 \(p)%" : "，比前 7 天少 \(-p)%"
        }
        return s
    }

    /// Consecutive days with dictation, ending today (or yesterday while today is still empty).
    private var streak: Int {
        let d = days(400).reversed().map(\.u.n)
        let tail = d.first == 0 ? Array(d.dropFirst()) : Array(d)
        return tail.prefix { $0 > 0 }.count
    }

    private var status: some View {
        HStack(spacing: 8) {
            if streak > 1 { Pill(symbol: "flame.fill", text: "连续 \(streak) 天", color: .orange) }
            Pill(symbol: "cloud", text: "火山引擎", color: .blue)
            Pill(symbol: "mic.fill", text: Mic.current?.name ?? "无输入设备", color: .secondary)
            if llmEnabled { Pill(symbol: "sparkles", text: "LLM 优化", color: .purple) }
        }
    }

    private func tiles(_ total: Usage) -> some View {
        let today = records.usage[Records.day(Date())] ?? Usage()
        let speed = total.ms > 0 ? Int(Double(total.chars) / (Double(total.ms) / 60000)) : 0
        let saved = max(0, Int(Double(total.chars) / Self.typingCPM * 60000) - total.ms)
        return HStack(spacing: 12) {
            Tile(symbol: "clock.fill", color: .blue, title: "听写时长", value: duration(total.ms),
                 sub: today.n == 0 ? "今天还没有听写" : "今天 \(duration(today.ms))")
            Tile(symbol: "character.cursor.ibeam", color: .orange, title: "听写字数", value: total.chars.formatted(),
                 sub: "今天 \(today.chars.formatted()) · 累计 \(total.n.formatted()) 句")
            Tile(symbol: "gauge.with.dots.needle.67percent", color: .green, title: "平均语速", value: "\(speed)",
                 unit: "字/分", sub: "打字约 \(Int(Self.typingCPM)) 字/分")
            Tile(symbol: "hourglass", color: .purple, title: "节省时间", value: duration(saved),
                 sub: speed > 0 ? "比打字快 \(String(format: "%.1f", Double(speed) / Self.typingCPM)) 倍" : "–")
        }
    }

    private var chart: some View {
        let d = days(range)
        let cal = Calendar.current
        let picked = hover.flatMap { h in d.first { cal.isDate($0.date, inSameDayAs: h) } }
        let sum = d.reduce(0) { $0 + $1.u.chars }
        let stride = range == 7 ? 1 : range == 30 ? 7 : 14
        return VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .center) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("每日字数").font(.headline)
                    Group {
                        if let p = picked {
                            Text("\(p.date.formatted(.dateTime.month().day().weekday()))  \(p.u.chars.formatted()) 字 · \(p.u.n) 句 · \(duration(p.u.ms))")
                        } else {
                            Text("\(sum.formatted()) 字 · 活跃 \(d.filter { $0.u.n > 0 }.count) 天")
                        }
                    }
                    .font(.callout).monospacedDigit().foregroundStyle(.secondary)
                }
                Spacer()
                Picker("", selection: $range) {
                    Text("7 天").tag(7)
                    Text("30 天").tag(30)
                    Text("90 天").tag(90)
                }
                .pickerStyle(.segmented).labelsHidden().fixedSize()
            }
            Chart(d) { b in
                BarMark(x: .value("日期", b.date, unit: .day), y: .value("字数", b.u.chars), width: .ratio(range == 90 ? 0.8 : 0.6))
                    .clipShape(UnevenRoundedRectangle(topLeadingRadius: 3, topTrailingRadius: 3))
                    .foregroundStyle(picked == nil || picked?.date == b.date ? Color.accentColor : Color.accentColor.opacity(0.3))
            }
            .chartXSelection(value: $hover)
            .chartXAxis {
                AxisMarks(values: .stride(by: .day, count: stride)) { _ in
                    AxisValueLabel(format: .dateTime.month(.defaultDigits).day())
                }
            }
            .chartYAxis {
                AxisMarks(position: .leading, values: .automatic(desiredCount: 4)) { _ in
                    AxisGridLine(stroke: StrokeStyle(lineWidth: 0.5, dash: [3, 3])).foregroundStyle(.quaternary)
                    AxisValueLabel()
                }
            }
            .frame(height: 190)
        }
        .card()
    }

    private func apps(_ total: Usage) -> some View {
        let sorted = total.apps.sorted { $0.value > $1.value }
        var rows = sorted.prefix(5).map { (name: Optional($0.key), chars: $0.value) }
        let rest = sorted.dropFirst(5).reduce(0) { $0 + $1.value }
        if rest > 0 { rows.append((nil, rest)) }
        let sum = Double(max(1, total.chars))
        return VStack(alignment: .leading, spacing: 12) {
            Text("常用应用").font(.headline)
            ForEach(rows, id: \.name) { r in
                HStack(spacing: 10) {
                    if let n = r.name {
                        Image(nsImage: AppIcon.image(n)).resizable().frame(width: 22, height: 22)
                    } else {
                        Image(systemName: "ellipsis.circle.fill").font(.system(size: 18)).foregroundStyle(.tertiary).frame(width: 22, height: 22)
                    }
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            Text(r.name ?? "其他").lineLimit(1)
                            Spacer()
                            Text("\(Int((Double(r.chars) / sum * 100).rounded()))%").monospacedDigit().foregroundStyle(.secondary)
                        }
                        Bar(share: Double(r.chars) / sum)
                    }
                }
            }
        }
        .frame(maxHeight: .infinity, alignment: .top)
        .card()
    }

    private var learned: some View {
        let edits = records.all.filter { $0.edited != nil }.count
        let pending = pendingFixes(records, dismissed: dismissed).count
        return VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text("个性化").font(.headline)
                Spacer()
                Button("打开词典") { SettingsNav.shared.page = .dictionary }.buttonStyle(.link)
            }
            LearnRow(symbol: "character.book.closed.fill", color: .blue, name: "热词", value: "\(Hotwords.terms(auto: false).count) 个")
            LearnRow(symbol: "wand.and.stars", color: .purple, name: "自动规则", value: "\(records.autoRules().count) 条")
            LearnRow(symbol: "pencil.line", color: .green, name: "手动修改", value: "\(edits) 句")
            LearnRow(symbol: "lightbulb.fill", color: pending > 0 ? .orange : .gray, name: "待处理建议", value: "\(pending) 条")
        }
        .frame(maxHeight: .infinity, alignment: .top)
        .card()
    }
}

private struct Tile: View {
    let symbol: String, color: Color, title: String, value: String
    var unit: String?
    let sub: String

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                IconBadge(symbol: symbol, color: color, size: 24)
                Text(title).font(.callout).foregroundStyle(.secondary)
            }
            HStack(alignment: .firstTextBaseline, spacing: 3) {
                Text(value).font(.system(size: 26, weight: .semibold, design: .rounded)).monospacedDigit()
                if let unit { Text(unit).font(.callout).foregroundStyle(.secondary) }
            }
            .lineLimit(1).minimumScaleFactor(0.6)
            Text(sub).font(.caption).foregroundStyle(.secondary).lineLimit(1)
        }
        .card()
    }
}

private struct Pill: View {
    let symbol: String, text: String, color: Color

    var body: some View {
        Label(text, systemImage: symbol)
            .font(.caption.weight(.medium))
            .lineLimit(1)
            .foregroundStyle(color == .secondary ? Color.secondary : color)
            .padding(.horizontal, 9).padding(.vertical, 4)
            .background(Capsule().fill((color == .secondary ? Color.gray : color).opacity(0.12)))
    }
}

private struct LearnRow: View {
    let symbol: String, color: Color, name: String, value: String

    var body: some View {
        HStack(spacing: 10) {
            IconBadge(symbol: symbol, color: color, size: 24)
            Text(name)
            Spacer()
            Text(value).monospacedDigit().foregroundStyle(.secondary)
        }
    }
}

private struct Bar: View {
    let share: Double

    var body: some View {
        GeometryReader { g in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.gray.opacity(0.15))
                Capsule().fill(Color.accentColor).frame(width: max(4, g.size.width * share))
            }
        }
        .frame(height: 6)
    }
}
