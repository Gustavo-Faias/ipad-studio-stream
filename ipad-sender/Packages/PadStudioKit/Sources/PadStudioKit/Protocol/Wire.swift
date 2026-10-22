import Foundation

/// Constantes do enquadramento PSWP v1 (PROTOCOL.md §2).
public enum PSWP {
    public static let version = 1
    public static let headerLength = 24
    public static let maxPayload = 16 * 1024 * 1024
    public static let magic0: UInt8 = 0x50 // 'P'
    public static let magic1: UInt8 = 0x53 // 'S'
}

/// Tipos de mensagem (PROTOCOL.md §3). Tipos desconhecidos não têm caso: o recetor ignora-os.
public enum MessageType: UInt8, CaseIterable, Sendable {
    case hello = 0x01
    case helloAck = 0x02
    case ping = 0x03
    case pong = 0x04
    case bye = 0x05
    case error = 0x06
    case senderState = 0x07
    case videoConfig = 0x10
    case videoFrame = 0x11
    case audioConfig = 0x12
    case audioFrame = 0x13
    case senderStats = 0x20
    case receiverReport = 0x21
    case configure = 0x30
    case configureAck = 0x31
    case requestKeyframe = 0x32
    case setBitrate = 0x33

    public var name: String {
        switch self {
        case .hello: "HELLO"
        case .helloAck: "HELLO_ACK"
        case .ping: "PING"
        case .pong: "PONG"
        case .bye: "BYE"
        case .error: "ERROR"
        case .senderState: "SENDER_STATE"
        case .videoConfig: "VIDEO_CONFIG"
        case .videoFrame: "VIDEO_FRAME"
        case .audioConfig: "AUDIO_CONFIG"
        case .audioFrame: "AUDIO_FRAME"
        case .senderStats: "SENDER_STATS"
        case .receiverReport: "RECEIVER_REPORT"
        case .configure: "CONFIGURE"
        case .configureAck: "CONFIGURE_ACK"
        case .requestKeyframe: "REQUEST_KEYFRAME"
        case .setBitrate: "SET_BITRATE"
        }
    }

    /// Tipos cujo payload é um objeto JSON.
    public var isJSON: Bool {
        switch self {
        case .hello, .helloAck, .bye, .error, .senderState, .senderStats, .receiverReport, .configure, .configureAck:
            true
        default:
            false
        }
    }
}

/// Erros do protocolo. `code` é o mesmo dos vetores de teste e do crate Rust.
public enum ProtocolError: Error, Equatable, CustomStringConvertible {
    /// Os dois primeiros bytes não são "PS": o fluxo está dessincronizado.
    case badMagic
    /// `payload_len` acima de 16 MiB.
    case payloadTooLarge(Int)
    /// Payload binário curto ou com valores fora do domínio.
    case badPayload(String)
    /// Payload JSON que não corresponde ao esquema do tipo.
    case json(type: String, message: String)

    public var code: String {
        switch self {
        case .badMagic: "bad_magic"
        case .payloadTooLarge: "payload_too_large"
        case .badPayload, .json: "bad_payload"
        }
    }

    /// Erros que obrigam a fechar a ligação (o fluxo de bytes deixou de ser fiável).
    public var isFatal: Bool {
        switch self {
        case .badMagic, .payloadTooLarge: true
        default: false
        }
    }

    public var description: String {
        switch self {
        case .badMagic: "magic inválido (esperado \"PS\")"
        case let .payloadTooLarge(n): "payload demasiado grande: \(n) bytes (máximo 16 MiB)"
        case let .badPayload(m): m
        case let .json(type, message): "JSON inválido em \(type): \(message)"
        }
    }
}

/// Cabeçalho fixo de 24 bytes, little-endian (PROTOCOL.md §2).
public struct Header: Equatable, Sendable {
    public var type: UInt8
    public var flags: UInt8
    public var payloadLength: UInt32
    public var seq: UInt32
    public var aux: UInt32
    public var ptsUs: UInt64

    public init(type: UInt8, flags: UInt8 = 0, payloadLength: UInt32 = 0, seq: UInt32 = 0, aux: UInt32 = 0, ptsUs: UInt64 = 0) {
        self.type = type
        self.flags = flags
        self.payloadLength = payloadLength
        self.seq = seq
        self.aux = aux
        self.ptsUs = ptsUs
    }

    /// Escreve os 24 bytes no início de `buffer` (que tem de ter pelo menos 24 bytes).
    public func write(to buffer: UnsafeMutableRawBufferPointer) {
        precondition(buffer.count >= PSWP.headerLength)
        buffer[0] = PSWP.magic0
        buffer[1] = PSWP.magic1
        buffer[2] = type
        buffer[3] = flags
        buffer.storeBytes(of: payloadLength.littleEndian, toByteOffset: 4, as: UInt32.self)
        buffer.storeBytes(of: seq.littleEndian, toByteOffset: 8, as: UInt32.self)
        buffer.storeBytes(of: aux.littleEndian, toByteOffset: 12, as: UInt32.self)
        buffer.storeBytes(of: ptsUs.littleEndian, toByteOffset: 16, as: UInt64.self)
    }

    public var bytes: [UInt8] {
        var out = [UInt8](repeating: 0, count: PSWP.headerLength)
        out.withUnsafeMutableBytes { write(to: $0) }
        return out
    }

    /// Lê um cabeçalho dos primeiros 24 bytes de `buffer`.
    public static func read(from buffer: UnsafeRawBufferPointer) throws -> Header {
        precondition(buffer.count >= PSWP.headerLength)
        guard buffer[0] == PSWP.magic0, buffer[1] == PSWP.magic1 else { throw ProtocolError.badMagic }
        let len = UInt32(littleEndian: buffer.loadUnaligned(fromByteOffset: 4, as: UInt32.self))
        guard Int(len) <= PSWP.maxPayload else { throw ProtocolError.payloadTooLarge(Int(len)) }
        return Header(
            type: buffer[2],
            flags: buffer[3],
            payloadLength: len,
            seq: UInt32(littleEndian: buffer.loadUnaligned(fromByteOffset: 8, as: UInt32.self)),
            aux: UInt32(littleEndian: buffer.loadUnaligned(fromByteOffset: 12, as: UInt32.self)),
            ptsUs: UInt64(littleEndian: buffer.loadUnaligned(fromByteOffset: 16, as: UInt64.self))
        )
    }
}

/// Mensagem completa: cabeçalho + payload.
public struct Message: Equatable, Sendable {
    public var header: Header
    public var payload: Data

    public init(header: Header, payload: Data) {
        self.header = header
        self.payload = payload
        self.header.payloadLength = UInt32(payload.count)
    }

    public init(type: MessageType, flags: UInt8 = 0, seq: UInt32 = 0, aux: UInt32 = 0, ptsUs: UInt64 = 0, payload: Data = Data()) {
        self.init(header: Header(type: type.rawValue, flags: flags, seq: seq, aux: aux, ptsUs: ptsUs), payload: payload)
    }

    /// `nil` para tipos desconhecidos (a ignorar, PROTOCOL.md §2.2).
    public var kind: MessageType? { MessageType(rawValue: header.type) }

    public var encodedLength: Int { PSWP.headerLength + payload.count }

    /// Bytes no fio: cabeçalho + payload.
    public func encoded() -> Data {
        var out = Data(capacity: encodedLength)
        out.append(contentsOf: header.bytes)
        out.append(payload)
        return out
    }

    /// Mensagem JSON (`HELLO_ACK`, `SENDER_STATS`, …).
    public static func json<T: Encodable>(_ type: MessageType, _ value: T, seq: UInt32 = 0, ptsUs: UInt64 = 0) -> Message {
        Message(type: type, seq: seq, ptsUs: ptsUs, payload: JSONCoding.encode(value))
    }

    /// Descodifica o payload JSON.
    public func decodeJSON<T: Decodable>(_: T.Type) throws -> T {
        try JSONCoding.decode(T.self, from: payload, type: kind?.name ?? "tipo 0x\(String(header.type, radix: 16))")
    }
}

enum JSONCoding {
    static func encode<T: Encodable>(_ value: T) -> Data {
        do {
            return try JSONEncoder().encode(value)
        } catch {
            preconditionFailure("os tipos do protocolo serializam sempre: \(error)")
        }
    }

    static func decode<T: Decodable>(_: T.Type, from data: Data, type: String) throws -> T {
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw ProtocolError.json(type: type, message: String(describing: error))
        }
    }
}
