import Foundation

/// Fila de envio com três faixas e prioridade controlo > áudio > vídeo (PROTOCOL.md §7.1).
///
/// - Vídeo: limitada a 8 MiB ou à idade máxima (100 ms USB / 250 ms rede). Se um frame não cabe,
///   esvazia-se a faixa e a sessão descarta até ao keyframe seguinte.
/// - Áudio: limitada a 512 KiB; descarta os pacotes mais antigos.
/// - Configurações (`VIDEO_CONFIG`/`AUDIO_CONFIG`) nunca são descartadas, para manterem a ordem.
final class SendQueue: @unchecked Sendable {
    enum Item {
        case data(Data)
        /// Cabeçalho e payload em separado (vídeo: o payload nunca é copiado).
        case parts(Data, Data)
        /// O `t2` é carimbado pelo escritor imediatamente antes de escrever.
        case pong(t0: UInt64, t1: UInt64, seq: UInt32)
        /// Escreve e fecha a ligação.
        case bye(Data)
    }

    struct Limits {
        var maxVideoBytes = 8 * 1024 * 1024
        var maxVideoAgeUs: UInt64 = 100_000
        var maxAudioBytes = 512 * 1024

        static let usb = Limits()
        static let lan = Limits(maxVideoAgeUs: 250_000)
    }

    private struct Queued {
        var head: Data?
        var data: Data
        var droppable: Bool
        var atUs: UInt64

        var size: Int { (head?.count ?? 0) + data.count }

        var item: Item { head.map { .parts($0, data) } ?? .data(data) }
    }

    private let cond = NSCondition()
    private var control: [Item] = []
    private var audio: [Queued] = []
    private var audioBytes = 0
    private var video: [Queued] = []
    private var videoBytes = 0
    private var closed = false
    private let limits: Limits
    private let clock: HostClock

    init(limits: Limits, clock: HostClock) {
        self.limits = limits
        self.clock = clock
    }

    func pushControl(_ item: Item) {
        cond.lock()
        defer { cond.unlock() }
        guard !closed else { return }
        control.append(item)
        cond.signal()
    }

    /// Devolve quantos pacotes antigos foram descartados para caber este.
    @discardableResult
    func pushAudio(_ data: Data, droppable: Bool) -> Int {
        cond.lock()
        defer { cond.unlock() }
        guard !closed else { return 0 }
        var dropped = 0
        while audioBytes + data.count > limits.maxAudioBytes {
            guard let i = audio.firstIndex(where: { $0.droppable }) else { break }
            audioBytes -= audio.remove(at: i).size
            dropped += 1
        }
        audioBytes += data.count
        audio.append(Queued(head: nil, data: data, droppable: droppable, atUs: clock.nowUs()))
        cond.signal()
        return dropped
    }

    /// `nil` se entrou; senão, a faixa de vídeo foi esvaziada e devolve quantos frames saíram.
    func pushVideo(head: Data? = nil, _ data: Data, droppable: Bool) -> Int? {
        cond.lock()
        defer { cond.unlock() }
        guard !closed else { return nil }
        let now = clock.nowUs()
        let q = Queued(head: head, data: data, droppable: droppable, atUs: now)
        if droppable {
            let tooBig = videoBytes + q.size > limits.maxVideoBytes
            let tooOld = video.first(where: { $0.droppable }).map { now > $0.atUs && now - $0.atUs > limits.maxVideoAgeUs } ?? false
            if tooBig || tooOld {
                return flushLocked()
            }
        }
        videoBytes += q.size
        video.append(q)
        cond.signal()
        return nil
    }

    func flushVideo() -> Int {
        cond.lock()
        defer { cond.unlock() }
        return flushLocked()
    }

    private func flushLocked() -> Int {
        let before = video.count
        video.removeAll { $0.droppable }
        videoBytes = video.reduce(0) { $0 + $1.size }
        return before - video.count
    }

    /// Bloqueia até haver algo para enviar; `nil` depois de fechada.
    func pop() -> Item? {
        cond.lock()
        defer { cond.unlock() }
        while true {
            if closed { return nil }
            if !control.isEmpty { return control.removeFirst() }
            if !audio.isEmpty {
                let q = audio.removeFirst()
                audioBytes -= q.size
                return q.item
            }
            if !video.isEmpty {
                let q = video.removeFirst()
                videoBytes -= q.size
                return q.item
            }
            cond.wait()
        }
    }

    /// Bytes de vídeo na fila e idade do mais antigo (ms).
    func videoBacklog() -> (bytes: Int, ageMs: Double) {
        cond.lock()
        defer { cond.unlock() }
        let now = clock.nowUs()
        let age = video.first.map { now > $0.atUs ? Double(now - $0.atUs) / 1000 : 0 } ?? 0
        return (videoBytes, age)
    }

    func close() {
        cond.lock()
        closed = true
        cond.broadcast()
        cond.unlock()
    }
}

/// Junta pedidos de keyframe (PROTOCOL.md §7.2): no máximo um keyframe forçado a cada 250 ms.
/// O encoder pergunta em cada frame se tem de forçar um keyframe.
public final class KeyframeGate: @unchecked Sendable {
    private let lock = NSLock()
    private var pending = false
    private var lastForcedUs: UInt64?
    private var lastKeyframeUs: UInt64?
    private var periodicUs: UInt64 = 0
    public let minIntervalUs: UInt64

    public init(minIntervalUs: UInt64 = 250_000) {
        self.minIntervalUs = minIntervalUs
    }

    /// Pede um keyframe (novo cliente, `REQUEST_KEYFRAME`, descarte na fila).
    public func request() {
        lock.lock()
        pending = true
        lock.unlock()
    }

    /// `keyframe_interval_s` (0 = só a pedido).
    public func setPeriodic(seconds: Double) {
        lock.lock()
        periodicUs = seconds > 0 ? UInt64(seconds * 1_000_000) : 0
        lock.unlock()
    }

    /// Chamado pelo encoder antes de cada frame.
    public func shouldForce(nowUs: UInt64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if periodicUs > 0, let last = lastKeyframeUs, nowUs >= last + periodicUs {
            pending = true
        }
        guard pending else { return false }
        if let last = lastForcedUs, nowUs < last + minIntervalUs { return false }
        pending = false
        lastForcedUs = nowUs
        return true
    }

    /// O encoder produziu um keyframe (forçado ou não).
    public func keyframeProduced(nowUs: UInt64) {
        lock.lock()
        lastKeyframeUs = nowUs
        lock.unlock()
    }

    var isPending: Bool {
        lock.lock()
        defer { lock.unlock() }
        return pending
    }
}
