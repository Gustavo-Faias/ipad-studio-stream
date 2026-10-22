import Foundation

/// Contribui com campos para o `SENDER_STATS` de cada segundo (captura, encoder, sistema).
public protocol StatsSource: AnyObject {
    func contribute(to stats: inout SenderStats)
}

/// Eventos do emissor para a extensão (ou para o simulador).
public protocol SenderDelegate: AnyObject {
    /// O PC mudou a configuração (`HELLO.config`, `CONFIGURE`, `SET_BITRATE`).
    func sender(_ sender: Sender, didChangeSettings settings: EncoderSettings, from old: EncoderSettings)
    func sender(_ sender: Sender, clientConnected peer: String)
    func sender(_ sender: Sender, clientDisconnected peer: String)
}

public extension SenderDelegate {
    func sender(_: Sender, didChangeSettings _: EncoderSettings, from _: EncoderSettings) {}
    func sender(_: Sender, clientConnected _: String) {}
    func sender(_: Sender, clientDisconnected _: String) {}
}

/// Um access unit pronto a enviar.
public struct EncodedFrame: Sendable {
    public var packet: FramePacket
    public var keyframe: Bool
    /// PTS de captura (host time, µs).
    public var ptsUs: UInt64
    public var rotation: Rotation
    /// Muda sempre que a configuração do encoder muda (sessão VT nova, parâmetros novos).
    public var configId: UInt64
    public var config: VideoConfig

    public init(packet: FramePacket, keyframe: Bool, ptsUs: UInt64, rotation: Rotation, configId: UInt64, config: VideoConfig) {
        self.packet = packet
        self.keyframe = keyframe
        self.ptsUs = ptsUs
        self.rotation = rotation
        self.configId = configId
        self.config = config
    }
}

/// Servidor PSWP do lado iPad, independente do transporte e das frameworks da Apple.
///
/// - `serve(_:peer:)` trata uma ligação (bloqueia; uma thread por ligação). A ligação mais recente
///   ganha: a anterior recebe `BYE{replaced}`.
/// - `submitVideo(_:)` recebe os frames do encoder.
/// - `keyframes` diz ao encoder quando forçar um keyframe.
/// - A cada segundo envia `SENDER_STATS` com o contributo das `StatsSource`.
public final class Sender: @unchecked Sendable {
    public struct Options {
        public var transport: Transport = .usb
        /// PIN do modo rede local (obrigatório em `.lan`).
        public var token: String?
        public var serverName = "PadStudioBroadcast"
        public var device = DeviceInfo()
        public var limits = EncoderSettings.Limits()
        public var clock: HostClock = .system
        public var helloTimeoutUs: UInt64 = 3_000_000
        public var idleTimeoutUs: UInt64 = 5_000_000
        public var statsIntervalS: Double = 1
        public var log: @Sendable (String) -> Void = { _ in }

        public init() {}
    }

    public let options: Options
    public let keyframes = KeyframeGate()
    public weak var delegate: SenderDelegate?

    private let lock = NSLock()
    private var currentSettings: EncoderSettings
    private var client: ClientSession?
    private var nextSessionId: UInt64 = 1
    private var sources: [StatsSource] = []
    private var state = BroadcastState.active
    private var authFailures = 0
    private var lockedUntilUs: UInt64 = 0
    private var windowFrames = 0
    private var windowBytes = 0
    private var keyframesTotal = 0
    private var running = false
    private let statsWake = NSCondition()

    public init(options: Options, settings: EncoderSettings) {
        self.options = options
        currentSettings = settings
        keyframes.setPeriodic(seconds: settings.keyframeIntervalS)
    }

    private var clock: HostClock { options.clock }

    public var settings: EncoderSettings {
        lock.lock()
        defer { lock.unlock() }
        return currentSettings
    }

    public var hasClient: Bool {
        lock.lock()
        defer { lock.unlock() }
        return client != nil
    }

    /// Último `RECEIVER_REPORT` do PC ligado.
    public var lastReceiverReport: ReceiverReport? { currentClient()?.lastReport }

    public func addStatsSource(_ source: StatsSource) {
        lock.lock()
        sources.append(source)
        lock.unlock()
    }

    // ── Ciclo de vida ───────────────────────────────────────────────────────

    /// Arranca o envio periódico de `SENDER_STATS`.
    public func start() {
        lock.lock()
        guard !running else { lock.unlock(); return }
        running = true
        lock.unlock()
        let t = Thread { [weak self] in self?.statsLoop() }
        t.name = "padstudio-stats"
        t.start()
    }

    /// Termina: `BYE` ao PC ligado e para as estatísticas.
    public func stop(reason: ByeReason = .userStopped, message: String = "Transmissão terminada no iPad") {
        lock.lock()
        running = false
        let c = client
        client = nil
        lock.unlock()
        statsWake.lock()
        statsWake.broadcast()
        statsWake.unlock()
        if let c {
            c.bye(reason, message)
            c.finish()
        }
    }

    /// `SENDER_STATE` (ex.: o sistema pausou a transmissão).
    public func setBroadcastState(_ s: BroadcastState, reason: String? = nil) {
        lock.lock()
        state = s
        let c = client
        lock.unlock()
        if let c {
            c.send(.json(.senderState, SenderState(state: s, reason: reason), seq: c.nextSeq(.senderState), ptsUs: clock.nowUs()))
        }
    }

    private func currentClient() -> ClientSession? {
        lock.lock()
        defer { lock.unlock() }
        return client
    }

    // ── Vídeo ───────────────────────────────────────────────────────────────

    /// Novo access unit do encoder. Sem PC ligado é descartado (e não pesa em nada).
    public func submitVideo(_ frame: EncodedFrame) {
        let now = clock.nowUs()
        if frame.keyframe { keyframes.keyframeProduced(nowUs: now) }
        lock.lock()
        windowFrames += 1
        windowBytes += frame.packet.payload.count
        if frame.keyframe { keyframesTotal += 1 }
        let c = client
        lock.unlock()
        guard let c else { return }
        if c.onVideo(frame, nowUs: now) == .needsKeyframe {
            keyframes.request()
        }
    }

    /// Simula uma fila cheia no PC ligado (testes e simulador).
    @discardableResult
    public func simulateDrop() -> Int {
        guard let c = currentClient() else { return 0 }
        let n = c.simulateDrop()
        keyframes.request()
        return n
    }

    // ── Configuração ────────────────────────────────────────────────────────

    /// Aplica uma configuração do PC. Devolve a configuração efetiva e as notas.
    @discardableResult
    func applyConfig(_ config: StreamConfig) -> (EncoderSettings, [String]) {
        lock.lock()
        let old = currentSettings
        var new = old
        let notes = new.apply(config, limits: options.limits)
        currentSettings = new
        lock.unlock()
        if new != old {
            keyframes.setPeriodic(seconds: new.keyframeIntervalS)
            delegate?.sender(self, didChangeSettings: new, from: old)
        }
        return (new, notes)
    }

    // ── Ligação ─────────────────────────────────────────────────────────────

    /// Trata uma ligação até ela terminar (bloqueia a thread que chama).
    public func serve(_ stream: ByteStream, peer: String) {
        var parser = StreamParser()
        do {
            guard let hello = try waitForHello(stream, &parser, peer: peer) else { return }
            guard let session = try accept(hello, stream: stream, peer: peer) else { return }
            let result = Result { try readLoop(session, stream, &parser) }
            lock.lock()
            if client === session { client = nil }
            lock.unlock()
            session.finish()
            options.log("\(peer): desligado (drops: \(session.dropped))")
            delegate?.sender(self, clientDisconnected: peer)
            if case let .failure(error) = result { options.log("\(peer): \(error)") }
        } catch {
            options.log("\(peer): \(error)")
            stream.close()
        }
    }

    /// 1. `HELLO` nos primeiros 3 s (PROTOCOL.md §5.1). O iPad não envia nada antes.
    private func waitForHello(_ stream: ByteStream, _ parser: inout StreamParser, peer: String) throws -> Hello? {
        let deadline = clock.nowUs() + options.helloTimeoutUs
        while true {
            while let msg = try parser.next() {
                if msg.kind == .hello { return try msg.decodeJSON(Hello.self) }
            }
            if clock.nowUs() >= deadline {
                reject(stream, .timeout, "HELLO não recebido em 3 s")
                options.log("\(peer): sem HELLO em 3 s, ligação recusada")
                return nil
            }
            switch try stream.read(maxLength: 64 * 1024) {
            case let .data(d): parser.push(d)
            case .timeout: break
            case .eof: stream.close(); return nil
            }
        }
    }

    /// Recusa uma ligação antes de haver sessão.
    private func reject(_ stream: ByteStream, _ reason: ByeReason, _ message: String) {
        try? stream.write([Message.json(.bye, Bye(reason: reason, message: message), ptsUs: clock.nowUs()).encoded()])
        stream.close()
    }

    /// 2. Validação, sessão, escritor e `HELLO_ACK`.
    private func accept(_ hello: Hello, stream: ByteStream, peer: String) throws -> ClientSession? {
        if hello.protocol < PSWP.version {
            reject(stream, .protocolVersion, "versão do protocolo não suportada")
            options.log("\(peer): protocolo \(hello.protocol) não suportado")
            return nil
        }
        if let pin = options.token {
            let now = clock.nowUs()
            lock.lock()
            let locked = now < lockedUntilUs
            var ok = false
            if !locked {
                ok = hello.token == pin
                if ok {
                    authFailures = 0
                } else {
                    authFailures += 1
                    if authFailures >= 3 {
                        authFailures = 0
                        lockedUntilUs = now + 30_000_000
                    }
                }
            }
            lock.unlock()
            if locked {
                reject(stream, .unauthorized, "demasiadas tentativas; tenta de novo dentro de 30 s")
                options.log("\(peer): PIN bloqueado")
                return nil
            }
            if !ok {
                reject(stream, .unauthorized, "PIN errado")
                options.log("\(peer): PIN errado, ligação recusada")
                return nil
            }
        }
        if let cfg = hello.config { applyConfig(cfg) }

        lock.lock()
        let id = nextSessionId
        nextSessionId += 1
        let st = state
        lock.unlock()
        let session = ClientSession(
            id: id, peer: peer, stream: stream, limits: options.transport == .lan ? .lan : .usb, clock: clock
        )
        let writer = Thread { [clock] in session.runWriter(clock: clock) }
        writer.name = "padstudio-writer"
        writer.start()

        let ack = HelloAck(
            server: options.serverName,
            serverVersion: PadStudioKit.version,
            sessionId: UUID().uuidString,
            device: options.device,
            codecs: options.limits.codecs,
            config: settings.streamConfig
        )
        session.send(.json(.helloAck, ack, seq: session.nextSeq(.helloAck), ptsUs: clock.nowUs()))
        if st != .active {
            session.send(.json(.senderState, SenderState(state: st), seq: session.nextSeq(.senderState), ptsUs: clock.nowUs()))
        }

        lock.lock()
        let old = client
        client = session
        lock.unlock()
        if let old {
            old.bye(.replaced, "nova ligação aceite")
            options.log("\(old.peer): substituída por \(peer)")
        }
        keyframes.request() // o vídeo de uma ligação nova começa num keyframe
        options.log("\(peer): ligado (\(hello.client ?? "cliente") \(hello.clientVersion ?? ""), \(hello.transport.rawValue))")
        delegate?.sender(self, clientConnected: peer)
        return session
    }

    /// 3. Comandos do PC até a ligação acabar.
    private func readLoop(_ session: ClientSession, _ stream: ByteStream, _ parser: inout StreamParser) throws {
        var lastRx = clock.nowUs()
        while true {
            while let msg = try parser.next() {
                do {
                    try handle(msg, session)
                } catch let error as ProtocolError {
                    options.log("\(session.peer): \(msg.kind?.name ?? "?") inválido: \(error)")
                    let err = ErrorMessage(code: error.code, message: error.description, fatal: false,
                                           refType: Int(msg.header.type), refSeq: msg.header.seq)
                    session.send(.json(.error, err, seq: session.nextSeq(.error), ptsUs: clock.nowUs()))
                }
            }
            if session.isClosed { return }
            if clock.nowUs() > lastRx + options.idleTimeoutUs {
                session.bye(.timeout, "sem mensagens do PC há 5 s")
                options.log("\(session.peer): sem mensagens há 5 s")
                return
            }
            switch try stream.read(maxLength: 64 * 1024) {
            case let .data(d):
                parser.push(d)
                lastRx = clock.nowUs()
            case .timeout:
                break
            case .eof:
                return
            }
        }
    }

    private func handle(_ msg: Message, _ session: ClientSession) throws {
        guard let kind = msg.kind else { return } // tipo desconhecido: ignorar (PROTOCOL.md §2.2)
        switch kind {
        case .ping:
            let ping = try Ping.decode(msg.payload)
            session.queue.pushControl(.pong(t0: ping.t0, t1: clock.nowUs(), seq: session.nextSeq(.pong)))
        case .requestKeyframe:
            keyframes.request()
        case .setBitrate:
            let b = try SetBitrate.decode(msg.payload)
            applyConfig(StreamConfig(bitrateBps: Int(b.bitrateBps)))
        case .configure:
            let req = try msg.decodeJSON(StreamConfig.self)
            let (effective, notes) = applyConfig(req)
            let ack = ConfigureAck(requestSeq: msg.header.seq, applied: true, config: effective.streamConfig,
                                   message: notes.isEmpty ? nil : notes.joined(separator: "; "))
            session.send(.json(.configureAck, ack, seq: session.nextSeq(.configureAck), ptsUs: clock.nowUs()))
        case .receiverReport:
            session.lastReport = try msg.decodeJSON(ReceiverReport.self)
        case .bye:
            let bye = try msg.decodeJSON(Bye.self)
            options.log("\(session.peer): BYE \(bye.reason.rawValue) \(bye.message ?? "")")
            session.close()
        case .error:
            let e = try msg.decodeJSON(ErrorMessage.self)
            options.log("\(session.peer): ERROR \(e.code) \(e.message ?? "")")
        case .pong:
            break
        default:
            options.log("\(session.peer): \(kind.name) inesperado do PC, ignorado")
        }
    }

    // ── Estatísticas ────────────────────────────────────────────────────────

    private func statsLoop() {
        var lastTick = clock.nowUs()
        while true {
            statsWake.lock()
            _ = statsWake.wait(until: Date(timeIntervalSinceNow: options.statsIntervalS))
            statsWake.unlock()
            lock.lock()
            guard running else { lock.unlock(); return }
            lock.unlock()
            let now = clock.nowUs()
            let elapsed = max(Double(now - lastTick) / 1_000_000, 0.001)
            lastTick = now
            let stats = collectStats(elapsedS: elapsed)
            if let c = currentClient() {
                c.send(.json(.senderStats, stats, seq: c.nextSeq(.senderStats), ptsUs: now))
            }
        }
    }

    /// Junta os contadores da janela e o contributo das fontes. Público para testes e diagnóstico.
    public func collectStats(elapsedS: Double) -> SenderStats {
        lock.lock()
        let (frames, bytes, keys, settings, c, sources) = (windowFrames, windowBytes, keyframesTotal, currentSettings, client, self.sources)
        windowFrames = 0
        windowBytes = 0
        lock.unlock()

        var s = SenderStats()
        s.fpsEncoded = rounded(Double(frames) / elapsedS, 1)
        s.bitrateBps = Int(Double(bytes * 8) / elapsedS)
        s.targetBitrateBps = settings.bitrateBps
        s.keyframes = keys
        if let c {
            let (queueBytes, queueMs) = c.queue.videoBacklog()
            s.framesDropped = c.dropped
            s.sendQueueBytes = queueBytes
            s.sendQueueMs = rounded(queueMs, 1)
        }
        for source in sources { source.contribute(to: &s) }
        return s
    }
}
