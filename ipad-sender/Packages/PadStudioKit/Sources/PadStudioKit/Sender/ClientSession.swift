import Foundation

/// Um PC ligado: fila de envio, contadores `seq` e o estado do vídeo desta ligação
/// (PROTOCOL.md §2.1 e §7.1). Espelha `tools/fake-sender/src/session.rs`.
final class ClientSession: @unchecked Sendable {
    let id: UInt64
    let peer: String
    let queue: SendQueue
    let stream: ByteStream

    private let lock = NSLock()
    private var seqs = [UInt32](repeating: 0, count: 0x40)
    private var videoStarted = false
    private var videoSeq: UInt32 = 0
    private var sentConfigId: UInt64 = 0
    private var dropping = false
    private var droppedFrames = 0
    private var report: ReceiverReport?
    private var closed = false
    private var byePending = false
    private let writerDone = NSCondition()
    private var writerFinished = false

    init(id: UInt64, peer: String, stream: ByteStream, limits: SendQueue.Limits, clock: HostClock) {
        self.id = id
        self.peer = peer
        self.stream = stream
        queue = SendQueue(limits: limits, clock: clock)
    }

    /// Contador `seq` por tipo de mensagem.
    func nextSeq(_ t: MessageType) -> UInt32 {
        lock.lock()
        defer { lock.unlock() }
        let i = Int(t.rawValue & 0x3f)
        let s = seqs[i]
        seqs[i] = s &+ 1
        return s
    }

    var dropped: Int {
        lock.lock()
        defer { lock.unlock() }
        return droppedFrames
    }

    var lastReport: ReceiverReport? {
        get {
            lock.lock()
            defer { lock.unlock() }
            return report
        }
        set {
            lock.lock()
            report = newValue
            lock.unlock()
        }
    }

    var isClosed: Bool {
        lock.lock()
        defer { lock.unlock() }
        return closed
    }

    /// Mensagem de controlo (prioridade máxima).
    func send(_ msg: Message) {
        queue.pushControl(.data(msg.encoded()))
    }

    func bye(_ reason: ByeReason, _ message: String) {
        let msg = Message.json(.bye, Bye(reason: reason, message: message), seq: nextSeq(.bye))
        lock.lock()
        byePending = true
        lock.unlock()
        queue.pushControl(.bye(msg.encoded()))
    }

    func close() {
        lock.lock()
        closed = true
        lock.unlock()
        queue.close()
        stream.close()
    }

    /// Fecha, dando ao escritor até 500 ms para entregar um BYE pendente.
    func finish() {
        lock.lock()
        let waitForBye = byePending
        lock.unlock()
        if waitForBye {
            writerDone.lock()
            let deadline = Date(timeIntervalSinceNow: 0.5)
            while !writerFinished, writerDone.wait(until: deadline) {}
            writerDone.unlock()
        }
        close()
    }

    /// Thread de escrita: esvazia a fila para o socket até fechar.
    func runWriter(clock: HostClock) {
        while let item = queue.pop() {
            do {
                switch item {
                case let .data(d):
                    try stream.write([d])
                case let .parts(head, body):
                    try stream.write([head, body])
                case let .pong(t0, t1, seq):
                    let pong = Pong(t0: t0, t1: t1, t2: clock.nowUs())
                    try stream.write([Message(type: .pong, seq: seq, payload: pong.encode()).encoded()])
                case let .bye(d):
                    try stream.write([d])
                    throw StreamClosed()
                }
            } catch {
                break
            }
        }
        writerDone.lock()
        writerFinished = true
        writerDone.broadcast()
        writerDone.unlock()
        close()
    }

    /// Resultado de entregar um frame a esta ligação.
    enum VideoOutcome: Equatable {
        case queued
        /// Descartado (ou à espera do primeiro keyframe): é preciso um keyframe.
        case needsKeyframe
    }

    /// Novo access unit do encoder (PROTOCOL.md §7.1).
    func onVideo(_ frame: EncodedFrame, nowUs: UInt64) -> VideoOutcome {
        lock.lock()
        defer { lock.unlock() }
        guard !closed else { return .queued }
        if !videoStarted {
            guard frame.keyframe else { return .needsKeyframe } // uma ligação nova começa num keyframe
            videoStarted = true
        }
        if frame.keyframe, sentConfigId != frame.configId {
            let seq = seqLocked(.videoConfig)
            let msg = Message(type: .videoConfig, seq: seq, ptsUs: frame.ptsUs, payload: frame.config.encode())
            _ = queue.pushVideo(msg.encoded(), droppable: false)
            sentConfigId = frame.configId
        }

        let seq = videoSeq
        videoSeq &+= 1 // conta também os frames que vão ser descartados
        if dropping, !frame.keyframe {
            droppedFrames += 1
            return .needsKeyframe
        }
        var flags = VideoFlags(keyframe: frame.keyframe, paramsInband: frame.keyframe, discontinuity: dropping, rotation: frame.rotation)
        dropping = false
        // Latência no emissor: entrada na fila − PTS de captura.
        let aux = UInt32(clamping: nowUs > frame.ptsUs ? nowUs - frame.ptsUs : 0)
        func header() -> Data {
            Data(Header(type: MessageType.videoFrame.rawValue, flags: flags.rawValue, payloadLength: UInt32(frame.packet.payload.count),
                        seq: seq, aux: aux, ptsUs: frame.ptsUs).bytes)
        }
        guard let flushed = queue.pushVideo(head: header(), frame.packet.payload, droppable: true) else { return .queued }

        // A fila estava cheia ou atrasada e foi esvaziada.
        droppedFrames += flushed
        if frame.keyframe {
            // O keyframe entra logo, marcado como descontinuidade.
            flags.discontinuity = true
            if queue.pushVideo(head: header(), frame.packet.payload, droppable: true) == nil { return .queued }
        }
        droppedFrames += 1
        dropping = true
        return .needsKeyframe
    }

    private func seqLocked(_ t: MessageType) -> UInt32 {
        let i = Int(t.rawValue & 0x3f)
        let s = seqs[i]
        seqs[i] = s &+ 1
        return s
    }

    /// Simula uma fila cheia: esvazia o vídeo e descarta até ao próximo keyframe.
    func simulateDrop() -> Int {
        lock.lock()
        defer { lock.unlock() }
        dropping = true
        let n = queue.flushVideo()
        droppedFrames += n
        return n
    }
}
