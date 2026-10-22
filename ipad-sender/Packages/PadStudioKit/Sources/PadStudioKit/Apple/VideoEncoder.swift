#if canImport(VideoToolbox)
import CoreMedia
import CoreVideo
import Foundation
import VideoToolbox

/// Encoder de hardware em tempo real (ARCHITECTURE.md §4.1): o `CVPixelBuffer` do ReplayKit
/// (IOSurface) entra direto no `VTCompressionSession`, sem cópias nem GPU.
///
/// `encode` é chamado sempre da mesma fila (a do pipeline). O resultado pode chegar dentro da
/// própria chamada (`MaxFrameDelayCount = 0`) ou noutra thread do VideoToolbox; por isso o
/// VideoToolbox nunca é chamado com o `lock` tomado.
public final class VideoEncoder: @unchecked Sendable {
    /// Frames prontos a enviar (thread do VideoToolbox).
    public var onFrame: ((EncodedFrame) -> Void)?
    public var onError: ((String) -> Void)?

    private let lock = NSLock()
    private var settings: EncoderSettings
    private var session: VTCompressionSession?
    private var needsNewSession = true
    private var input = (width: 0, height: 0)
    private var output = (width: 0, height: 0)
    private var color = ColorInfo()
    private var configKey: ConfigKey?
    private var configId: UInt64 = 0
    private var config: VideoConfig?

    private let gate: KeyframeGate
    private let monitor: EncodeMonitor
    private let clock: HostClock

    public init(settings: EncoderSettings, gate: KeyframeGate, monitor: EncodeMonitor, clock: HostClock = .system) {
        self.settings = settings
        self.gate = gate
        self.monitor = monitor
        self.clock = clock
    }

    deinit {
        invalidate()
    }

    /// Nova configuração do PC. O bitrate muda em direto; o resto recria a sessão no frame seguinte.
    public func update(settings new: EncoderSettings) {
        lock.lock()
        let old = settings
        settings = new
        let rebuild = new.needsNewSession(comparedTo: old)
        if rebuild { needsNewSession = true }
        let live = rebuild ? nil : session
        lock.unlock()
        if let live, new.bitrateBps != old.bitrateBps {
            _ = Self.applyBitrate(live, new)
        }
    }

    /// Termina a sessão (fim da transmissão).
    public func invalidate() {
        lock.lock()
        let s = session
        session = nil
        needsNewSession = true
        lock.unlock()
        if let s {
            VTCompressionSessionCompleteFrames(s, untilPresentationTimeStamp: .invalid)
            VTCompressionSessionInvalidate(s)
        }
    }

    /// Codifica um frame. `ptsUs` é o PTS de captura no host time.
    public func encode(_ pixelBuffer: CVPixelBuffer, ptsUs: UInt64, rotation: Rotation) {
        let w = CVPixelBufferGetWidth(pixelBuffer)
        let h = CVPixelBufferGetHeight(pixelBuffer)

        lock.lock()
        let stale: VTCompressionSession?
        let mustCreate = needsNewSession || session == nil || input.width != w || input.height != h
        if mustCreate {
            stale = session
            session = nil
            needsNewSession = false
        } else {
            stale = nil
        }
        let current = settings
        lock.unlock()

        if let stale {
            VTCompressionSessionCompleteFrames(stale, untilPresentationTimeStamp: .invalid)
            VTCompressionSessionInvalidate(stale)
        }
        if mustCreate {
            guard createSession(for: pixelBuffer, width: w, height: h, settings: current) else { return }
        }

        lock.lock()
        let session = self.session
        lock.unlock()
        guard let session else { return }

        let submitUs = clock.nowUs()
        let force = gate.shouldForce(nowUs: submitUs)
        let properties: CFDictionary? = force
            ? [kVTEncodeFrameOptionKey_ForceKeyFrame: kCFBooleanTrue as Any] as CFDictionary
            : nil
        monitor.submitted()
        let pts = CMTime(value: CMTimeValue(ptsUs), timescale: 1_000_000)
        let status = VTCompressionSessionEncodeFrame(
            session, imageBuffer: pixelBuffer, presentationTimeStamp: pts, duration: .invalid,
            frameProperties: properties, infoFlagsOut: nil
        ) { [weak self] status, flags, sample in
            self?.handleOutput(status: status, flags: flags, sample: sample, ptsUs: ptsUs, rotation: rotation,
                               submitUs: submitUs, codec: current.codec, fps: current.maxFps)
        }
        if status != noErr {
            monitor.failed(dropped: false)
            if force { gate.request() }
            if status == kVTInvalidSessionErr {
                lock.lock()
                needsNewSession = true
                lock.unlock()
            }
            onError?("VTCompressionSessionEncodeFrame: \(status)")
        }
    }

    // ── Sessão ──────────────────────────────────────────────────────────────

    private func createSession(for pixelBuffer: CVPixelBuffer, width: Int, height: Int, settings: EncoderSettings) -> Bool {
        let out = settings.outputSize(width: width, height: height)
        var spec: [CFString: Any] = [:]
        if settings.codec == .h264 {
            // Modo de baixa latência (só H.264): saída imediata, controlo de ritmo por frame.
            spec[kVTVideoEncoderSpecification_EnableLowLatencyRateControl] = true
        }
        var created: VTCompressionSession?
        let status = VTCompressionSessionCreate(
            allocator: kCFAllocatorDefault,
            width: Int32(out.width), height: Int32(out.height),
            codecType: settings.codec == .hevc ? kCMVideoCodecType_HEVC : kCMVideoCodecType_H264,
            encoderSpecification: spec as CFDictionary,
            imageBufferAttributes: nil, compressedDataAllocator: nil,
            outputCallback: nil, refcon: nil, compressionSessionOut: &created
        )
        guard status == noErr, let s = created else {
            monitor.failed(dropped: false)
            onError?("VTCompressionSessionCreate \(settings.codec.name) \(out.width)x\(out.height): \(status)")
            lock.lock()
            needsNewSession = true
            lock.unlock()
            return false
        }

        let color = ColorInfo(pixelBuffer: pixelBuffer)
        var notes: [String] = []
        func set(_ key: CFString, _ value: Any, _ name: String) {
            let st = VTSessionSetProperty(s, key: key, value: value as CFTypeRef)
            if st != noErr { notes.append("VT \(name) recusado (\(st))") }
        }
        set(kVTCompressionPropertyKey_RealTime, kCFBooleanTrue as Any, "RealTime")
        set(kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse as Any, "AllowFrameReordering")
        if settings.codec == .h264 {
            // Constrained High = High sem B-frames (o perfil que o modo de baixa latência aceita).
            if VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ProfileLevel, value: kVTProfileLevel_H264_ConstrainedHigh_AutoLevel) != noErr {
                set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_High_AutoLevel, "ProfileLevel")
            }
        } else {
            set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_HEVC_Main_AutoLevel, "ProfileLevel")
        }
        set(kVTCompressionPropertyKey_MaxFrameDelayCount, NSNumber(value: 0), "MaxFrameDelayCount")
        set(kVTCompressionPropertyKey_ExpectedFrameRate, NSNumber(value: settings.maxFps), "ExpectedFrameRate")
        // Sem keyframes periódicos no encoder: o KeyframeGate decide (pedidos do PC e keyframe_interval_s).
        set(kVTCompressionPropertyKey_MaxKeyFrameInterval, NSNumber(value: 0), "MaxKeyFrameInterval")
        set(kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, NSNumber(value: 0), "MaxKeyFrameIntervalDuration")
        set(kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, kCFBooleanTrue as Any, "PrioritizeEncodingSpeedOverQuality")
        notes += Self.applyBitrate(s, settings)
        if let p = color.primariesKey { set(kVTCompressionPropertyKey_ColorPrimaries, p, "ColorPrimaries") }
        if let t = color.transferKey { set(kVTCompressionPropertyKey_TransferFunction, t, "TransferFunction") }
        if let m = color.matrixKey { set(kVTCompressionPropertyKey_YCbCrMatrix, m, "YCbCrMatrix") }
        VTCompressionSessionPrepareToEncodeFrames(s)

        lock.lock()
        session = s
        input = (width, height)
        output = out
        self.color = color
        lock.unlock()
        monitor.sessionCreated(codec: settings.codec, inputWidth: width, inputHeight: height,
                               outputWidth: out.width, outputHeight: out.height, notes: notes)
        gate.request() // o primeiro frame de uma sessão nova é keyframe; garante-se o pedido
        return true
    }

    /// VBR com teto de 1,5× o alvo, ou CBR (PROTOCOL.md §5.3). Devolve notas de propriedades recusadas.
    @discardableResult
    static func applyBitrate(_ s: VTCompressionSession, _ settings: EncoderSettings) -> [String] {
        var notes: [String] = []
        if settings.bitrateMode == .cbr {
            let st = VTSessionSetProperty(s, key: kVTCompressionPropertyKey_ConstantBitRate, value: NSNumber(value: settings.bitrateBps))
            if st == noErr { return notes }
            notes.append("VT ConstantBitRate recusado (\(st)); a usar VBR")
        }
        let st = VTSessionSetProperty(s, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: settings.bitrateBps))
        if st != noErr { notes.append("VT AverageBitRate recusado (\(st))") }
        let bytesPerSecond = Double(settings.bitrateBps) * 1.5 / 8
        let limits = [NSNumber(value: bytesPerSecond), NSNumber(value: 1.0)] as CFArray
        let st2 = VTSessionSetProperty(s, key: kVTCompressionPropertyKey_DataRateLimits, value: limits)
        if st2 != noErr { notes.append("VT DataRateLimits recusado (\(st2))") }
        return notes
    }

    // ── Saída ───────────────────────────────────────────────────────────────

    private func handleOutput(
        status: OSStatus, flags: VTEncodeInfoFlags, sample: CMSampleBuffer?,
        ptsUs: UInt64, rotation: Rotation, submitUs: UInt64, codec: VideoCodec, fps: Int
    ) {
        if flags.contains(.frameDropped) {
            monitor.failed(dropped: true)
            return
        }
        guard status == noErr, let sample, let format = CMSampleBufferGetFormatDescription(sample),
              let block = CMSampleBufferGetDataBuffer(sample)
        else {
            monitor.failed(dropped: false)
            if status != noErr { onError?("encoder: \(status)") }
            return
        }
        monitor.completed(ms: Double(clock.nowUs() &- submitUs) / 1000)

        let keyframe = Self.isKeyframe(sample)
        let (paramSets, nalLength) = Self.parameterSets(format, codec)
        let packet: FramePacket
        do {
            packet = try Self.packet(block, nalLength: nalLength, codec: codec, paramSets: keyframe ? paramSets : nil)
        } catch {
            onError?("\(error)")
            return
        }

        lock.lock()
        let dims = CMVideoFormatDescriptionGetDimensions(format)
        let key = ConfigKey(codec: codec, width: Int(dims.width), height: Int(dims.height), fps: fps, color: color, paramSets: paramSets)
        if keyframe, key != configKey {
            configKey = key
            configId += 1
            config = VideoConfig(
                codec: codec, fpsMax: UInt16(clamping: fps), width: UInt16(clamping: Int(dims.width)),
                height: UInt16(clamping: Int(dims.height)), bitrateBps: UInt32(clamping: settings.bitrateBps),
                colorPrimaries: color.primaries, transfer: color.transfer, matrix: color.matrix, fullRange: color.fullRange,
                paramSets: NAL.join(paramSets)
            )
        }
        let id = configId
        let cfg = config
        lock.unlock()
        guard let cfg else { return } // nenhum keyframe ainda (não acontece: o 1.º frame é keyframe)
        onFrame?(EncodedFrame(packet: packet, keyframe: keyframe, ptsUs: ptsUs, rotation: rotation, configId: id, config: cfg))
    }

    static func isKeyframe(_ sample: CMSampleBuffer) -> Bool {
        guard let array = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[CFString: Any]],
              let first = array.first
        else { return true }
        return !((first[kCMSampleAttachmentKey_NotSync] as? Bool) ?? false)
    }

    /// VPS/SPS/PPS do format description e o tamanho do prefixo das NALs (normalmente 4).
    static func parameterSets(_ format: CMFormatDescription, _ codec: VideoCodec) -> ([Data], Int) {
        func get(_ index: Int, _ pointer: UnsafeMutablePointer<UnsafePointer<UInt8>?>?, _ size: UnsafeMutablePointer<Int>?,
                 _ count: UnsafeMutablePointer<Int>?, _ nal: UnsafeMutablePointer<Int32>?) -> OSStatus {
            switch codec {
            case .h264:
                CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    format, parameterSetIndex: index, parameterSetPointerOut: pointer, parameterSetSizeOut: size,
                    parameterSetCountOut: count, nalUnitHeaderLengthOut: nal
                )
            case .hevc:
                CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    format, parameterSetIndex: index, parameterSetPointerOut: pointer, parameterSetSizeOut: size,
                    parameterSetCountOut: count, nalUnitHeaderLengthOut: nal
                )
            }
        }
        var count = 0
        var nalLength: Int32 = 4
        guard get(0, nil, nil, &count, &nalLength) == noErr else { return ([], 4) }
        var sets: [Data] = []
        for i in 0 ..< count {
            var pointer: UnsafePointer<UInt8>?
            var size = 0
            if get(i, &pointer, &size, nil, nil) == noErr, let pointer, size > 0 {
                sets.append(Data(bytes: pointer, count: size))
            }
        }
        return (sets, Int(nalLength))
    }

    /// AVCC → Anexo-B durante a única cópia que sai do buffer do encoder.
    static func packet(_ block: CMBlockBuffer, nalLength: Int, codec: VideoCodec, paramSets: [Data]?) throws -> FramePacket {
        var lengthAtOffset = 0
        var total = 0
        var pointer: UnsafeMutablePointer<CChar>?
        let st = CMBlockBufferGetDataPointer(block, atOffset: 0, lengthAtOffsetOut: &lengthAtOffset,
                                             totalLengthOut: &total, dataPointerOut: &pointer)
        if st == kCMBlockBufferNoErr, let pointer, lengthAtOffset == total {
            return try FramePacket.fromAVCC(UnsafeRawBufferPointer(start: pointer, count: total),
                                            nalLengthSize: nalLength, codec: codec, paramSets: paramSets)
        }
        // Buffer não contíguo: copia primeiro.
        let length = CMBlockBufferGetDataLength(block)
        var bytes = [UInt8](repeating: 0, count: length)
        let copied = bytes.withUnsafeMutableBytes {
            CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: length, destination: $0.baseAddress!)
        }
        guard copied == kCMBlockBufferNoErr else {
            throw MalformedBitstream(description: "CMBlockBufferCopyDataBytes: \(copied)")
        }
        return try bytes.withUnsafeBytes {
            try FramePacket.fromAVCC($0, nalLengthSize: nalLength, codec: codec, paramSets: paramSets)
        }
    }
}

/// O que define um `VIDEO_CONFIG` (o bitrate não conta: muda em direto sem reiniciar nada no PC).
private struct ConfigKey: Equatable {
    var codec: VideoCodec
    var width: Int
    var height: Int
    var fps: Int
    var color: ColorInfo
    var paramSets: [Data]
}

/// Cor do `CVPixelBuffer` do ReplayKit, em códigos H.273 (para o `VIDEO_CONFIG`) e nas chaves do VT.
struct ColorInfo: Equatable {
    var primaries: UInt8 = 1
    var transfer: UInt8 = 1
    var matrix: UInt8 = 1
    var fullRange = false
    var primariesKey: CFString?
    var transferKey: CFString?
    var matrixKey: CFString?

    init() {}

    init(pixelBuffer: CVPixelBuffer) {
        let format = CVPixelBufferGetPixelFormatType(pixelBuffer)
        fullRange = format == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
            || format == kCVPixelFormatType_420YpCbCr10BiPlanarFullRange
        let attachments = CVBufferCopyAttachments(pixelBuffer, .shouldPropagate) as? [CFString: Any] ?? [:]
        // Código H.273 2 = "não especificado".
        if let p = attachments[kCVImageBufferColorPrimariesKey] as? String {
            primariesKey = p as CFString
            primaries = Self.primariesCodes[p] ?? 2
        }
        if let t = attachments[kCVImageBufferTransferFunctionKey] as? String {
            transferKey = t as CFString
            transfer = Self.transferCodes[t] ?? 2
        }
        if let m = attachments[kCVImageBufferYCbCrMatrixKey] as? String {
            matrixKey = m as CFString
            matrix = Self.matrixCodes[m] ?? 2
        }
    }

    private static let primariesCodes: [String: UInt8] = [
        kCVImageBufferColorPrimaries_ITU_R_709_2 as String: 1,
        kCVImageBufferColorPrimaries_EBU_3213 as String: 5,
        kCVImageBufferColorPrimaries_SMPTE_C as String: 6,
        kCVImageBufferColorPrimaries_ITU_R_2020 as String: 9,
        kCVImageBufferColorPrimaries_P3_D65 as String: 12,
    ]

    private static let transferCodes: [String: UInt8] = [
        kCVImageBufferTransferFunction_ITU_R_709_2 as String: 1,
        kCVImageBufferTransferFunction_Linear as String: 8,
        kCVImageBufferTransferFunction_sRGB as String: 13,
        kCVImageBufferTransferFunction_ITU_R_2020 as String: 14,
        kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ as String: 16,
        kCVImageBufferTransferFunction_ITU_R_2100_HLG as String: 18,
    ]

    private static let matrixCodes: [String: UInt8] = [
        kCVImageBufferYCbCrMatrix_ITU_R_709_2 as String: 1,
        kCVImageBufferYCbCrMatrix_ITU_R_601_4 as String: 6,
        kCVImageBufferYCbCrMatrix_ITU_R_2020 as String: 9,
    ]

    static func == (a: ColorInfo, b: ColorInfo) -> Bool {
        a.primaries == b.primaries && a.transfer == b.transfer && a.matrix == b.matrix && a.fullRange == b.fullRange
    }
}

/// FourCC legível (ex.: "420f").
func fourCC(_ code: OSType) -> String {
    let bytes = [24, 16, 8, 0].map { UInt8(truncatingIfNeeded: code >> $0) }
    return bytes.allSatisfy({ (0x20 ... 0x7e).contains($0) }) ? String(decoding: bytes, as: UTF8.self) : String(code)
}
#endif
