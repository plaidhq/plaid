import AppKit
import AVFoundation
import SwiftUI

/// Write-only key field: the stored key is never shown, only whether one exists.
struct KeyField: View {
    let account: String
    @State private var text = ""
    @State private var stored = false
    @State private var editing = false

    var body: some View {
        HStack {
            if stored && !editing {
                Label {
                    Text("已存入钥匙串").foregroundStyle(.secondary)
                } icon: {
                    Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
                }
                Button("更换…") { editing = true }
                Button("删除") { Keychain.set("", account: account); stored = false }
            } else {
                SecureField(stored ? "输入新的 key" : "粘贴 API key", text: $text)
                    .accessibilityLabel("API key")
                    .textFieldStyle(.roundedBorder).frame(width: 260).onSubmit(save)
                Button("保存", action: save).disabled(text.isEmpty)
                if stored { Button("取消") { editing = false; text = "" } }
            }
        }
        .task(id: account) {
            text = ""
            editing = false
            stored = Keychain.get(account) != nil
        }
    }

    private func save() {
        let k = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !k.isEmpty else { return }
        Keychain.set(k, account: account)
        text = ""
        stored = true
        editing = false
    }
}

/// Click, then press a single key. Esc cancels; ⏎, esc and Caps Lock can't be bound.
struct KeyRecorder: View {
    let key: String
    @AppStorage private var code: Int
    @AppStorage private var name: String
    @State private var monitor: Any?

    init(key: String) {
        self.key = key
        _code = AppStorage(wrappedValue: 61, key)
        _name = AppStorage(wrappedValue: "右 ⌥", key + "Name")
    }

    var body: some View {
        HStack(spacing: 6) {
            let b = Button(action: record) {
                Text(monitor != nil ? "请按一个键…" : code < 0 ? "未绑定" : Hotkeys.modifiers[code]?.name ?? name).frame(minWidth: 110)
            }
            .accessibilityLabel(key == "holdKey" ? "录制按住说话快捷键" : "录制轻点开关快捷键")
            .accessibilityValue(monitor != nil ? "请按一个键，esc 取消" : code < 0 ? "未绑定" : Hotkeys.modifiers[code]?.name ?? name)
            if monitor != nil { b.buttonStyle(.borderedProminent) } else { b.buttonStyle(.bordered) }
            if code >= 0 {
                Button { stop(); code = -1 } label: { Image(systemName: "xmark.circle.fill") }
                    .buttonStyle(.borderless).foregroundStyle(.secondary).help("不绑定")
                    .accessibilityLabel(key == "holdKey" ? "清除按住说话快捷键" : "清除轻点开关快捷键")
            }
        }
        .onDisappear(perform: stop)
    }

    private func record() {
        guard monitor == nil else { return stop() }
        Hotkeys.stopRecording?()
        Hotkeys.stopRecording = stop
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .flagsChanged]) { e in
            let c = Int(e.keyCode)
            if e.type == .flagsChanged {
                guard Hotkeys.modifierDown(c, e.modifierFlags) == true else { return e }
            } else if c == 53 {
                stop()
                return nil
            } else if [36, 76, 57].contains(c) {
                return nil
            }
            code = c
            name = Hotkeys.name(e)
            stop()
            return nil
        }
    }

    private func stop() {
        guard let m = monitor else { return }  // not this recorder's capture (onDisappear of an idle one)
        NSEvent.removeMonitor(m)
        monitor = nil
        Hotkeys.stopRecording = nil
    }
}

struct GeneralPage: View {
    @AppStorage("nonstream") private var nonstream = "1"
    @AppStorage("micDevice") private var micDevice = ""  // device UID, empty = system default
    @State private var mics = Mic.inputs()
    @State private var systemMic = Mic.systemDefault
    @State private var defaultInputObserver = Mic.DefaultInputObserver()
    @State private var showAdvanced = false
    @AppStorage("codec") private var codec = "opus"
    @AppStorage("asrContext") private var asrContext = "1"
    @AppStorage("keepAudio") private var keepAudio = "1"
    @AppStorage("silenceRMS") private var silenceRMS = ""  // empty: Config.silenceDefault
    @AppStorage("tailMs") private var tailMs = "300"
    @ObservedObject private var records = Records.shared

    var body: some View {
        Page(title: "设置", subtitle: "快捷键、识别引擎、麦克风、数据") {
            SectionBlock("快捷键", footnote: "仅单独按键触发；同一个键可以同时用于按住和轻点。") {
                RowGroup {
                    SettingRow("按住说话", subtitle: "单独按住 300 ms 后开始，松开后粘贴") { KeyRecorder(key: "holdKey") }
                    SettingRow("轻点开关", subtitle: "短按开始，再按一次或 ⏎ 结束；esc 取消") { KeyRecorder(key: "tapKey") }
                }
                .help("修饰键无法被系统拦截，fn 可能同时触发系统功能。与其他键组合使用不会启动听写。")
            }
            SectionBlock("语音识别（火山引擎）") {
                RowGroup { asrRows }
            }
            SectionBlock("麦克风", footnote: "仅影响 Plaid，不更改系统输入设备。") {
                RowGroup { micRows }
            }
            DisclosureGroup(isExpanded: $showAdvanced) {
                VStack(alignment: .leading, spacing: Metrics.gap) {
                    SectionBlock("云端识别") { RowGroup { cloudAdvancedRows } }
                    SectionBlock("松键收尾", footnote: "松键后短暂保留录音，等待静音或达到时限，避免吞掉最后一个字。") {
                        RowGroup { tailRows }
                    }
                }
                .padding(.top, 12)
            } label: {
                Label("高级设置", systemImage: "slider.horizontal.3").font(.headline)
            }
            .accessibilityIdentifier("general.advanced")
            SectionBlock("数据", footnote: "云端识别会发送音频及已启用的上下文；LLM 优化和后台校对可能发送识别文本、热词及上下文。后台校对独立于优化开关，可在「词典」中管理。") {
                RowGroup {
                    SettingRow("保留最近 500 条录音", subtitle: "只存本机，用于回放和反馈问题") {
                        Toggle("", isOn: flag($keepAudio)).toggleStyle(.switch).labelsHidden()
                    }
                    SettingRow("数据文件夹", subtitle: "录音、历史、热词都在 ~/Library/Application Support/Plaid") {
                        Button("在访达中显示") { NSWorkspace.shared.activateFileViewerSelecting([Records.url]) }
                    }
                }
            }
        }
        .background(WindowVisibility { visible in
            if visible {
                defaultInputObserver.start { refreshMics() }
                refreshMics()
            } else {
                defaultInputObserver.stop()
            }
        }.frame(width: 0, height: 0))
    }

    @ViewBuilder private var asrRows: some View {
        SettingRow("API key") { KeyField(account: "apiKey") }
        SettingRow("发送上下文", subtitle: "发送光标前文字、最近几句和应用名，帮助断句和选词") {
            Toggle("", isOn: flag($asrContext)).toggleStyle(.switch).labelsHidden()
        }
    }

    @ViewBuilder private var cloudAdvancedRows: some View {
        SettingRow("二遍识别", subtitle: "松键后再识别整句，难词更准，约慢 300 ms") {
            Toggle("", isOn: flag($nonstream)).toggleStyle(.switch).labelsHidden()
        }
        SettingRow("上传格式", subtitle: "PCM 无损，流量约为 Opus 的 8 倍") {
            Picker("", selection: $codec) {
                Text("Opus 32 kbps").tag("opus")
                Text("PCM").tag("pcm")
            }
            .labelsHidden().fixedSize()
        }
    }

    @ViewBuilder private var micRows: some View {
        SettingRow("输入设备") {
            Picker("", selection: $micDevice) {
                Text("跟随系统（\(systemMic?.name ?? "无")）").tag("")
                ForEach(mics, id: \.uid) { Text($0.name).tag($0.uid) }
                if !micDevice.isEmpty, !mics.contains(where: { $0.uid == micDevice }) {
                    Text("已选设备未连接，暂用系统默认").tag(micDevice)
                }
            }
            .labelsHidden().fixedSize()
            .onReceive(NotificationCenter.default.publisher(for: AVCaptureDevice.wasConnectedNotification)) { _ in refreshMics() }
            .onReceive(NotificationCenter.default.publisher(for: AVCaptureDevice.wasDisconnectedNotification)) { _ in refreshMics() }
            .onAppear { refreshMics() }
        }
    }

    @ViewBuilder private var tailRows: some View {
        let threshold = Int(silenceRMS) ?? Int(Config.silenceDefault)
        let last = records.all.last(where: { $0.noiseRMS != nil && $0.speechRMS != nil })
        if let r = last, let n = r.noiseRMS, let sp = r.speechRMS {
            SettingRow("静音阈值", subtitle: "上一句噪声约 \(n)，语音约 \(sp)（RMS）；阈值放在两者之间、靠近噪声", tint: sp < threshold ? .orange : nil) {
                silenceSlider
            }
        } else {
            SettingRow("静音阈值", subtitle: "太高会丢字，太低会变慢") { silenceSlider }
        }
        SettingRow("最长收尾", subtitle: "松键后最多再录这么久（ms）") {
            HStack {
                Slider(value: num($tailMs, 300), in: 100...1000).frame(width: 200)
                    .accessibilityLabel("最长收尾").accessibilityValue("\(tailMs) 毫秒")
                Text(tailMs).monospacedDigit().frame(width: 40, alignment: .trailing)
            }
        }
        if let r = last, let sp = r.speechRMS, sp < Mic.quietRMS {
            SettingRow("语音电平过低", subtitle: "\(r.mic ?? "当前设备")：识别会漏字或整句为空。换一个输入设备，或调高该设备的输入音量。", tint: .orange)
        }
    }

    private var silenceSlider: some View {
        HStack {
            Slider(value: num($silenceRMS, Config.silenceDefault), in: 0...1500).frame(width: 200)
                .accessibilityLabel("静音阈值")
                .accessibilityValue("\(Int(Double(silenceRMS) ?? Config.silenceDefault)) RMS")
            Text(silenceRMS.isEmpty ? String(Int(Config.silenceDefault)) : silenceRMS).monospacedDigit().frame(width: 40, alignment: .trailing)
        }
    }
}

extension GeneralPage {
    fileprivate func refreshMics() {
        mics = Mic.inputs()
        systemMic = Mic.systemDefault
    }
}

private func num(_ b: Binding<String>, _ fallback: Double) -> Binding<Double> {
    Binding(get: { Double(b.wrappedValue) ?? fallback }, set: { b.wrappedValue = String(Int(($0 / 10).rounded()) * 10) })
}

private func flag(_ b: Binding<String>) -> Binding<Bool> {
    Binding(get: { b.wrappedValue == "1" }, set: { b.wrappedValue = $0 ? "1" : "0" })
}

