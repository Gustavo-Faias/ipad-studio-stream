import Foundation

/// Parser incremental: aceita pedaços de qualquer tamanho e devolve as mensagens completas.
///
/// Verifica o magic logo que há 2 bytes, sem esperar pelo cabeçalho completo. Depois de um erro
/// fatal o fluxo deixou de ser fiável: a ligação tem de ser fechada.
public struct StreamParser {
    private var buffer = Data()
    private var offset = 0

    public init() {}

    /// Bytes recebidos que ainda não formam uma mensagem completa.
    public var pending: Int { buffer.count - offset }

    public mutating func push(_ chunk: Data) {
        // Compacta quando já se consumiu mais de metade (evita crescer sem fim).
        if offset > 0, offset >= buffer.count / 2 {
            buffer = buffer.subdata(in: buffer.startIndex + offset ..< buffer.endIndex)
            offset = 0
        }
        buffer.append(chunk)
    }

    public mutating func next() throws -> Message? {
        let available = buffer.count - offset
        guard available >= 2 else { return nil }
        let header: Header? = try buffer.withUnsafeBytes { raw in
            let base = UnsafeRawBufferPointer(rebasing: raw[offset...])
            guard base[0] == PSWP.magic0, base[1] == PSWP.magic1 else { throw ProtocolError.badMagic }
            guard base.count >= PSWP.headerLength else { return nil }
            return try Header.read(from: base)
        }
        guard let header else { return nil }
        let total = PSWP.headerLength + Int(header.payloadLength)
        guard available >= total else { return nil }
        let start = buffer.startIndex + offset + PSWP.headerLength
        let payload = buffer.subdata(in: start ..< start + Int(header.payloadLength))
        offset += total
        if offset == buffer.count {
            buffer.removeAll(keepingCapacity: true)
            offset = 0
        }
        return Message(header: header, payload: payload)
    }
}
