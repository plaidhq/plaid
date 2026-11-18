import AVFoundation

/// Streams 16 kHz mono s16le PCM as Ogg/Opus (RFC 7845), about 10x smaller than raw PCM.
/// Each `encode` call emits one Ogg page holding every complete 20 ms frame; the first call
/// is prefixed with the OpusHead/OpusTags header pages.
final class OggOpus {
    private static let frame = 320  // 20 ms at 16 kHz
    private let conv: AVAudioConverter
    private let pcmFormat = Resampler.target
    private let opusFormat: AVAudioFormat
    private var pending = Data()
    private var pageSeq: UInt32 = 0
    private var granule: Int64 = 0
    private let serial = UInt32.random(in: 0...UInt32.max)

    init(bitRate: Int = 32000) {
        var desc = AudioStreamBasicDescription(
            mSampleRate: 16000, mFormatID: kAudioFormatOpus, mFormatFlags: 0, mBytesPerPacket: 0,
            mFramesPerPacket: UInt32(Self.frame), mBytesPerFrame: 0, mChannelsPerFrame: 1, mBitsPerChannel: 0, mReserved: 0)
        opusFormat = AVAudioFormat(streamDescription: &desc)!
        conv = AVAudioConverter(from: pcmFormat, to: opusFormat)!
        conv.bitRate = bitRate
    }

    func encode(_ pcm: Data, last: Bool = false) -> Data {
        var out = Data()
        if pageSeq == 0 {
            var head = Data("OpusHead".utf8) + [1, 1]  // version, channels
            head.append(le: UInt16(312))  // pre-skip (libopus default lookahead at 48 kHz)
            head.append(le: UInt32(16000))
            head.append(contentsOf: [0, 0, 0])  // gain, mapping family
            var tags = Data("OpusTags".utf8)
            tags.append(le: UInt32(5))
            tags.append(Data("plaid".utf8))
            tags.append(le: UInt32(0))
            out += page([head], flags: 0x02)
            out += page([tags], flags: 0)
        }
        pending.append(pcm)
        if last, pending.count % (Self.frame * 2) != 0 {
            pending.append(Data(count: Self.frame * 2 - pending.count % (Self.frame * 2)))
        }
        let usable = pending.count / (Self.frame * 2) * Self.frame
        var packets: [Data] = []
        if usable > 0 {
            let buf = AVAudioPCMBuffer(pcmFormat: pcmFormat, frameCapacity: AVAudioFrameCount(usable))!
            buf.frameLength = AVAudioFrameCount(usable)
            pending.withUnsafeBytes { buf.int16ChannelData![0].update(from: $0.bindMemory(to: Int16.self).baseAddress!, count: usable) }
            pending.removeFirst(usable * 2)
            packets = encodePackets(buf)
        }
        granule += Int64(packets.count) * 960  // granule counts 48 kHz samples
        if !packets.isEmpty || last { out += page(packets, flags: last ? 0x04 : 0) }
        return out
    }

    private func encodePackets(_ buf: AVAudioPCMBuffer) -> [Data] {
        let n = Int(buf.frameLength) / Self.frame
        let out = AVAudioCompressedBuffer(format: opusFormat, packetCapacity: AVAudioPacketCount(n + 1), maximumPacketSize: max(conv.maximumOutputPacketSize, 1500))
        var fed = false
        conv.convert(to: out, error: nil) { _, status in
            if fed { status.pointee = .noDataNow; return nil }
            fed = true
            status.pointee = .haveData
            return buf
        }
        return (0..<Int(out.packetCount)).map { i in
            let d = out.packetDescriptions![i]
            return Data(bytes: out.data + Int(d.mStartOffset), count: Int(d.mDataByteSize))
        }
    }

    // ponytail: one page per call; 255 lacing values caps a page at ~250 packets (5 s), plenty for 200 ms chunks.
    private func page(_ packets: [Data], flags: UInt8) -> Data {
        var lacing: [UInt8] = []
        for p in packets {
            lacing += Array(repeating: 255, count: p.count / 255) + [UInt8(p.count % 255)]
        }
        var d = Data("OggS".utf8) + [0, flags]
        d.append(le: UInt64(bitPattern: pageSeq < 2 ? 0 : granule))
        d.append(le: serial)
        d.append(le: pageSeq)
        d.append(le: UInt32(0))  // crc placeholder
        d.append(UInt8(lacing.count))
        d.append(contentsOf: lacing)
        packets.forEach { d.append($0) }
        let crc = Self.crc(d)
        d.replaceSubrange(22..<26, with: withUnsafeBytes(of: crc.littleEndian) { Data($0) })
        pageSeq += 1
        return d
    }

    private static let table: [UInt32] = (0..<256).map { i in
        (0..<8).reduce(UInt32(i) << 24) { c, _ in c & 0x8000_0000 != 0 ? c << 1 ^ 0x04C1_1DB7 : c << 1 }
    }

    static func crc(_ d: Data) -> UInt32 {
        d.reduce(0) { c, b in c << 8 ^ table[Int(UInt8(c >> 24) ^ b)] }
    }
}

extension Data {
    mutating func append<T: FixedWidthInteger>(le v: T) {
        Swift.withUnsafeBytes(of: v.littleEndian) { append(contentsOf: $0) }
    }
}
