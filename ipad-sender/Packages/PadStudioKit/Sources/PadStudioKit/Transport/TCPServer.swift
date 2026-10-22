import Foundation
#if os(Windows)
import WinSDK
#elseif canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#elseif canImport(Musl)
import Musl
#endif

// Servidor TCP com sockets BSD (iPad, macOS, Linux) ou Winsock (Windows, para o simulador).
//
// Sockets "à moda antiga" em vez de Network.framework: dão `TCP_NODELAY`, `SO_SNDBUF` moderado
// (PROTOCOL.md §1: a pressão tem de aparecer na fila da aplicação) e o mesmo código corre no
// Windows, onde se testa o emissor Swift contra o PC real.

#if os(Windows)
typealias NativeSocket = SOCKET
private let invalidSocket = INVALID_SOCKET
#else
typealias NativeSocket = Int32
private let invalidSocket: Int32 = -1
#endif

public struct SocketError: Error, CustomStringConvertible {
    public let operation: String
    public let code: Int32

    public var description: String {
        #if os(Windows)
        if code == 10048 { return "\(operation): a porta já está em uso" }
        #else
        if code == EADDRINUSE { return "\(operation): a porta já está em uso" }
        #endif
        return "\(operation) falhou (erro \(code))"
    }
}

enum Socket {
    #if os(Windows)
    private static let startup: Void = {
        var data = WSADATA()
        _ = WSAStartup(0x0202, &data)
    }()
    #endif

    static func lastError() -> Int32 {
        #if os(Windows)
        return WSAGetLastError()
        #else
        return errno
        #endif
    }

    static func make() throws -> NativeSocket {
        #if os(Windows)
        _ = startup
        let s = WinSDK.socket(AF_INET, SOCK_STREAM, IPPROTO_TCP.rawValue)
        #elseif canImport(Darwin)
        let s = Darwin.socket(AF_INET, SOCK_STREAM, 0)
        #else
        let s = socket(AF_INET, Int32(SOCK_STREAM.rawValue), 0)
        #endif
        guard s != invalidSocket else { throw SocketError(operation: "socket", code: lastError()) }
        return s
    }

    static func close(_ s: NativeSocket) {
        #if os(Windows)
        _ = closesocket(s)
        #elseif canImport(Darwin)
        _ = Darwin.close(s)
        #else
        _ = Glibc.close(s)
        #endif
    }

    static func shutdownBoth(_ s: NativeSocket) {
        #if os(Windows)
        _ = WinSDK.shutdown(s, Int32(SD_BOTH))
        #elseif canImport(Darwin)
        _ = Darwin.shutdown(s, SHUT_RDWR)
        #else
        _ = Glibc.shutdown(s, Int32(SHUT_RDWR))
        #endif
    }

    static func setInt(_ s: NativeSocket, level: Int32, name: Int32, _ value: Int32) {
        var v = value
        #if os(Windows)
        _ = withUnsafePointer(to: &v) {
            $0.withMemoryRebound(to: CChar.self, capacity: 4) { setsockopt(s, level, name, $0, 4) }
        }
        #else
        _ = setsockopt(s, level, name, &v, socklen_t(MemoryLayout<Int32>.size))
        #endif
    }

    static var tcpLevel: Int32 {
        #if os(Windows)
        return IPPROTO_TCP.rawValue
        #else
        return Int32(IPPROTO_TCP)
        #endif
    }

    /// Opções de uma ligação aceite: sem Nagle, buffer de envio moderado, sem SIGPIPE.
    static func configureClient(_ s: NativeSocket) {
        setInt(s, level: tcpLevel, name: Int32(TCP_NODELAY), 1)
        setInt(s, level: Int32(SOL_SOCKET), name: Int32(SO_SNDBUF), 512 * 1024)
        #if canImport(Darwin)
        setInt(s, level: SOL_SOCKET, name: SO_NOSIGPIPE, 1)
        #endif
    }

    static func makeAddress(ipv4: UInt32, port: UInt16) -> sockaddr_in {
        var addr = sockaddr_in()
        #if os(Windows)
        addr.sin_family = ADDRESS_FAMILY(AF_INET)
        addr.sin_addr.S_un.S_addr = ipv4.bigEndian
        #else
        #if canImport(Darwin)
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        #endif
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_addr.s_addr = ipv4.bigEndian
        #endif
        addr.sin_port = port.bigEndian
        return addr
    }

    static func describe(_ addr: sockaddr_in) -> String {
        #if os(Windows)
        let ip = UInt32(bigEndian: addr.sin_addr.S_un.S_addr)
        #else
        let ip = UInt32(bigEndian: addr.sin_addr.s_addr)
        #endif
        let port = UInt16(bigEndian: addr.sin_port)
        return "\(ip >> 24).\(ip >> 16 & 0xff).\(ip >> 8 & 0xff).\(ip & 0xff):\(port)"
    }

    static func localPort(_ s: NativeSocket) -> UInt16? {
        var addr = sockaddr_in()
        #if os(Windows)
        var len = Int32(MemoryLayout<sockaddr_in>.size)
        #else
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        #endif
        let ok = withUnsafeMutablePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(s, $0, &len) }
        }
        return ok == 0 ? UInt16(bigEndian: addr.sin_port) : nil
    }

    static func peer(_ s: NativeSocket) -> String {
        var addr = sockaddr_in()
        #if os(Windows)
        var len = Int32(MemoryLayout<sockaddr_in>.size)
        #else
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        #endif
        let ok = withUnsafeMutablePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getpeername(s, $0, &len) }
        }
        return ok == 0 ? describe(addr) : "?"
    }

    /// Espera até `timeoutMs` por dados (ou por uma ligação, num socket à escuta).
    /// `true` = pronto a ler (dados, EOF ou erro).
    static func waitReadable(_ s: NativeSocket, timeoutMs: Int32) -> Bool {
        #if os(Windows)
        var pfd = WSAPOLLFD(fd: s, events: Int16(POLLRDNORM), revents: 0)
        return WSAPoll(&pfd, 1, timeoutMs) > 0
        #else
        var pfd = pollfd(fd: s, events: Int16(POLLIN), revents: 0)
        return poll(&pfd, 1, timeoutMs) > 0
        #endif
    }
}

extension Socket {
    /// Cliente para 127.0.0.1:`port` (testes).
    static func connectLoopback(port: UInt16) throws -> SocketStream {
        let s = try make()
        var addr = makeAddress(ipv4: 0x7f00_0001, port: port)
        let rc = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { ptr in
                #if os(Windows)
                connect(s, ptr, Int32(MemoryLayout<sockaddr_in>.size))
                #else
                connect(s, ptr, socklen_t(MemoryLayout<sockaddr_in>.size))
                #endif
            }
        }
        guard rc == 0 else {
            let e = lastError()
            close(s)
            throw SocketError(operation: "connect 127.0.0.1:\(port)", code: e)
        }
        return SocketStream(fd: s)
    }
}

/// Uma ligação TCP aceite.
public final class SocketStream: ByteStream, @unchecked Sendable {
    private let fd: NativeSocket
    private let lock = NSLock()
    private var closed = false
    public let peer: String

    init(fd: NativeSocket) {
        self.fd = fd
        peer = Socket.peer(fd)
        Socket.configureClient(fd)
    }

    deinit {
        close()
    }

    public func read(maxLength: Int) throws -> ReadResult {
        guard !isClosed else { return .eof }
        guard Socket.waitReadable(fd, timeoutMs: 200) else { return isClosed ? .eof : .timeout }
        var buf = [UInt8](repeating: 0, count: maxLength)
        #if os(Windows)
        let n = buf.withUnsafeMutableBytes { recv(fd, $0.baseAddress?.assumingMemoryBound(to: CChar.self), Int32(maxLength), 0) }
        #else
        let n = buf.withUnsafeMutableBytes { recv(fd, $0.baseAddress, maxLength, 0) }
        #endif
        if n > 0 { return .data(Data(buf[0 ..< Int(n)])) }
        if n == 0 { return .eof }
        #if !os(Windows)
        let e = Socket.lastError()
        if e == EAGAIN || e == EWOULDBLOCK || e == EINTR { return .timeout }
        #endif
        return .eof // ECONNRESET e afins: a ligação acabou
    }

    public func write(_ parts: [Data]) throws {
        for part in parts {
            try part.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
                var offset = 0
                while offset < raw.count {
                    guard !isClosed, let base = raw.baseAddress else { throw StreamClosed() }
                    let remaining = raw.count - offset
                    #if os(Windows)
                    let n = Int(send(fd, (base + offset).assumingMemoryBound(to: CChar.self), Int32(min(remaining, 1 << 20)), 0))
                    #elseif canImport(Darwin)
                    let n = send(fd, base + offset, remaining, 0)
                    #else
                    let n = send(fd, base + offset, remaining, Int32(MSG_NOSIGNAL))
                    #endif
                    if n <= 0 {
                        #if !os(Windows)
                        if n < 0, Socket.lastError() == EINTR { continue }
                        #endif
                        throw SocketError(operation: "send", code: Socket.lastError())
                    }
                    offset += n
                }
            }
        }
    }

    private var isClosed: Bool {
        lock.lock()
        defer { lock.unlock() }
        return closed
    }

    public func close() {
        lock.lock()
        let wasOpen = !closed
        closed = true
        lock.unlock()
        guard wasOpen else { return }
        Socket.shutdownBoth(fd)
        Socket.close(fd)
    }
}

/// Servidor TCP: aceita ligações numa thread própria e entrega cada uma ao `handler`, numa thread nova.
public final class TCPServer: @unchecked Sendable {
    public enum Interface: Sendable {
        /// 127.0.0.1: o túnel USB do usbmuxd chega aqui.
        case loopback
        /// 0.0.0.0: rede local.
        case any
    }

    private let fd: NativeSocket
    private let lock = NSLock()
    private var stopped = false
    public let port: UInt16

    /// Cria o socket e começa a escutar (falha logo se a porta estiver ocupada). `port` 0 = efémera.
    public init(interface: Interface, port: UInt16) throws {
        let s = try Socket.make()
        #if !os(Windows)
        // Reiniciar a transmissão logo a seguir não pode falhar por TIME_WAIT.
        Socket.setInt(s, level: Int32(SOL_SOCKET), name: Int32(SO_REUSEADDR), 1)
        #endif
        var addr = Socket.makeAddress(ipv4: interface == .loopback ? 0x7f00_0001 : 0, port: port)
        let bound = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { ptr in
                #if os(Windows)
                bind(s, ptr, Int32(MemoryLayout<sockaddr_in>.size))
                #else
                bind(s, ptr, socklen_t(MemoryLayout<sockaddr_in>.size))
                #endif
            }
        }
        guard bound == 0 else {
            let e = Socket.lastError()
            Socket.close(s)
            throw SocketError(operation: "bind \(interface == .loopback ? "127.0.0.1" : "0.0.0.0"):\(port)", code: e)
        }
        guard listen(s, 4) == 0 else {
            let e = Socket.lastError()
            Socket.close(s)
            throw SocketError(operation: "listen", code: e)
        }
        fd = s
        self.port = Socket.localPort(s) ?? port
    }

    deinit {
        stop()
    }

    /// Começa a aceitar ligações.
    public func start(handler: @escaping @Sendable (SocketStream) -> Void) {
        let t = Thread { [self] in
            while true {
                guard !isStopped else { return }
                guard Socket.waitReadable(fd, timeoutMs: 250) else { continue }
                guard !isStopped else { return }
                let c = accept(fd, nil, nil)
                guard c != invalidSocket else { continue }
                let stream = SocketStream(fd: c)
                let worker = Thread { handler(stream) }
                worker.name = "padstudio-connection"
                worker.start()
            }
        }
        t.name = "padstudio-accept"
        t.start()
    }

    private var isStopped: Bool {
        lock.lock()
        defer { lock.unlock() }
        return stopped
    }

    public func stop() {
        lock.lock()
        let wasRunning = !stopped
        stopped = true
        lock.unlock()
        if wasRunning { Socket.close(fd) }
    }
}
