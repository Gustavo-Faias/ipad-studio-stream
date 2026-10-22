import Foundation

// Medições dos spikes do passo 4, enviadas ao PC em `SENDER_STATS.diag` (janela de ~1 s):
//
// - S1 · O ReplayKit entrega 120 fps na extensão? → `capture` (ritmo real, histograma dos
//   intervalos, atraso entre o PTS e a chegada ao callback, orientação recebida).
// - S2 · A escala dentro do VideoToolbox pesa? → `encode` (tempo por frame, frames em voo,
//   dimensões nativas e de saída) + `cpu_percent`; compara-se nativo vs. escalado pela UI do PC.
// - S6 · Limite de ~50 MB da extensão → `memory` (footprint, pico, margem até ao limite).

/// Bloco `diag` do `SENDER_STATS`.
public struct SenderDiagnostics: Codable, Equatable, Sendable {
    public var capture: CaptureDiagnostics?
    public var encode: EncodeDiagnostics?
    public var memory: MemoryDiagnostics?
    public var cpuPercent: Double?
    public var notes: [String]?

    public init() {}

    enum CodingKeys: String, CodingKey {
        case capture, encode, memory, notes
        case cpuPercent = "cpu_percent"
    }
}

/// Histograma dos intervalos entre frames entregues (ms). A 120 Hz o intervalo é 8,3 ms; a 60 Hz, 16,7 ms.
public struct IntervalHistogram: Codable, Equatable, Sendable {
    public var lt6 = 0
    public var from6to10 = 0
    public var from10to14 = 0
    public var from14to20 = 0
    public var from20to40 = 0
    public var ge40 = 0

    public init() {}

    enum CodingKeys: String, CodingKey {
        case lt6 = "lt6"
        case from6to10 = "6to10"
        case from10to14 = "10to14"
        case from14to20 = "14to20"
        case from20to40 = "20to40"
        case ge40 = "ge40"
    }

    public mutating func add(ms: Double) {
        switch ms {
        case ..<6: lt6 += 1
        case ..<10.5: from6to10 += 1
        case ..<14: from10to14 += 1
        case ..<20: from14to20 += 1
        case ..<40: from20to40 += 1
        default: ge40 += 1
        }
    }

    public var total: Int { lt6 + from6to10 + from10to14 + from14to20 + from20to40 + ge40 }
}

/// S1 — ritmo de captura do ReplayKit.
public struct CaptureDiagnostics: Codable, Equatable, Sendable {
    /// Frames entregues pelo ReplayKit nesta janela, por segundo.
    public var fps: Double?
    public var intervalMsP50: Double?
    public var intervalMsP95: Double?
    public var intervalMsMax: Double?
    public var intervals: IntervalHistogram?
    /// Chegada ao callback − PTS do frame (ms). Valores enormes = o PTS não está no host time.
    public var lagMsP50: Double?
    public var lagMsMax: Double?
    /// Frames repetidos por ecrã estático (`min_fps`) nesta janela.
    public var repeats: Int?
    public var width: Int?
    public var height: Int?
    public var pixelFormat: String?
    /// Último `RPVideoSampleOrientationKey` recebido (EXIF) e a rotação enviada ao PC.
    public var orientation: Int?
    public var rotation: Int?

    public init() {}

    enum CodingKeys: String, CodingKey {
        case fps, intervals, repeats, width, height, orientation, rotation
        case intervalMsP50 = "interval_ms_p50"
        case intervalMsP95 = "interval_ms_p95"
        case intervalMsMax = "interval_ms_max"
        case lagMsP50 = "lag_ms_p50"
        case lagMsMax = "lag_ms_max"
        case pixelFormat = "pixel_format"
    }
}

/// S2 — custo do encoder.
public struct EncodeDiagnostics: Codable, Equatable, Sendable {
    public var codec: String?
    /// Resolução recebida do ReplayKit e resolução codificada.
    public var inputWidth: Int?
    public var inputHeight: Int?
    public var outputWidth: Int?
    public var outputHeight: Int?
    public var scaled: Bool?
    /// Tempo entre entregar o frame ao VideoToolbox e receber o resultado.
    public var msP50: Double?
    public var msP95: Double?
    public var msMax: Double?
    public var inFlightMax: Int?
    public var errors: Int?
    /// Frames que o encoder recusou (`kVTEncodeInfo_FrameDropped`).
    public var encoderDrops: Int?
    /// Vezes que a sessão VT foi (re)criada desde o início da transmissão.
    public var sessions: Int?

    public init() {}

    enum CodingKeys: String, CodingKey {
        case codec, scaled, errors, sessions
        case inputWidth = "input_width"
        case inputHeight = "input_height"
        case outputWidth = "output_width"
        case outputHeight = "output_height"
        case msP50 = "ms_p50"
        case msP95 = "ms_p95"
        case msMax = "ms_max"
        case inFlightMax = "in_flight_max"
        case encoderDrops = "encoder_drops"
    }
}

/// S6 — memória da extensão (o sistema mata-a acima do limite, ~50 MB).
public struct MemoryDiagnostics: Codable, Equatable, Sendable {
    /// `phys_footprint` (o valor que o jetsam usa).
    public var footprintMb: Double?
    public var peakMb: Double?
    /// `os_proc_available_memory()`: margem até ao limite.
    public var availableMb: Double?
    /// footprint + margem = limite efetivo deste processo.
    public var limitMb: Double?

    public init() {}

    enum CodingKeys: String, CodingKey {
        case footprintMb = "footprint_mb"
        case peakMb = "peak_mb"
        case availableMb = "available_mb"
        case limitMb = "limit_mb"
    }
}

// ── Medidores ───────────────────────────────────────────────────────────────

/// Percentil de uma lista já ordenada (`p` em 0…1), como no `probe` do PC.
func percentile(_ sorted: [Double], _ p: Double) -> Double? {
    guard !sorted.isEmpty else { return nil }
    let i = Int((Double(sorted.count - 1) * p).rounded())
    return sorted[min(max(i, 0), sorted.count - 1)]
}

/// Arredonda para `digits` casas decimais (JSON mais curto e legível).
func rounded(_ v: Double?, _ digits: Int = 2) -> Double? {
    guard let v, v.isFinite else { return nil }
    let f = pow(10.0, Double(digits))
    return (v * f).rounded() / f
}

/// Valores de uma janela: percentis e máximo. Thread-safe.
public final class SampleWindow: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [Double] = []
    private let capacity: Int

    public init(capacity: Int = 1024) {
        self.capacity = capacity
        values.reserveCapacity(capacity)
    }

    public func add(_ v: Double) {
        lock.lock()
        if values.count < capacity { values.append(v) }
        lock.unlock()
    }

    /// Devolve os valores ordenados e começa uma janela nova.
    public func drain() -> [Double] {
        lock.lock()
        let out = values
        values.removeAll(keepingCapacity: true)
        lock.unlock()
        return out.sorted()
    }
}

/// S1: chamado em cada frame de vídeo entregue pelo ReplayKit (ou pelo simulador).
public final class CaptureMonitor: StatsSource, @unchecked Sendable {
    private let lock = NSLock()
    private var lastArrivalUs: UInt64?
    private var frames = 0
    private var repeats = 0
    private var histogram = IntervalHistogram()
    private let intervals = SampleWindow()
    private let lags = SampleWindow()
    private var windowStartUs: UInt64?
    private var width = 0
    private var height = 0
    private var pixelFormat: String?
    private var orientation: Int?
    private var rotation: Rotation = .none
    private let clock: HostClock

    public init(clock: HostClock = .system) {
        self.clock = clock
    }

    /// Um frame novo chegou ao callback. `ptsUs` é o PTS do frame (host time em µs).
    public func frameArrived(ptsUs: UInt64, width: Int, height: Int, pixelFormat: String?, orientation: Int?, rotation: Rotation) {
        let now = clock.nowUs()
        lock.lock()
        defer { lock.unlock() }
        if windowStartUs == nil { windowStartUs = now }
        if let last = lastArrivalUs, now > last {
            let ms = Double(now - last) / 1000
            histogram.add(ms: ms)
            intervals.add(ms)
        }
        lastArrivalUs = now
        frames += 1
        lags.add((Double(now) - Double(ptsUs)) / 1000)
        self.width = width
        self.height = height
        self.pixelFormat = pixelFormat
        self.orientation = orientation
        self.rotation = rotation
    }

    /// O último frame foi recodificado por ecrã estático (`min_fps`, ADR-08).
    public func frameRepeated() {
        lock.lock()
        repeats += 1
        lock.unlock()
    }

    public func contribute(to stats: inout SenderStats) {
        let now = clock.nowUs()
        lock.lock()
        let elapsedS = windowStartUs.map { now > $0 ? Double(now - $0) / 1_000_000 : 0 } ?? 0
        let (frames, repeats, histogram) = (self.frames, self.repeats, self.histogram)
        let (width, height, pixelFormat, orientation, rotation) = (self.width, self.height, self.pixelFormat, self.orientation, self.rotation)
        self.frames = 0
        self.repeats = 0
        self.histogram = IntervalHistogram()
        windowStartUs = now
        lock.unlock()

        let iv = intervals.drain()
        let lag = lags.drain()
        let fps = elapsedS > 0.2 ? Double(frames) / elapsedS : nil
        stats.fpsCapture = rounded(fps, 1)
        guard width > 0 else { return }
        stats.withDiag { d in
            var c = CaptureDiagnostics()
            c.fps = rounded(fps, 1)
            c.intervalMsP50 = rounded(percentile(iv, 0.5))
            c.intervalMsP95 = rounded(percentile(iv, 0.95))
            c.intervalMsMax = rounded(iv.last)
            c.intervals = histogram
            c.lagMsP50 = rounded(percentile(lag, 0.5))
            c.lagMsMax = rounded(lag.last)
            c.repeats = repeats
            c.width = width
            c.height = height
            c.pixelFormat = pixelFormat
            c.orientation = orientation
            c.rotation = rotation.degrees
            d.capture = c
        }
    }
}

/// S2: tempos do encoder.
public final class EncodeMonitor: StatsSource, @unchecked Sendable {
    private let lock = NSLock()
    private let durations = SampleWindow()
    private var inFlight = 0
    private var inFlightMax = 0
    private var errors = 0
    private var encoderDrops = 0
    private var sessions = 0
    private var codec: VideoCodec?
    private var input = (0, 0)
    private var output = (0, 0)
    private var notes: [String] = []

    public init() {}

    /// `notes`: propriedades do VideoToolbox que o encoder recusou (aparecem em `diag.notes`).
    public func sessionCreated(codec: VideoCodec, inputWidth: Int, inputHeight: Int, outputWidth: Int, outputHeight: Int, notes: [String] = []) {
        lock.lock()
        sessions += 1
        self.codec = codec
        input = (inputWidth, inputHeight)
        output = (outputWidth, outputHeight)
        self.notes = notes
        lock.unlock()
    }

    public func submitted() {
        lock.lock()
        inFlight += 1
        inFlightMax = max(inFlightMax, inFlight)
        lock.unlock()
    }

    public func completed(ms: Double) {
        durations.add(ms)
        lock.lock()
        inFlight = max(0, inFlight - 1)
        lock.unlock()
    }

    public func failed(dropped: Bool) {
        lock.lock()
        inFlight = max(0, inFlight - 1)
        if dropped { encoderDrops += 1 } else { errors += 1 }
        lock.unlock()
    }

    public func contribute(to stats: inout SenderStats) {
        let d = durations.drain()
        lock.lock()
        let snapshot = (codec, input, output, inFlightMax, errors, encoderDrops, sessions)
        let notes = self.notes
        inFlightMax = inFlight
        lock.unlock()
        let (codec, input, output, inFlightMax, errors, encoderDrops, sessions) = snapshot
        guard let codec else { return }
        if !notes.isEmpty {
            stats.withDiag { $0.notes = ($0.notes ?? []) + notes }
        }
        if !d.isEmpty {
            stats.encodeMsAvg = rounded(d.reduce(0, +) / Double(d.count))
        }
        stats.withDiag { diag in
            var e = EncodeDiagnostics()
            e.codec = codec.name
            e.inputWidth = input.0
            e.inputHeight = input.1
            e.outputWidth = output.0
            e.outputHeight = output.1
            e.scaled = input != output
            e.msP50 = rounded(percentile(d, 0.5))
            e.msP95 = rounded(percentile(d, 0.95))
            e.msMax = rounded(d.last)
            e.inFlightMax = inFlightMax
            e.errors = errors
            e.encoderDrops = encoderDrops
            e.sessions = sessions
            diag.encode = e
        }
    }
}
