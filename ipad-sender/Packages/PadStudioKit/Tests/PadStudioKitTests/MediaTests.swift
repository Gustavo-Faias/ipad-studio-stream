import Foundation
import XCTest
@testable import PadStudioKit

final class AnnexBTests: XCTestCase {
    // NALs sintéticas: SPS(7) PPS(8) IDR(5, primeira slice) e P(1).
    let sps = Data([0x67, 0x64, 0x00, 0x34, 0xac])
    let pps = Data([0x68, 0xee, 0x3c, 0xb0])
    let aud = Data([0x09, 0xf0])
    let idr = Data([0x65, 0x88, 0x84, 0x00, 0x00, 0x03, 0x01])
    let p = Data([0x41, 0x9a, 0x22, 0x10])

    func avcc(_ nals: [Data], lengthSize: Int = 4) -> Data {
        var out = Data()
        for n in nals {
            for k in (0 ..< lengthSize).reversed() { out.append(UInt8(truncatingIfNeeded: n.count >> (8 * k))) }
            out.append(n)
        }
        return out
    }

    func testAVCCToAnnexBWithParamsOnKeyframe() throws {
        // O VT pode pôr um AUD e parâmetros dentro do AU: saem, e os parâmetros vão à frente.
        let input = avcc([aud, sps, idr])
        let packet = try input.withUnsafeBytes { try FramePacket.fromAVCC($0, nalLengthSize: 4, codec: .h264, paramSets: [sps, pps]) }
        XCTAssertEqual(NAL.split(annexB: packet.payload), [sps, pps, idr])
        XCTAssertEqual(packet.payload.prefix(4), Data([0, 0, 0, 1]))
    }

    func testAVCCToAnnexBPFrameKeepsEverythingButAUD() throws {
        let input = avcc([aud, p, p])
        let packet = try input.withUnsafeBytes { try FramePacket.fromAVCC($0, nalLengthSize: 4, codec: .h264, paramSets: nil) }
        XCTAssertEqual(NAL.split(annexB: packet.payload), [p, p])
        XCTAssertEqual(packet.payload.count, input.count - (4 + aud.count))
    }

    func testShortLengthPrefixes() throws {
        for size in 1 ... 3 {
            let input = avcc([idr, p], lengthSize: size)
            let packet = try input.withUnsafeBytes { try FramePacket.fromAVCC($0, nalLengthSize: size, codec: .h264, paramSets: nil) }
            XCTAssertEqual(NAL.split(annexB: packet.payload), [idr, p], "prefixo de \(size) bytes")
        }
    }

    func testMalformedAVCCThrows() {
        var bad = avcc([idr])
        bad[3] = 200 // tamanho maior do que o buffer
        XCTAssertThrowsError(try bad.withUnsafeBytes { try FramePacket.fromAVCC($0, nalLengthSize: 4, codec: .h264, paramSets: nil) })
        let truncated = Data([0, 0])
        XCTAssertThrowsError(try truncated.withUnsafeBytes { try FramePacket.fromAVCC($0, nalLengthSize: 4, codec: .h264, paramSets: nil) })
    }

    func testSplitHandlesThreeAndFourByteStartCodes() {
        let stream = Data([0, 0, 0, 1]) + sps + Data([0, 0, 1]) + pps + Data([0, 0, 0, 1]) + idr
        XCTAssertEqual(NAL.split(annexB: stream), [sps, pps, idr])
        XCTAssertEqual(NAL.join([sps, pps]), Data([0, 0, 0, 1]) + sps + Data([0, 0, 0, 1]) + pps)
    }

    func testAccessUnitSplitterH264() {
        // AUD+SPS+PPS+IDR | P | AUD+P (duas slices) | SEI+P
        let slice2 = Data([0x41, 0x40, 0x11]) // first_mb_in_slice != 0 → mesma imagem
        let sei = Data([0x06, 0x05, 0x01])
        let stream = NAL.join([aud, sps, pps, idr, p, aud, p, slice2, sei, p])
        let s = AccessUnitSplitter(codec: .h264, annexB: stream)
        XCTAssertEqual(s.units.count, 4)
        XCTAssertEqual(s.units.map(\.keyframe), [true, false, false, false])
        XCTAssertEqual(s.units[2].nals, [aud, p, slice2])
        XCTAssertEqual(s.orderedParamSets, [sps, pps])
        // Para o fio: sem AUD nem parâmetros repetidos; parâmetros à frente no keyframe.
        let key = FramePacket.fromNALs(s.units[0].nals, codec: .h264, paramSets: s.orderedParamSets)
        XCTAssertEqual(NAL.split(annexB: key.payload), [sps, pps, idr])
    }

    func testAccessUnitSplitterHEVC() {
        let vps = Data([0x40, 0x01, 0x0c])
        let hsps = Data([0x42, 0x01, 0x01])
        let hpps = Data([0x44, 0x01, 0xc1])
        let idrW = Data([0x26, 0x01, 0xaf]) // IDR_W_RADL (19), primeira slice
        let trail = Data([0x02, 0x01, 0xd0]) // TRAIL_R (1), primeira slice
        let s = AccessUnitSplitter(codec: .hevc, annexB: NAL.join([vps, hsps, hpps, idrW, trail, trail]))
        XCTAssertEqual(s.units.map(\.keyframe), [true, false, false])
        XCTAssertEqual(s.orderedParamSets, [vps, hsps, hpps])
    }

    func testRotationFromEXIF() {
        XCTAssertEqual(Rotation(exifOrientation: 1), .none)
        XCTAssertEqual(Rotation(exifOrientation: 3), .upsideDown)
        XCTAssertEqual(Rotation(exifOrientation: 6), .clockwise90)
        XCTAssertEqual(Rotation(exifOrientation: 8), .clockwise270)
        XCTAssertEqual(Rotation(exifOrientation: 42), .none)
    }
}

final class EncoderSettingsTests: XCTestCase {
    func testOutputSizeIgnoresOrientation() {
        let s = EncoderSettings() // caixa 1920x1440
        // iPad Pro 11" (2018): o ReplayKit pode entregar o painel em retrato ou paisagem.
        XCTAssertEqual(s.outputSize(width: 2388, height: 1668).width, 1920)
        XCTAssertEqual(s.outputSize(width: 2388, height: 1668).height, 1340)
        XCTAssertEqual(s.outputSize(width: 1668, height: 2388).width, 1340)
        XCTAssertEqual(s.outputSize(width: 1668, height: 2388).height, 1920)
        // Nunca aumenta; dimensões pares.
        XCTAssertEqual(s.outputSize(width: 1281, height: 721).width, 1280)
        XCTAssertEqual(s.outputSize(width: 1281, height: 721).height, 720)
    }

    func testNativeAndSingleLimit() {
        var s = EncoderSettings()
        s.maxWidth = 0
        s.maxHeight = 0
        XCTAssertEqual(s.outputSize(width: 2388, height: 1668).width, 2388)
        s.maxWidth = 1280
        let o = s.outputSize(width: 1668, height: 2388)
        XCTAssertEqual(o.height, 1280)
        XCTAssertEqual(o.width, 894)
    }

    func testApplyClampsAndReportsNotes() {
        var s = EncoderSettings()
        var limits = EncoderSettings.Limits()
        limits.codecs = [.h264]
        let notes = s.apply(StreamConfig(codec: .hevc, maxFps: 240, minFps: 50, bitrateBps: 500_000_000, bitrateMode: .cbr), limits: limits)
        XCTAssertEqual(s.codec, .h264)
        XCTAssertEqual(s.maxFps, 120)
        XCTAssertEqual(s.minFps, 30)
        XCTAssertEqual(s.bitrateBps, 100_000_000)
        XCTAssertEqual(s.bitrateMode, .cbr)
        XCTAssertEqual(notes.count, 4, "\(notes)")
        // Campos em falta mantêm o valor.
        s.apply(StreamConfig(maxFps: 60))
        XCTAssertEqual(s.bitrateBps, 100_000_000)
        XCTAssertEqual(s.maxFps, 60)
        XCTAssertEqual(s.streamConfig.maxFps, 60)
    }

    func testNeedsNewSession() {
        let a = EncoderSettings()
        var b = a
        b.bitrateBps = 20_000_000
        XCTAssertFalse(b.needsNewSession(comparedTo: a), "o bitrate muda em direto")
        b.codec = .hevc
        XCTAssertTrue(b.needsNewSession(comparedTo: a))
    }

    func testH264Level52() {
        XCTAssertTrue(H264Level.fitsLevel52(width: 2388, height: 1668, fps: 120)) // iPad de referência
        XCTAssertTrue(H264Level.fitsLevel52(width: 1920, height: 1440, fps: 120))
        XCTAssertFalse(H264Level.fitsLevel52(width: 2732, height: 2048, fps: 120)) // iPad Pro 12,9"
    }
}

final class DiagnosticsTests: XCTestCase {
    func testHistogramBuckets() {
        var h = IntervalHistogram()
        for ms in [3.0, 8.3, 8.4, 12, 16.7, 25, 100] { h.add(ms: ms) }
        XCTAssertEqual([h.lt6, h.from6to10, h.from10to14, h.from14to20, h.from20to40, h.ge40], [1, 2, 1, 1, 1, 1])
        XCTAssertEqual(h.total, 7)
        let json = String(decoding: JSONCoding.encode(h), as: UTF8.self)
        XCTAssertTrue(json.contains("\"6to10\":2"), json)
    }

    func testCaptureMonitorAt120Hz() {
        let clock = ManualClock()
        let m = CaptureMonitor(clock: clock.clock)
        for _ in 0 ..< 121 {
            m.frameArrived(ptsUs: clock.value - 3000, width: 1668, height: 2388, pixelFormat: "420f", orientation: 6, rotation: .clockwise90)
            clock.advance(ms: 1000.0 / 120)
        }
        var s = SenderStats()
        m.contribute(to: &s)
        let c = try! XCTUnwrap(s.diag?.capture)
        XCTAssertEqual(s.fpsCapture ?? 0, 120, accuracy: 1.5)
        XCTAssertEqual(c.intervals?.from6to10, 120)
        XCTAssertEqual(c.intervalMsP50 ?? 0, 8.33, accuracy: 0.01)
        XCTAssertEqual(c.lagMsP50 ?? 0, 3, accuracy: 0.01)
        XCTAssertEqual(c.rotation, 90)
        XCTAssertEqual(c.orientation, 6)
        // Janela nova depois de recolher.
        var s2 = SenderStats()
        clock.advance(ms: 1000)
        m.contribute(to: &s2)
        XCTAssertEqual(s2.fpsCapture, 0)
        XCTAssertEqual(s2.diag?.capture?.intervals?.total, 0)
    }

    func testEncodeMonitor() {
        let m = EncodeMonitor()
        var s = SenderStats()
        m.contribute(to: &s)
        XCTAssertNil(s.diag, "sem sessão ainda não há nada a reportar")
        m.sessionCreated(codec: .h264, inputWidth: 2388, inputHeight: 1668, outputWidth: 1920, outputHeight: 1340)
        for _ in 0 ..< 4 { m.submitted() }
        for ms in [3.0, 4, 5, 6] { m.completed(ms: ms) }
        m.submitted()
        m.failed(dropped: true)
        m.contribute(to: &s)
        let e = try! XCTUnwrap(s.diag?.encode)
        XCTAssertEqual(s.encodeMsAvg, 4.5)
        XCTAssertEqual(e.inFlightMax, 4)
        XCTAssertEqual(e.encoderDrops, 1)
        XCTAssertEqual(e.scaled, true)
        XCTAssertEqual(e.msMax, 6)
    }
}
