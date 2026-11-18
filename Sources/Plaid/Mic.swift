import AVFoundation
import CoreAudio

/// Input device choice: defaults `micDevice` (Core Audio device UID), unset = follow the system default input.
enum Mic {
    struct Device: Hashable {
        let id: AudioDeviceID
        let uid: String
        let name: String
    }

    /// p90 of the 100 ms buffer RMS below this means the input barely carries speech. A friend's bundle recorded through a
    /// Bluetooth remote's loopback device ("MiRemoteV 2ch") had speech p90 68-98 and half the clips came back empty;
    /// 52 own recordings (built-in mic, VP on and off) are all >= 249.
    static let quietRMS = 150

    /// Microphones as the system lists them. Core Audio's device list also has process-private aggregates and,
    /// under voice processing, the speaker's reference input, so the list comes from AVCapture (its uniqueID is the Core Audio UID).
    static func inputs() -> [Device] {
        AVCaptureDevice.DiscoverySession(deviceTypes: [.microphone, .external], mediaType: .audio, position: .unspecified)
            .devices.filter { !$0.uniqueID.hasPrefix("CADefaultDeviceAggregate") && !$0.uniqueID.hasPrefix("VPAUAggregateAudioDevice") }
            .compactMap { d in deviceID(uid: d.uniqueID).map { Device(id: $0, uid: d.uniqueID, name: d.localizedName) } }
    }

    static var systemDefault: Device? {
        var addr = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultInputDevice, mScope: kAudioObjectPropertyScopeGlobal,
                                              mElement: kAudioObjectPropertyElementMain)
        var id = AudioDeviceID(0), size = UInt32(MemoryLayout<AudioDeviceID>.size)
        guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &addr, 0, nil, &size, &id) == noErr, id != 0 else { return nil }
        return device(id)
    }

    /// The chosen device if it is connected; nil = system default.
    static var chosen: Device? {
        guard let uid = Config.value("micDevice") else { return nil }
        return inputs().first { $0.uid == uid }
    }

    /// The device the next utterance records from.
    static var current: Device? { chosen ?? systemDefault }

    /// A chosen device other than the system default, i.e. one the engine has to be pointed at; nil = default input.
    static var override: Device? { override(chosen: chosen, systemDefault: systemDefault) }

    /// Device names are not unique. Use the same Core Audio identity in recording and settings.
    static func override(chosen: Device?, systemDefault: Device?) -> Device? {
        guard let chosen, chosen.id != systemDefault?.id else { return nil }
        return chosen
    }

    /// Retains the exact listener block needed for removal. Start/stop on the main queue with the settings window.
    final class DefaultInputObserver {
        private var listener: AudioObjectPropertyListenerBlock?

        func start(_ changed: @escaping () -> Void) {
            guard listener == nil else { return }
            var address = Self.address
            let block: AudioObjectPropertyListenerBlock = { _, _ in changed() }
            guard AudioObjectAddPropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &address, .main, block) == noErr else { return }
            listener = block
        }

        func stop() {
            guard let listener else { return }
            var address = Self.address
            AudioObjectRemovePropertyListenerBlock(AudioObjectID(kAudioObjectSystemObject), &address, .main, listener)
            self.listener = nil
        }

        deinit { stop() }

        private static var address: AudioObjectPropertyAddress {
            AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultInputDevice, mScope: kAudioObjectPropertyScopeGlobal,
                                       mElement: kAudioObjectPropertyElementMain)
        }
    }

    /// Points a fresh engine's input at `dev`.
    static func apply(_ dev: Device, to input: AVAudioInputNode) -> Bool {
        guard let unit = input.audioUnit else { return false }
        var id = dev.id
        return AudioUnitSetProperty(unit, kAudioOutputUnitProperty_CurrentDevice, kAudioUnitScope_Global, 0,
                                    &id, UInt32(MemoryLayout<AudioDeviceID>.size)) == noErr
    }

    private static func deviceID(uid: String) -> AudioDeviceID? {
        var addr = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyTranslateUIDToDevice, mScope: kAudioObjectPropertyScopeGlobal,
                                              mElement: kAudioObjectPropertyElementMain)
        var cf = uid as CFString, id = AudioDeviceID(0), size = UInt32(MemoryLayout<AudioDeviceID>.size)
        let st = withUnsafePointer(to: &cf) {
            AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &addr, UInt32(MemoryLayout<CFString>.size), $0, &size, &id)
        }
        return st == noErr && id != kAudioObjectUnknown ? id : nil
    }

    private static func device(_ id: AudioDeviceID) -> Device? {
        guard let uid = string(id, kAudioDevicePropertyDeviceUID) else { return nil }
        return Device(id: id, uid: uid, name: string(id, kAudioObjectPropertyName) ?? uid)
    }

    private static func string(_ id: AudioObjectID, _ selector: AudioObjectPropertySelector) -> String? {
        var addr = AudioObjectPropertyAddress(mSelector: selector, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
        var s: Unmanaged<CFString>?
        var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        guard AudioObjectGetPropertyData(id, &addr, 0, nil, &size, &s) == noErr, let s else { return nil }
        return s.takeRetainedValue() as String
    }
}

