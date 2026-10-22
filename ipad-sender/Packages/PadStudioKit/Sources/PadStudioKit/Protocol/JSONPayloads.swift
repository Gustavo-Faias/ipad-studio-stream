import Foundation

// Payloads JSON (PROTOCOL.md §5). Campos opcionais a `nil` não são escritos; campos desconhecidos
// são ignorados; valores de enumeração desconhecidos viram `.unknown`.

public enum Transport: String, Codable, Sendable {
    case usb, lan, unknown

    public init(from decoder: Decoder) throws {
        self = Transport(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

public enum BitrateMode: String, Codable, Sendable {
    case vbr, cbr, unknown

    public init(from decoder: Decoder) throws {
        self = BitrateMode(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

public enum ByeReason: String, Codable, Sendable {
    case userStopped = "user_stopped"
    case replaced
    case unauthorized
    case protocolVersion = "protocol_version"
    case timeout
    case shutdown
    case error
    case unknown

    public init(from decoder: Decoder) throws {
        self = ByeReason(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

public enum BroadcastState: String, Codable, Sendable {
    case active, paused, finishing, unknown

    public init(from decoder: Decoder) throws {
        self = BroadcastState(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

public enum ThermalState: String, Codable, Sendable {
    case nominal, fair, serious, critical, unknown

    public init(from decoder: Decoder) throws {
        self = ThermalState(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .unknown
    }
}

/// Configuração do encoder: objeto de `CONFIGURE`, `HELLO.config` e `HELLO_ACK.config`.
/// Campos em falta mantêm o valor atual.
public struct StreamConfig: Codable, Equatable, Sendable {
    public var codec: VideoCodec?
    public var maxFps: Int?
    public var minFps: Int?
    public var bitrateBps: Int?
    public var bitrateMode: BitrateMode?
    public var maxWidth: Int?
    public var maxHeight: Int?
    public var keyframeIntervalS: Double?
    public var audioApp: Bool?
    public var audioMic: Bool?

    public init(
        codec: VideoCodec? = nil, maxFps: Int? = nil, minFps: Int? = nil, bitrateBps: Int? = nil,
        bitrateMode: BitrateMode? = nil, maxWidth: Int? = nil, maxHeight: Int? = nil,
        keyframeIntervalS: Double? = nil, audioApp: Bool? = nil, audioMic: Bool? = nil
    ) {
        self.codec = codec
        self.maxFps = maxFps
        self.minFps = minFps
        self.bitrateBps = bitrateBps
        self.bitrateMode = bitrateMode
        self.maxWidth = maxWidth
        self.maxHeight = maxHeight
        self.keyframeIntervalS = keyframeIntervalS
        self.audioApp = audioApp
        self.audioMic = audioMic
    }

    enum CodingKeys: String, CodingKey {
        case codec
        case maxFps = "max_fps"
        case minFps = "min_fps"
        case bitrateBps = "bitrate_bps"
        case bitrateMode = "bitrate_mode"
        case maxWidth = "max_width"
        case maxHeight = "max_height"
        case keyframeIntervalS = "keyframe_interval_s"
        case audioApp = "audio_app"
        case audioMic = "audio_mic"
    }
}

public struct Want: Codable, Equatable, Sendable {
    public var video: Bool?
    public var audioApp: Bool?
    public var audioMic: Bool?

    public init(video: Bool? = nil, audioApp: Bool? = nil, audioMic: Bool? = nil) {
        self.video = video
        self.audioApp = audioApp
        self.audioMic = audioMic
    }

    enum CodingKeys: String, CodingKey {
        case video
        case audioApp = "audio_app"
        case audioMic = "audio_mic"
    }
}

/// `HELLO` (PC → iPad).
public struct Hello: Codable, Equatable, Sendable {
    public var `protocol`: Int
    public var client: String?
    public var clientVersion: String?
    public var transport: Transport
    public var token: String?
    public var want: Want?
    public var config: StreamConfig?

    public init(
        protocol: Int = PSWP.version, client: String? = nil, clientVersion: String? = nil, transport: Transport,
        token: String? = nil, want: Want? = nil, config: StreamConfig? = nil
    ) {
        self.protocol = `protocol`
        self.client = client
        self.clientVersion = clientVersion
        self.transport = transport
        self.token = token
        self.want = want
        self.config = config
    }

    enum CodingKeys: String, CodingKey {
        case `protocol`, client, transport, token, want, config
        case clientVersion = "client_version"
    }
}

public struct DeviceInfo: Codable, Equatable, Sendable {
    public var model: String?
    public var system: String?
    public var screenWidth: Int?
    public var screenHeight: Int?
    public var maxFps: Int?

    public init(model: String? = nil, system: String? = nil, screenWidth: Int? = nil, screenHeight: Int? = nil, maxFps: Int? = nil) {
        self.model = model
        self.system = system
        self.screenWidth = screenWidth
        self.screenHeight = screenHeight
        self.maxFps = maxFps
    }

    enum CodingKeys: String, CodingKey {
        case model, system
        case screenWidth = "screen_width"
        case screenHeight = "screen_height"
        case maxFps = "max_fps"
    }
}

/// `HELLO_ACK` (iPad → PC).
public struct HelloAck: Codable, Equatable, Sendable {
    public var `protocol`: Int
    public var server: String?
    public var serverVersion: String?
    public var sessionId: String?
    public var clock: String?
    public var device: DeviceInfo?
    public var codecs: [VideoCodec]?
    public var config: StreamConfig?

    public init(
        protocol: Int = PSWP.version, server: String? = nil, serverVersion: String? = nil, sessionId: String? = nil,
        clock: String? = "host_us", device: DeviceInfo? = nil, codecs: [VideoCodec]? = nil, config: StreamConfig? = nil
    ) {
        self.protocol = `protocol`
        self.server = server
        self.serverVersion = serverVersion
        self.sessionId = sessionId
        self.clock = clock
        self.device = device
        self.codecs = codecs
        self.config = config
    }

    enum CodingKeys: String, CodingKey {
        case `protocol`, server, clock, device, codecs, config
        case serverVersion = "server_version"
        case sessionId = "session_id"
    }
}

/// `BYE` (ambos os sentidos). Quem envia fecha o socket a seguir.
public struct Bye: Codable, Equatable, Sendable {
    public var reason: ByeReason
    public var message: String?

    public init(reason: ByeReason, message: String? = nil) {
        self.reason = reason
        self.message = message
    }
}

/// `ERROR` (ambos os sentidos).
public struct ErrorMessage: Codable, Equatable, Sendable {
    public var code: String
    public var message: String?
    public var fatal: Bool?
    public var refType: Int?
    public var refSeq: UInt32?

    public init(code: String, message: String? = nil, fatal: Bool? = nil, refType: Int? = nil, refSeq: UInt32? = nil) {
        self.code = code
        self.message = message
        self.fatal = fatal
        self.refType = refType
        self.refSeq = refSeq
    }

    enum CodingKeys: String, CodingKey {
        case code, message, fatal
        case refType = "ref_type"
        case refSeq = "ref_seq"
    }
}

/// `SENDER_STATE` (iPad → PC).
public struct SenderState: Codable, Equatable, Sendable {
    public var state: BroadcastState
    public var reason: String?

    public init(state: BroadcastState, reason: String? = nil) {
        self.state = state
        self.reason = reason
    }
}

/// `SENDER_STATS` (iPad → PC, ~1 Hz). `diag` é uma extensão compatível (campo novo, ignorado por
/// recetores antigos) com as medições dos spikes do passo 4.
public struct SenderStats: Codable, Equatable, Sendable {
    public var fpsCapture: Double?
    public var fpsEncoded: Double?
    public var bitrateBps: Int?
    public var targetBitrateBps: Int?
    public var framesDropped: Int?
    public var keyframes: Int?
    public var sendQueueBytes: Int?
    public var sendQueueMs: Double?
    public var encodeMsAvg: Double?
    public var thermal: ThermalState?
    public var memoryMb: Double?
    public var lowPowerMode: Bool?
    public var diag: SenderDiagnostics?

    public init() {}

    enum CodingKeys: String, CodingKey {
        case thermal, diag
        case fpsCapture = "fps_capture"
        case fpsEncoded = "fps_encoded"
        case bitrateBps = "bitrate_bps"
        case targetBitrateBps = "target_bitrate_bps"
        case framesDropped = "frames_dropped"
        case keyframes
        case sendQueueBytes = "send_queue_bytes"
        case sendQueueMs = "send_queue_ms"
        case encodeMsAvg = "encode_ms_avg"
        case memoryMb = "memory_mb"
        case lowPowerMode = "low_power_mode"
    }

    /// Edita `diag`, criando-o se ainda não existir.
    public mutating func withDiag(_ edit: (inout SenderDiagnostics) -> Void) {
        var d = diag ?? SenderDiagnostics()
        edit(&d)
        diag = d
    }
}

/// `RECEIVER_REPORT` (PC → iPad, ~1 Hz).
public struct ReceiverReport: Codable, Equatable, Sendable {
    public var fpsReceived: Double?
    public var fpsDecoded: Double?
    public var recvBitrateBps: Int?
    public var latencyMsP50: Double?
    public var latencyMsP95: Double?
    public var rttMs: Double?
    public var decodeErrors: Int?

    public init() {}

    enum CodingKeys: String, CodingKey {
        case fpsReceived = "fps_received"
        case fpsDecoded = "fps_decoded"
        case recvBitrateBps = "recv_bitrate_bps"
        case latencyMsP50 = "latency_ms_p50"
        case latencyMsP95 = "latency_ms_p95"
        case rttMs = "rtt_ms"
        case decodeErrors = "decode_errors"
    }
}

/// `CONFIGURE_ACK` (iPad → PC).
public struct ConfigureAck: Codable, Equatable, Sendable {
    public var requestSeq: UInt32
    public var applied: Bool
    public var config: StreamConfig?
    public var message: String?

    public init(requestSeq: UInt32, applied: Bool, config: StreamConfig? = nil, message: String? = nil) {
        self.requestSeq = requestSeq
        self.applied = applied
        self.config = config
        self.message = message
    }

    enum CodingKeys: String, CodingKey {
        case applied, config, message
        case requestSeq = "request_seq"
    }
}
