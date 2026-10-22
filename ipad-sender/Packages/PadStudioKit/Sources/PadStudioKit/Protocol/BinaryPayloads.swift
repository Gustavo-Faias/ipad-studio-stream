import Foundation

// Payloads binários (PROTOCOL.md §4). Os encoders produzem exatamente os bytes dos vetores.

/// Codec de vídeo. No binário é `1`/`2`; no JSON é `"h264"`/`"hevc"`.
public enum VideoCodec: UInt8, CaseIterable, Sendable, Codable {
    case h264 = 1
    case hevc = 2

    public var name: String {
        switch self {
        case .h264: "h264"
        case .hevc: "hevc"
        }
    }

    public init?(name: String) {
        switch name {
        case "h264": self = .h264
        case "hevc": self = .hevc
        default: return nil
        }
    }

    public init(from decoder: Decoder) throws {
        let s = try decoder.singleValueContainer().decode(String.self)
        guard let codec = VideoCodec(name: s) else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "codec desconhecido: \(s)"))
        }
        self = codec
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        try c.encode(name)
    }
}

/// Rotação que o PC aplica para mostrar a imagem direita (bits 4–5 de `VIDEO_FRAME.flags`).
public enum Rotation: UInt8, Sendable {
    case none = 0
    case clockwise90 = 1
    case upsideDown = 2
    case clockwise270 = 3

    public var degrees: Int { Int(rawValue) * 90 }

    /// A partir de `RPVideoSampleOrientationKey` (`CGImagePropertyOrientation`, semântica EXIF).
    /// Tabela de PROTOCOL.md §4.4; as orientações espelhadas não aparecem no ReplayKit.
    public init(exifOrientation: UInt32) {
        switch exifOrientation {
        case 3: self = .upsideDown // .down
        case 6: self = .clockwise90 // .right
        case 8: self = .clockwise270 // .left
        default: self = .none // .up e desconhecidas
        }
    }
}

/// Flags de `VIDEO_FRAME` (PROTOCOL.md §4.4).
public struct VideoFlags: Equatable, Sendable {
    public var keyframe = false
    public var paramsInband = false
    public var discontinuity = false
    public var rotation = Rotation.none

    public init(keyframe: Bool = false, paramsInband: Bool = false, discontinuity: Bool = false, rotation: Rotation = .none) {
        self.keyframe = keyframe
        self.paramsInband = paramsInband
        self.discontinuity = discontinuity
        self.rotation = rotation
    }

    public init(rawValue: UInt8) {
        keyframe = rawValue & 0x01 != 0
        paramsInband = rawValue & 0x02 != 0
        discontinuity = rawValue & 0x04 != 0
        rotation = Rotation(rawValue: (rawValue & 0x30) >> 4) ?? .none
    }

    public var rawValue: UInt8 {
        (keyframe ? 0x01 : 0) | (paramsInband ? 0x02 : 0) | (discontinuity ? 0x04 : 0) | (rotation.rawValue << 4)
    }
}

/// Fonte de áudio (`AUDIO_CONFIG.source`, bits 0–3 de `AUDIO_FRAME.flags`).
public enum AudioSource: UInt8, Sendable {
    case app = 0
    case mic = 1
}

/// Flags de `AUDIO_FRAME` (PROTOCOL.md §4.6).
public struct AudioFlags: Equatable, Sendable {
    public var source: UInt8
    public var discontinuity: Bool

    public init(source: UInt8, discontinuity: Bool = false) {
        self.source = source
        self.discontinuity = discontinuity
    }

    public init(rawValue: UInt8) {
        source = rawValue & 0x0f
        discontinuity = rawValue & 0x10 != 0
    }

    public var rawValue: UInt8 { (source & 0x0f) | (discontinuity ? 0x10 : 0) }
}

// ── Leitura e escrita little-endian ─────────────────────────────────────────

struct ByteWriter {
    var data: Data

    init(capacity: Int) {
        data = Data(capacity: capacity)
    }

    mutating func u8(_ v: UInt8) { data.append(v) }
    mutating func u16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { data.append(contentsOf: $0) } }
    mutating func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { data.append(contentsOf: $0) } }
    mutating func u64(_ v: UInt64) { withUnsafeBytes(of: v.littleEndian) { data.append(contentsOf: $0) } }
    mutating func bytes(_ d: Data) { data.append(d) }
}

struct ByteReader {
    let data: Data

    /// Garante pelo menos `n` bytes (PROTOCOL.md §2.2, regra 5: bytes a mais são ignorados).
    init(_ data: Data, minimum n: Int, type: MessageType) throws {
        guard data.count >= n else {
            throw ProtocolError.badPayload("payload de \(type.name) com \(data.count) bytes (mínimo \(n))")
        }
        self.data = data
    }

    func u8(_ at: Int) -> UInt8 { data[data.startIndex + at] }

    func u16(_ at: Int) -> UInt16 { UInt16(u8(at)) | UInt16(u8(at + 1)) << 8 }

    func u32(_ at: Int) -> UInt32 { UInt32(u16(at)) | UInt32(u16(at + 2)) << 16 }

    func u64(_ at: Int) -> UInt64 { UInt64(u32(at)) | UInt64(u32(at + 4)) << 32 }

    func rest(from at: Int) -> Data { data.subdata(in: data.startIndex + at ..< data.endIndex) }
}

// ── Payloads ────────────────────────────────────────────────────────────────

/// `PING` (8 bytes).
public struct Ping: Equatable, Sendable {
    public var t0: UInt64

    public init(t0: UInt64) { self.t0 = t0 }

    public func encode() -> Data {
        var w = ByteWriter(capacity: 8)
        w.u64(t0)
        return w.data
    }

    public static func decode(_ p: Data) throws -> Ping {
        let r = try ByteReader(p, minimum: 8, type: .ping)
        return Ping(t0: r.u64(0))
    }
}

/// `PONG` (24 bytes).
public struct Pong: Equatable, Sendable {
    public var t0: UInt64
    public var t1: UInt64
    public var t2: UInt64

    public init(t0: UInt64, t1: UInt64, t2: UInt64) {
        self.t0 = t0
        self.t1 = t1
        self.t2 = t2
    }

    public func encode() -> Data {
        var w = ByteWriter(capacity: 24)
        w.u64(t0)
        w.u64(t1)
        w.u64(t2)
        return w.data
    }

    public static func decode(_ p: Data) throws -> Pong {
        let r = try ByteReader(p, minimum: 24, type: .pong)
        return Pong(t0: r.u64(0), t1: r.u64(8), t2: r.u64(16))
    }
}

/// `VIDEO_CONFIG` (16 bytes + parâmetros em Anexo-B).
public struct VideoConfig: Equatable, Sendable {
    public var codec: VideoCodec
    public var bitDepth: UInt8 = 8
    public var fpsMax: UInt16
    public var width: UInt16
    public var height: UInt16
    public var bitrateBps: UInt32
    /// Códigos ITU-T H.273.
    public var colorPrimaries: UInt8 = 1
    public var transfer: UInt8 = 1
    public var matrix: UInt8 = 1
    public var fullRange = false
    /// VPS (só HEVC) + SPS + PPS em Anexo-B.
    public var paramSets: Data

    public init(
        codec: VideoCodec, bitDepth: UInt8 = 8, fpsMax: UInt16, width: UInt16, height: UInt16, bitrateBps: UInt32,
        colorPrimaries: UInt8 = 1, transfer: UInt8 = 1, matrix: UInt8 = 1, fullRange: Bool = false, paramSets: Data
    ) {
        self.codec = codec
        self.bitDepth = bitDepth
        self.fpsMax = fpsMax
        self.width = width
        self.height = height
        self.bitrateBps = bitrateBps
        self.colorPrimaries = colorPrimaries
        self.transfer = transfer
        self.matrix = matrix
        self.fullRange = fullRange
        self.paramSets = paramSets
    }

    public func encode() -> Data {
        var w = ByteWriter(capacity: 16 + paramSets.count)
        w.u8(codec.rawValue)
        w.u8(bitDepth)
        w.u16(fpsMax)
        w.u16(width)
        w.u16(height)
        w.u32(bitrateBps)
        w.u8(colorPrimaries)
        w.u8(transfer)
        w.u8(matrix)
        w.u8(fullRange ? 1 : 0)
        w.bytes(paramSets)
        return w.data
    }

    public static func decode(_ p: Data) throws -> VideoConfig {
        let r = try ByteReader(p, minimum: 16, type: .videoConfig)
        guard let codec = VideoCodec(rawValue: r.u8(0)) else {
            throw ProtocolError.badPayload("codec inválido em VIDEO_CONFIG: \(r.u8(0))")
        }
        return VideoConfig(
            codec: codec, bitDepth: r.u8(1), fpsMax: r.u16(2), width: r.u16(4), height: r.u16(6), bitrateBps: r.u32(8),
            colorPrimaries: r.u8(12), transfer: r.u8(13), matrix: r.u8(14), fullRange: r.u8(15) == 1,
            paramSets: r.rest(from: 16)
        )
    }
}

/// `AUDIO_CONFIG` (8 bytes).
public struct AudioConfig: Equatable, Sendable {
    public enum Format: UInt8, Sendable {
        case s16le = 1
        case f32le = 2
    }

    public var source: UInt8
    public var format: UInt8
    public var channels: UInt8
    public var sampleRate: UInt32

    public init(source: UInt8, format: UInt8, channels: UInt8, sampleRate: UInt32) {
        self.source = source
        self.format = format
        self.channels = channels
        self.sampleRate = sampleRate
    }

    public func encode() -> Data {
        var w = ByteWriter(capacity: 8)
        w.u8(source)
        w.u8(format)
        w.u8(channels)
        w.u8(0)
        w.u32(sampleRate)
        return w.data
    }

    public static func decode(_ p: Data) throws -> AudioConfig {
        let r = try ByteReader(p, minimum: 8, type: .audioConfig)
        return AudioConfig(source: r.u8(0), format: r.u8(1), channels: r.u8(2), sampleRate: r.u32(4))
    }
}

/// `SET_BITRATE` (4 bytes).
public struct SetBitrate: Equatable, Sendable {
    public var bitrateBps: UInt32

    public init(bitrateBps: UInt32) { self.bitrateBps = bitrateBps }

    public func encode() -> Data {
        var w = ByteWriter(capacity: 4)
        w.u32(bitrateBps)
        return w.data
    }

    public static func decode(_ p: Data) throws -> SetBitrate {
        let r = try ByteReader(p, minimum: 4, type: .setBitrate)
        return SetBitrate(bitrateBps: r.u32(0))
    }
}
