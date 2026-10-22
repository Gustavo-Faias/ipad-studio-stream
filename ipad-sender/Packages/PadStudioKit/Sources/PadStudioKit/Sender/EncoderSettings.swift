import Foundation

/// Configuração efetiva do emissor (PROTOCOL.md §5.3). O PC altera-a com `HELLO.config`,
/// `CONFIGURE` e `SET_BITRATE`; a extensão aplica-a ao encoder.
public struct EncoderSettings: Equatable, Sendable {
    public var codec: VideoCodec = .h264
    public var maxFps = 120
    public var minFps = 10
    public var bitrateBps = 50_000_000
    public var bitrateMode: BitrateMode = .vbr
    /// Caixa máxima da imagem codificada (0 = nativo). Aplica-se sem olhar à orientação: o lado
    /// maior da imagem cabe no maior dos dois valores e o lado menor no menor.
    public var maxWidth = 1920
    public var maxHeight = 1440
    /// 0 = keyframes só a pedido (recomendado em USB).
    public var keyframeIntervalS: Double = 0
    public var audioApp = true
    public var audioMic = false

    public init() {}

    /// Perfil por omissão por USB (PROTOCOL.md §1).
    public static let usb = EncoderSettings()

    /// Perfil por omissão na rede local.
    public static var lan: EncoderSettings {
        var s = EncoderSettings()
        s.codec = .hevc
        s.maxFps = 60
        s.bitrateBps = 20_000_000
        return s
    }

    /// Limites do dispositivo e do protocolo.
    public struct Limits: Equatable, Sendable {
        public var deviceMaxFps = 120
        public var codecs: [VideoCodec] = [.h264, .hevc]
        public var minBitrate = 1_000_000
        public var maxBitrate = 100_000_000
        public var minDimension = 320

        public init() {}
    }

    /// Aplica os campos definidos em `config`, dentro dos limites. Devolve notas (PT-PT) sobre o
    /// que foi ajustado, para `CONFIGURE_ACK.message`.
    @discardableResult
    public mutating func apply(_ config: StreamConfig, limits: Limits = Limits()) -> [String] {
        var notes: [String] = []
        if let c = config.codec {
            if limits.codecs.contains(c) {
                codec = c
            } else {
                notes.append("\(c.name) não suportado neste iPad; mantém-se \(codec.name)")
            }
        }
        if let f = config.maxFps {
            maxFps = clamp(f, 1, limits.deviceMaxFps, "max_fps", &notes)
        }
        if let f = config.minFps {
            minFps = clamp(f, 0, 30, "min_fps", &notes)
        }
        if minFps > maxFps { minFps = maxFps }
        if let b = config.bitrateBps {
            bitrateBps = clamp(b, limits.minBitrate, limits.maxBitrate, "bitrate_bps", &notes)
        }
        if let m = config.bitrateMode {
            if m == .unknown {
                notes.append("bitrate_mode desconhecido; mantém-se \(bitrateMode.rawValue)")
            } else {
                bitrateMode = m
            }
        }
        if let w = config.maxWidth { maxWidth = w <= 0 ? 0 : clamp(w, limits.minDimension, 8192, "max_width", &notes) }
        if let h = config.maxHeight { maxHeight = h <= 0 ? 0 : clamp(h, limits.minDimension, 8192, "max_height", &notes) }
        if let k = config.keyframeIntervalS {
            keyframeIntervalS = k.isFinite ? min(max(k, 0), 60) : 0
        }
        if let a = config.audioApp { audioApp = a }
        if let a = config.audioMic { audioMic = a }
        return notes
    }

    private func clamp(_ v: Int, _ lo: Int, _ hi: Int, _ name: String, _ notes: inout [String]) -> Int {
        let c = min(max(v, lo), hi)
        if c != v { notes.append("\(name) ajustado a \(c)") }
        return c
    }

    /// Configuração completa, para `HELLO_ACK.config` e `CONFIGURE_ACK.config`.
    public var streamConfig: StreamConfig {
        StreamConfig(
            codec: codec, maxFps: maxFps, minFps: minFps, bitrateBps: bitrateBps, bitrateMode: bitrateMode,
            maxWidth: maxWidth, maxHeight: maxHeight, keyframeIntervalS: keyframeIntervalS,
            audioApp: audioApp, audioMic: audioMic
        )
    }

    /// Mudanças que obrigam a recriar a sessão VT (o resto aplica-se em direto).
    public func needsNewSession(comparedTo old: EncoderSettings) -> Bool {
        codec != old.codec || maxFps != old.maxFps || maxWidth != old.maxWidth || maxHeight != old.maxHeight
            || bitrateMode != old.bitrateMode
    }

    /// Resolução de saída para uma imagem `width`×`height`: mantém o aspeto, cabe na caixa máxima
    /// (sem olhar à orientação), nunca aumenta e tem dimensões pares.
    public func outputSize(width: Int, height: Int) -> (width: Int, height: Int) {
        guard width > 0, height > 0 else { return (0, 0) }
        var scale = 1.0
        if maxWidth > 0, maxHeight > 0 {
            let (boxLong, boxShort) = (Double(max(maxWidth, maxHeight)), Double(min(maxWidth, maxHeight)))
            let (long, short) = (Double(max(width, height)), Double(min(width, height)))
            scale = min(1, boxLong / long, boxShort / short)
        } else if maxWidth > 0 || maxHeight > 0 {
            // Só um limite: aplica-se ao lado maior.
            scale = min(1, Double(max(maxWidth, maxHeight)) / Double(max(width, height)))
        }
        let even = { (v: Double) in max(2, Int(v.rounded(.down)) & ~1) }
        return scale >= 1 ? (width & ~1, height & ~1) : (even(Double(width) * scale), even(Double(height) * scale))
    }
}

public enum H264Level {
    /// Level 5.2: 2 073 600 macroblocos/s e 36 864 macroblocos por imagem. Acima disto muitos
    /// descodificadores de PC recusam o fluxo (ARCHITECTURE.md, R3).
    public static func fitsLevel52(width: Int, height: Int, fps: Int) -> Bool {
        let mbs = ((width + 15) / 16) * ((height + 15) / 16)
        return mbs <= 36864 && mbs * fps <= 2_073_600
    }
}
