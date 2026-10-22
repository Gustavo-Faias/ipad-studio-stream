import Foundation

/// Resultado de uma leitura com tempo limite.
public enum ReadResult: Equatable {
    case data(Data)
    case timeout
    case eof
}

/// Ligação de bytes bidirecional (socket TCP ou, nos testes, um tubo em memória).
///
/// `read` é chamado só pela thread de leitura e `write` só pela de escrita; `close` pode vir de
/// qualquer thread e desbloqueia ambas.
public protocol ByteStream: AnyObject, Sendable {
    /// Espera até ~200 ms por dados.
    func read(maxLength: Int) throws -> ReadResult
    /// Escreve todas as partes, por ordem, ou lança erro.
    func write(_ parts: [Data]) throws
    func close()
}

public struct StreamClosed: Error, CustomStringConvertible {
    public init() {}
    public var description: String { "ligação fechada" }
}

/// Par de tubos em memória (testes): o que se escreve num lado lê-se no outro.
public final class PipeStream: ByteStream, @unchecked Sendable {
    private final class Buffer: @unchecked Sendable {
        let cond = NSCondition()
        var data = Data()
        var closed = false
    }

    private let incoming: Buffer
    private let outgoing: Buffer
    private let timeout: TimeInterval

    private init(incoming: Buffer, outgoing: Buffer, timeout: TimeInterval) {
        self.incoming = incoming
        self.outgoing = outgoing
        self.timeout = timeout
    }

    /// Dois extremos ligados entre si.
    public static func pair(readTimeout: TimeInterval = 0.05) -> (PipeStream, PipeStream) {
        let a = Buffer()
        let b = Buffer()
        return (PipeStream(incoming: a, outgoing: b, timeout: readTimeout), PipeStream(incoming: b, outgoing: a, timeout: readTimeout))
    }

    public func read(maxLength: Int) throws -> ReadResult {
        let buf = incoming
        buf.cond.lock()
        defer { buf.cond.unlock() }
        let deadline = Date(timeIntervalSinceNow: timeout)
        while buf.data.isEmpty, !buf.closed {
            if !buf.cond.wait(until: deadline) { return .timeout }
        }
        if buf.data.isEmpty { return .eof }
        let n = min(maxLength, buf.data.count)
        let chunk = buf.data.prefix(n)
        buf.data.removeFirst(n)
        return .data(Data(chunk))
    }

    public func write(_ parts: [Data]) throws {
        let buf = outgoing
        buf.cond.lock()
        defer { buf.cond.unlock() }
        guard !buf.closed else { throw StreamClosed() }
        for part in parts { buf.data.append(part) }
        buf.cond.broadcast()
    }

    public func close() {
        for buf in [incoming, outgoing] {
            buf.cond.lock()
            buf.closed = true
            buf.cond.broadcast()
            buf.cond.unlock()
        }
    }
}
