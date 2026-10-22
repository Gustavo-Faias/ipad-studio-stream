import Foundation

/// Utilitários de NAL units (H.264 / HEVC).
public enum NAL {
    public static let startCode: [UInt8] = [0, 0, 0, 1]

    /// Tipo da NAL: H.264 → 5 bits baixos; HEVC → bits 1…6 do primeiro byte.
    public static func type(_ codec: VideoCodec, firstByte: UInt8) -> UInt8 {
        codec == .hevc ? (firstByte >> 1) & 0x3f : firstByte & 0x1f
    }

    /// SPS/PPS (H.264) ou VPS/SPS/PPS (HEVC).
    public static func isParameterSet(_ codec: VideoCodec, _ t: UInt8) -> Bool {
        codec == .hevc ? (32 ... 34).contains(t) : t == 7 || t == 8
    }

    /// Access unit delimiter (o protocolo envia AUs sem AUD).
    public static func isAUD(_ codec: VideoCodec, _ t: UInt8) -> Bool {
        codec == .hevc ? t == 35 : t == 9
    }

    /// NAL com dados de imagem (slice).
    public static func isVCL(_ codec: VideoCodec, _ t: UInt8) -> Bool {
        codec == .hevc ? t <= 31 : (1 ... 5).contains(t)
    }

    /// IDR (H.264) / IRAP (HEVC).
    public static func isKeyframe(_ codec: VideoCodec, _ t: UInt8) -> Bool {
        codec == .hevc ? (16 ... 23).contains(t) : t == 5
    }

    /// NAL não-VCL que, depois de uma imagem, começa o access unit seguinte.
    static func startsAccessUnit(_ codec: VideoCodec, _ t: UInt8) -> Bool {
        codec == .hevc ? (32 ... 35).contains(t) || t == 39 : [6, 7, 8, 9, 14, 15].contains(t)
    }

    /// Primeira slice da imagem: `first_mb_in_slice == 0` (H.264) ou
    /// `first_slice_segment_in_pic_flag` (HEVC). Em ue(v), o valor 0 é o bit "1".
    static func isFirstSliceOfPicture(_ codec: VideoCodec, _ nal: Data) -> Bool {
        let headerLength = codec == .hevc ? 2 : 1
        guard nal.count > headerLength else { return false }
        return nal[nal.startIndex + headerLength] & 0x80 != 0
    }

    /// Divide um buffer Anexo-B em NAL units (sem start codes).
    public static func split(annexB data: Data) -> [Data] {
        var nals: [Data] = []
        data.withUnsafeBytes { raw in
            let b = raw.bindMemory(to: UInt8.self)
            var start = -1
            var i = 0
            while i + 2 < b.count {
                if b[i] == 0, b[i + 1] == 0, b[i + 2] == 1 {
                    if start >= 0 {
                        var end = i
                        while end > start, b[end - 1] == 0 { end -= 1 } // zero do start code de 4 bytes
                        if end > start { nals.append(Data(b[start ..< end])) }
                    }
                    i += 3
                    start = i
                } else {
                    i += 1
                }
            }
            if start >= 0, start < b.count { nals.append(Data(b[start ..< b.count])) }
        }
        return nals
    }

    /// Junta NAL units em Anexo-B (start codes de 4 bytes).
    public static func join(_ nals: [Data]) -> Data {
        var out = Data(capacity: nals.reduce(0) { $0 + 4 + $1.count })
        for nal in nals {
            out.append(contentsOf: startCode)
            out.append(nal)
        }
        return out
    }
}

/// Erro ao converter a saída do encoder.
public struct MalformedBitstream: Error, CustomStringConvertible {
    public let description: String
}

/// Payload de um `VIDEO_FRAME`: um access unit completo em Anexo-B. A conversão AVCC → Anexo-B
/// acontece durante a única cópia que sai do buffer do encoder (ADR-02); o cabeçalho de 24 bytes
/// segue à parte no socket, para nunca se copiar o vídeo outra vez.
public struct FramePacket: Sendable {
    public let payload: Data

    init(payload: Data) {
        self.payload = payload
    }

    /// Converte a saída AVCC do VideoToolbox (NALs com prefixo de tamanho big-endian) em Anexo-B,
    /// durante a cópia para o buffer de envio. Num keyframe, `paramSets` (VPS/SPS/PPS) vão à frente
    /// (`PARAMS_INBAND`). AUDs e parâmetros que já venham no AU são retirados.
    public static func fromAVCC(_ avcc: UnsafeRawBufferPointer, nalLengthSize: Int, codec: VideoCodec, paramSets: [Data]?) throws -> FramePacket {
        guard (1 ... 4).contains(nalLengthSize) else {
            throw MalformedBitstream(description: "tamanho do prefixo NAL inválido: \(nalLengthSize)")
        }
        let paramBytes = paramSets?.reduce(0) { $0 + 4 + $1.count } ?? 0
        // Com prefixos de 4 bytes o tamanho é igual; com prefixos mais curtos, cresce até 3 bytes por NAL.
        let extra = nalLengthSize < 4 ? (avcc.count / (nalLengthSize + 1)) * (4 - nalLengthSize) : 0
        var out = Data(capacity: paramBytes + avcc.count + extra)
        for ps in paramSets ?? [] {
            out.append(contentsOf: NAL.startCode)
            out.append(ps)
        }
        guard let base = avcc.baseAddress?.assumingMemoryBound(to: UInt8.self) else {
            return FramePacket(payload: out)
        }
        var offset = 0
        while offset < avcc.count {
            guard offset + nalLengthSize <= avcc.count else {
                throw MalformedBitstream(description: "prefixo NAL truncado no byte \(offset)")
            }
            var length = 0
            for k in 0 ..< nalLengthSize { length = length << 8 | Int(base[offset + k]) }
            offset += nalLengthSize
            guard length > 0, offset + length <= avcc.count else {
                throw MalformedBitstream(description: "NAL com \(length) bytes excede o buffer (\(avcc.count))")
            }
            let t = NAL.type(codec, firstByte: base[offset])
            if !NAL.isAUD(codec, t), !(paramSets != nil && NAL.isParameterSet(codec, t)) {
                out.append(contentsOf: NAL.startCode)
                out.append(base + offset, count: length)
            }
            offset += length
        }
        return FramePacket(payload: out)
    }

    /// Monta o pacote a partir de NAL units (simulador): retira AUDs e parâmetros e, num keyframe,
    /// põe `paramSets` à frente.
    public static func fromNALs(_ nals: [Data], codec: VideoCodec, paramSets: [Data]?) -> FramePacket {
        var out = Data()
        for ps in paramSets ?? [] {
            out.append(contentsOf: NAL.startCode)
            out.append(ps)
        }
        for nal in nals where !nal.isEmpty {
            let t = NAL.type(codec, firstByte: nal[nal.startIndex])
            if NAL.isAUD(codec, t) || NAL.isParameterSet(codec, t) { continue }
            out.append(contentsOf: NAL.startCode)
            out.append(nal)
        }
        return FramePacket(payload: out)
    }
}

/// Um access unit de um ficheiro Anexo-B (simulador).
public struct AccessUnit: Sendable {
    public var nals: [Data]
    public var keyframe: Bool
}

/// Divide um fluxo Anexo-B (ficheiro `.h264`/`.h265`) em access units e recolhe os parâmetros.
public struct AccessUnitSplitter {
    public let codec: VideoCodec
    public private(set) var units: [AccessUnit] = []
    /// Últimos VPS/SPS/PPS vistos, por tipo de NAL.
    public private(set) var paramSets: [UInt8: Data] = [:]

    public init(codec: VideoCodec, annexB: Data) {
        self.codec = codec
        var units: [AccessUnit] = []
        var params: [UInt8: Data] = [:]
        var current: [Data] = []
        var seenVCL = false
        var keyframe = false
        for nal in NAL.split(annexB: annexB) where !nal.isEmpty {
            let t = NAL.type(codec, firstByte: nal[nal.startIndex])
            let boundary = seenVCL
                && (NAL.isVCL(codec, t) ? NAL.isFirstSliceOfPicture(codec, nal) : NAL.startsAccessUnit(codec, t))
            if boundary {
                units.append(AccessUnit(nals: current, keyframe: keyframe))
                current = []
                seenVCL = false
                keyframe = false
            }
            if NAL.isVCL(codec, t) {
                seenVCL = true
                keyframe = keyframe || NAL.isKeyframe(codec, t)
            }
            if NAL.isParameterSet(codec, t) { params[t] = nal }
            current.append(nal)
        }
        if seenVCL { units.append(AccessUnit(nals: current, keyframe: keyframe)) }
        self.units = units
        paramSets = params
    }

    /// Parâmetros pela ordem do protocolo: VPS (HEVC), SPS, PPS.
    public var orderedParamSets: [Data] {
        paramSets.keys.sorted().compactMap { paramSets[$0] }
    }
}
