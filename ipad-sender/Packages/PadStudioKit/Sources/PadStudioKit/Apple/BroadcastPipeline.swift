#if canImport(VideoToolbox)
import CoreVideo
import Foundation
import os

/// Tudo o que corre na Broadcast Extension, sem ReplayKit nem UIKit (o `SampleHandler` extrai o
/// pixel buffer, o PTS e a orientação). Assim compila e testa-se também em macOS.
///
/// - Servidor PSWP em 127.0.0.1:23456 (túnel USB do usbmuxd).
/// - Só codifica com um PC ligado: sem PC, o custo no iPad é praticamente zero.
/// - Ecrã estático: repete o último frame a `min_fps` (ADR-08).
/// - Estatísticas a cada segundo com o diagnóstico dos spikes S1/S2/S6, também no log do sistema
///   (Console.app → subsistema "PadStudio").
public final class BroadcastPipeline: SenderDelegate, @unchecked Sendable {
    public struct Environment: Sendable {
        public var model: String
        public var system: String
        public var screenWidth: Int
        public var screenHeight: Int
        public var maxFps: Int

        public init(model: String, system: String, screenWidth: Int, screenHeight: Int, maxFps: Int) {
            self.model = model
            self.system = system
            self.screenWidth = screenWidth
            self.screenHeight = screenHeight
            self.maxFps = maxFps
        }
    }

    public let sender: Sender
    private let server: TCPServer
    private let encoder: VideoEncoder
    private let capture = CaptureMonitor()
    private let encodeMonitor: EncodeMonitor
    private let system = SystemMetrics()
    private let logger = Logger(subsystem: "PadStudio", category: "broadcast")

    /// Serializa a codificação (frames do ReplayKit e repetições do ecrã estático).
    private let queue = DispatchQueue(label: "padstudio.encode", qos: .userInteractive)
    private var lastBuffer: CVPixelBuffer?
    private var lastRotation = Rotation.none
    private var lastFrameUs: UInt64 = 0
    private var repeatTimer: DispatchSourceTimer?

    /// Falha se a porta 23456 estiver ocupada (ex.: outra transmissão ainda a terminar).
    public init(environment env: Environment) throws {
        let log = Logger(subsystem: "PadStudio", category: "sender")
        var o = Sender.Options()
        o.transport = .usb
        o.device = DeviceInfo(model: env.model, system: env.system, screenWidth: env.screenWidth,
                              screenHeight: env.screenHeight, maxFps: env.maxFps)
        o.limits.deviceMaxFps = max(env.maxFps, 1)
        o.log = { log.info("\($0, privacy: .public)") }
        var settings = EncoderSettings.usb
        settings.maxFps = min(settings.maxFps, o.limits.deviceMaxFps)
        let sender = Sender(options: o, settings: settings)
        let monitor = EncodeMonitor()
        self.sender = sender
        encodeMonitor = monitor
        encoder = VideoEncoder(settings: settings, gate: sender.keyframes, monitor: monitor)
        server = try TCPServer(interface: .loopback, port: PadStudioKit.usbPort)

        sender.delegate = self
        sender.addStatsSource(capture)
        sender.addStatsSource(encodeMonitor)
        sender.addStatsSource(system)
        sender.addStatsSource(StatsLogger(logger: logger))
        encoder.onFrame = { [weak sender] frame in sender?.submitVideo(frame) }
        encoder.onError = { [logger] message in logger.error("\(message, privacy: .public)") }
    }

    public func start() {
        let sender = self.sender
        server.start { stream in sender.serve(stream, peer: stream.peer) }
        sender.start()
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + .milliseconds(20), repeating: .milliseconds(20))
        timer.setEventHandler { [weak self] in self?.repeatIfStatic() }
        timer.resume()
        repeatTimer = timer
        logger.info("à escuta em 127.0.0.1:\(PadStudioKit.usbPort, privacy: .public)")
    }

    public func stop() {
        repeatTimer?.cancel()
        repeatTimer = nil
        sender.stop()
        server.stop()
        queue.sync {
            lastBuffer = nil
            encoder.invalidate()
        }
    }

    public func pause() { sender.setBroadcastState(.paused) }

    public func resume() { sender.setBroadcastState(.active) }

    /// Frame de vídeo do ReplayKit. `orientation` = `RPVideoSampleOrientationKey` (EXIF).
    public func video(_ pixelBuffer: CVPixelBuffer, ptsUs: UInt64, orientation: UInt32?) {
        let rotation = orientation.map(Rotation.init(exifOrientation:)) ?? .none
        capture.frameArrived(
            ptsUs: ptsUs, width: CVPixelBufferGetWidth(pixelBuffer), height: CVPixelBufferGetHeight(pixelBuffer),
            pixelFormat: fourCC(CVPixelBufferGetPixelFormatType(pixelBuffer)), orientation: orientation.map { Int($0) },
            rotation: rotation
        )
        let hasClient = sender.hasClient
        let keepLast = sender.settings.minFps > 0
        queue.sync {
            lastFrameUs = HostClock.system.nowUs()
            lastRotation = rotation
            lastBuffer = keepLast ? pixelBuffer : nil
            guard hasClient else { return }
            encoder.encode(pixelBuffer, ptsUs: ptsUs, rotation: rotation)
        }
    }

    /// Ecrã estático: o ReplayKit não entrega frames, mas o PC e as plataformas precisam de vídeo contínuo.
    private func repeatIfStatic() {
        let minFps = sender.settings.minFps
        guard minFps > 0, let buffer = lastBuffer, sender.hasClient else { return }
        let now = HostClock.system.nowUs()
        guard now >= lastFrameUs + UInt64(1_000_000 / minFps) else { return }
        lastFrameUs = now
        capture.frameRepeated()
        encoder.encode(buffer, ptsUs: now, rotation: lastRotation)
    }

    // ── SenderDelegate ──────────────────────────────────────────────────────

    public func sender(_: Sender, didChangeSettings settings: EncoderSettings, from _: EncoderSettings) {
        encoder.update(settings: settings)
    }

    public func sender(_: Sender, clientConnected peer: String) {
        logger.info("PC ligado: \(peer, privacy: .public)")
    }

    public func sender(_: Sender, clientDisconnected peer: String) {
        logger.info("PC desligado: \(peer, privacy: .public)")
        // Sem PC não se codifica: liberta a sessão VT e o último frame.
        queue.async { [weak self] in
            guard let self, !self.sender.hasClient else { return }
            self.encoder.invalidate()
        }
    }
}

/// Escreve um resumo do diagnóstico no log do sistema, a cada segundo (a última fonte a contribuir).
private final class StatsLogger: StatsSource {
    let logger: Logger

    init(logger: Logger) { self.logger = logger }

    func contribute(to s: inout SenderStats) {
        let c = s.diag?.capture
        let e = s.diag?.encode
        let m = s.diag?.memory
        logger.info("""
        captura \(c?.fps ?? 0, privacy: .public) fps (p50 \(c?.intervalMsP50 ?? 0, privacy: .public) ms, \
        máx \(c?.intervalMsMax ?? 0, privacy: .public) ms) · encoder \(e?.msP50 ?? 0, privacy: .public)/\(e?.msMax ?? 0, privacy: .public) ms \
        \(e?.outputWidth ?? 0, privacy: .public)x\(e?.outputHeight ?? 0, privacy: .public) · \
        memória \(m?.footprintMb ?? 0, privacy: .public)/\(m?.limitMb ?? 0, privacy: .public) MB · \
        CPU \(s.diag?.cpuPercent ?? 0, privacy: .public) % · fila \(s.sendQueueMs ?? 0, privacy: .public) ms · \
        drops \(s.framesDropped ?? 0, privacy: .public) · \(s.thermal?.rawValue ?? "?", privacy: .public)
        """)
    }
}
#endif
