import AppKit
import SwiftUI

struct RefinePage: View {
    @AppStorage("llmEnabled") private var enabled = false
    @AppStorage("llmFormat") private var format = "openai"
    @AppStorage("llmBaseUrl") private var baseUrl = ""
    @AppStorage("llmModel") private var model = ""
    @AppStorage("llmExtraBody") private var extraBody = ""
    @AppStorage("llmTimeoutMs") private var timeoutMs = 4000
    @AppStorage("llmContext") private var context = ""
    @AppStorage("llmAppContext") private var appContext = true
    @AppStorage("llmHistory") private var history = 3
    @AppStorage("llmPrompt") private var prompt = Refine.defaultPrompt
    @AppStorage("llmTemplate") private var template = Refine.defaultTemplate
    @ObservedObject private var records = Records.shared
    @State private var sample = "嗯那个我们周三，不对，周四下午开个会，就是说讨论一下 keep a life 和那个 CI 的问题"
    @State private var result = ""
    @State private var resultMs: Int?
    @State private var failed = false
    @State private var testing = false
    @State private var promptOpen = false
    @State private var templateOpen = false

    private var extraBodyValid: Bool {
        extraBody.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || (try? JSONSerialization.jsonObject(with: Data(extraBody.utf8))) is [String: Any]
    }

    private var statusSubtitle: String {
        guard enabled else { return "关闭时识别结果原样粘贴" }
        let s = "出错或超时会直接粘贴原文；优化中按 esc 跳过"
        let ms = records.all.suffix(50).filter { $0.llm == "ok" }.compactMap(\.llmMs)
        return pct(ms, 0.5).map { s + " · 最近 50 句中位 \($0) ms" } ?? s
    }

    var body: some View {
        Page(title: "优化", subtitle: "粘贴前用 LLM 整理识别结果：去口癖、修正自我纠正、统一术语") {
            SectionBlock("文本处理", footnote: "启用后，识别文本及已配置的上下文会发送给所选 LLM 服务；使用远程接口时会离开本机。「词典 → 后台校对」独立运行，关闭此开关不会关闭后台校对。") {
                RowGroup {
                    SettingRow("粘贴前用 LLM 优化", subtitle: statusSubtitle) {
                        Toggle("", isOn: $enabled).toggleStyle(.switch).labelsHidden()
                    }
                }
            }
            SectionBlock("服务商") {
                Menu("预设") {
                    ForEach(Refine.presets, id: \.name) { p in
                        Button(p.name) { format = p.format; baseUrl = p.url }
                    }
                }
                .fixedSize()
            } content: {
                RowGroup { providerRows }
            }
            SectionBlock("上下文") {
                RowGroup { contextRows }
            }
            SectionBlock("提示词", footnote: "占位符：{{text}} {{context}} {{hotwords}} {{corrections}} {{app}} {{history}}") {
                RowGroup {
                    editorRow("系统提示词", text: $prompt, default: Refine.defaultPrompt, minHeight: 200, isExpanded: $promptOpen)
                    editorRow("用户消息", text: $template, default: Refine.defaultTemplate, minHeight: 140, isExpanded: $templateOpen)
                }
            }
            SectionBlock("测试") {
                RowGroup { testRow }
            }
        }
    }

    @ViewBuilder private var providerRows: some View {
        SettingRow("接口格式") {
            Picker("", selection: $format) {
                Text("OpenAI 兼容").tag("openai")
                Text("Anthropic").tag("anthropic")
            }
            .labelsHidden().fixedSize()
        }
        SettingRow("接口地址") {
            TextField("", text: $baseUrl, prompt: Text("https://ark.cn-beijing.volces.com/api/v3"))
                .textFieldStyle(.roundedBorder).frame(width: 300)
        }
        SettingRow("模型") {
            TextField("", text: $model, prompt: Text("模型或接入点 ID"))
                .textFieldStyle(.roundedBorder).frame(width: 300)
        }
        SettingRow("API key", subtitle: "按接口地址的域名分别保存") { KeyField(account: Refine.keyAccount(baseUrl)) }
        SettingRow("额外请求体", subtitle: extraBodyValid ? "合并进每次请求的 JSON 对象，例如关闭思考" : "不是 JSON 对象",
                   tint: extraBodyValid ? nil : .red) {
            TextField("", text: $extraBody, prompt: Text(#"{"thinking":{"type":"disabled"}}"#), axis: .vertical)
                .font(.body.monospaced()).textFieldStyle(.roundedBorder).frame(width: 300)
        }
        SettingRow("超时", subtitle: "超过即粘贴原文") {
            HStack(spacing: 6) {
                TextField("", value: $timeoutMs, format: .number)
                    .accessibilityLabel("超时（毫秒）")
                    .textFieldStyle(.roundedBorder).frame(width: 70).multilineTextAlignment(.trailing)
                Text("ms").foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private var contextRows: some View {
        StackedRow("关于你", subtitle: "角色、项目、人名、偏好的文风。热词会另外作为词表发送，不用重复。") {
            TextEditor(text: $context)
                .accessibilityLabel("关于你")
                .scrollContentBackground(.hidden).frame(minHeight: 72).inputField()
                .overlay(alignment: .topLeading) {
                    if context.isEmpty {
                        Text("例如：后端工程师，主要写 Go 和 TypeScript；常提到的项目有……")
                            .foregroundStyle(.tertiary).padding(.horizontal, 14).padding(.vertical, 12).allowsHitTesting(false)
                    }
                }
        }
        SettingRow("发送前台应用和窗口标题", subtitle: "让模型按应用调整格式，例如聊天保持口语、邮件用书面句子") {
            Toggle("", isOn: $appContext).toggleStyle(.switch).labelsHidden()
        }
        SettingRow("历史句数", subtitle: "10 分钟内的最近几句作为前文，只用来理解语境") {
            HStack {
                Text("\(history) 句").monospacedDigit()
                Stepper("", value: $history, in: 0...10).labelsHidden()
                    .accessibilityLabel("历史句数").accessibilityValue("\(history) 句")
            }
        }
    }

    private func editorRow(_ title: String, text: Binding<String>, default def: String, minHeight: CGFloat,
                           isExpanded: Binding<Bool>) -> some View {
        DisclosureGroup(isExpanded: isExpanded) {
            TextEditor(text: text).font(.body.monospaced())
                .accessibilityLabel(title)
                .scrollContentBackground(.hidden).frame(minHeight: minHeight).inputField()
                .padding(.top, 8)
            Button("恢复默认") { text.wrappedValue = def }.disabled(text.wrappedValue == def)
                .accessibilityLabel("恢复\(title)默认内容")
                .padding(.top, 8)
        } label: {
            HStack {
                Text(title)
                if text.wrappedValue != def {
                    Text("已修改").font(.caption).padding(.horizontal, 6).padding(.vertical, 2)
                        .foregroundStyle(.orange).background(Capsule().fill(Color.orange.opacity(0.15)))
                }
            }
        }
        .row()
    }

    private var testRow: some View {
        StackedRow("示例文本") {
            TextField("", text: $sample, axis: .vertical).textFieldStyle(.plain).inputField()
                .accessibilityLabel("示例文本")
            HStack {
                Button(testing ? "运行中…" : "运行", action: test).disabled(testing)
                if let ms = resultMs { Text("\(ms) ms").font(.callout.monospacedDigit()).foregroundStyle(.secondary) }
            }
            if !result.isEmpty {
                Text(result).textSelection(.enabled).foregroundStyle(failed ? Color.red : Color.primary)
                    .frame(maxWidth: .infinity, alignment: .leading).inputField()
            }
        }
    }

    private func test() {
        testing = true
        let t0 = Date()
        Refine.run(sample, app: "Plaid 设置", history: []) { r in
            resultMs = Int(Date().timeIntervalSince(t0) * 1000)
            switch r {
            case .success(let t): result = t; failed = false
            case .failure(let e): result = "失败：\(e.localizedDescription)"; failed = true
            }
            testing = false
        }
    }
}
