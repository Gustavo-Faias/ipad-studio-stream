import CoreMedia
import PadStudioKit
import ReplayKit
import UIKit

/// Ponto de entrada da Broadcast Upload Extension.
///
/// Cada `CMSampleBuffer` de vídeo (IOSurface) vai direto para o `VTCompressionSession` através do
/// `BroadcastPipeline`. Não usar CIImage, Metal nem cópias de píxeis aqui (ARCHITECTURE.md, R1).
/// O áudio chega no passo 4b.
final class SampleHandler: RPBroadcastSampleHandler {
    private var pipeline: BroadcastPipeline?

    override func broadcastStarted(withSetupInfo _: [String: NSObject]?) {
        let screen = UIScreen.main
        let device = UIDevice.current
        let env = BroadcastPipeline.Environment(
            model: SystemMetrics.machineModel(),
            system: "\(device.systemName) \(device.systemVersion)",
            screenWidth: Int(screen.nativeBounds.width),
            screenHeight: Int(screen.nativeBounds.height),
            maxFps: screen.maximumFramesPerSecond
        )
        do {
            let p = try BroadcastPipeline(environment: env)
            p.start()
            pipeline = p
        } catch {
            let message = "Não foi possível abrir a porta \(PadStudioKit.usbPort) para o PC: \(error)"
            finishBroadcastWithError(NSError(domain: "PadStudio", code: 1, userInfo: [NSLocalizedDescriptionKey: message]))
        }
    }

    override func broadcastPaused() {
        pipeline?.pause()
    }

    override func broadcastResumed() {
        pipeline?.resume()
    }

    override func broadcastFinished() {
        pipeline?.stop()
        pipeline = nil
    }

    override func processSampleBuffer(_ sampleBuffer: CMSampleBuffer, with sampleBufferType: RPSampleBufferType) {
        guard sampleBufferType == .video, let pipeline, let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else {
            return // áudio: passo 4b
        }
        // O PTS do ReplayKit está no host time (o mesmo relógio do PING/PONG, PROTOCOL.md §6).
        // `diag.capture.lag_ms` no PC confirma-o: valores enormes indicariam outro relógio.
        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        let ptsUs = pts.isValid
            ? UInt64(max(0, CMTimeConvertScale(pts, timescale: 1_000_000, method: .roundTowardZero).value))
            : HostClock.system.nowUs()
        let orientation = (CMGetAttachment(sampleBuffer, key: RPVideoSampleOrientationKey as CFString, attachmentModeOut: nil)
            as? NSNumber)?.uint32Value
        pipeline.video(pixelBuffer, ptsUs: ptsUs, orientation: orientation)
    }
}
