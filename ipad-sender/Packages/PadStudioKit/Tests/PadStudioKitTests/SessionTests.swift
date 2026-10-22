import Foundation
import XCTest
@testable import PadStudioKit

/// O lado PC de uma ligação de teste.
final class TestPeer {
    let stream: ByteStream
    private var parser = StreamParser()
    private var seq: UInt32 = 0

    init(_ stream: ByteStream) { self.stream = stream }

    func send(_ msg: Message) {
        try? stream.write([msg.encoded()])
    }

    func hello(transport: Transport = .usb, token: String? = nil, protocol p: Int = 1, config: StreamConfig? = nil) {
        send(.json(.hello, Hello(protocol: p, client: "teste", transport: transport, token: token, config: config)))
    }

    func command(_ type: MessageType, payload: Data = Data()) -> UInt32 {
        seq += 1
        send(Message(type: type, seq: seq, payload: payload))
        return seq
    }

    /// Próxima mensagem (de qualquer tipo, exceto `SENDER_STATS` se `skipStats`).
    func receive(timeout: TimeInterval = 2, skipStats: Bool = true) -> Message? {
        let deadline = Date(timeIntervalSinceNow: timeout)
        while Date() < deadline {
            if let m = try? parser.next() {
                if skipStats, m.kind == .senderStats { continue }
                return m
            }
            switch try? stream.read(maxLength: 1 << 20) {
            case let .data(d)?: parser.push(d)
            case .eof?, nil: return try? parser.next()
            case .timeout?: break
            }
        }
        return nil
    }

    /// Próxima mensagem do tipo pedido (ignora as outras).
    func receive(_ type: MessageType, timeout: TimeInterval = 2) -> Message? {
        let deadline = Date(timeIntervalSinceNow: timeout)
        while Date() < deadline {
            guard let m = receive(timeout: deadline.timeIntervalSinceNow, skipStats: type != .senderStats) else { return nil }
            if m.kind == type { return m }
        }
        return nil
    }

    func bye(timeout: TimeInterval = 2) -> Bye? {
        try? receive(.bye, timeout: timeout)?.decodeJSON(Bye.self)
    }
}

final class Recorder: SenderDelegate, @unchecked Sendable {
    private let lock = NSLock()
    private var _changes: [EncoderSettings] = []
    var changes: [EncoderSettings] {
        lock.lock()
        defer { lock.unlock() }
        return _changes
    }

    func sender(_: Sender, didChangeSettings settings: EncoderSettings, from _: EncoderSettings) {
        lock.lock()
        _changes.append(settings)
        lock.unlock()
    }
}

final class SessionTests: XCTestCase {
    var sender: Sender!
    var recorder = Recorder()
    var threads: [Thread] = []

    func makeSender(transport: Transport = .usb, token: String? = nil, stats: Double = 60) -> Sender {
        var o = Sender.Options()
        o.transport = transport
        o.token = token
        o.helloTimeoutUs = 300_000
        o.idleTimeoutUs = 600_000
        o.statsIntervalS = stats
        o.device = DeviceInfo(model: "iPad8,1", system: "iPadOS 26.6.2", screenWidth: 2388, screenHeight: 1668, maxFps: 120)
        let s = Sender(options: o, settings: transport == .lan ? .lan : .usb)
        s.delegate = recorder
        sender = s
        return s
    }

    /// Liga um PC de teste por tubo em memória; `serve` corre numa thread.
    func connect(_ s: Sender) -> TestPeer {
        let (server, client) = PipeStream.pair()
        let t = Thread { s.serve(server, peer: "pc-\(UUID().uuidString.prefix(4))") }
        t.start()
        threads.append(t)
        return TestPeer(client)
    }

    override func tearDown() {
        sender?.stop()
        sender = nil
    }

    func frame(key: Bool, pts: UInt64 = HostClock.system.nowUs(), configId: UInt64 = 1, size: Int = 1000, rotation: Rotation = .none) -> EncodedFrame {
        let nal = Data([key ? 0x65 : 0x41]) + Data(repeating: 0xab, count: size)
        let packet = FramePacket.fromNALs([nal], codec: .h264, paramSets: key ? [Data([0x67, 0x64]), Data([0x68, 0xee])] : nil)
        let cfg = VideoConfig(codec: .h264, fpsMax: 120, width: 1920, height: 1340, bitrateBps: 50_000_000,
                              paramSets: NAL.join([Data([0x67, 0x64]), Data([0x68, 0xee])]))
        return EncodedFrame(packet: packet, keyframe: key, ptsUs: pts, rotation: rotation, configId: configId, config: cfg)
    }

    func waitForClient(_ s: Sender) {
        let deadline = Date(timeIntervalSinceNow: 2)
        while !s.hasClient, Date() < deadline { Thread.sleep(forTimeInterval: 0.005) }
        XCTAssertTrue(s.hasClient)
    }

    // ── Handshake ───────────────────────────────────────────────────────────

    func testHelloAckWithEffectiveConfig() throws {
        let s = makeSender()
        let pc = connect(s)
        pc.hello(config: StreamConfig(maxFps: 60, bitrateBps: 30_000_000))
        let m = try XCTUnwrap(pc.receive())
        XCTAssertEqual(m.kind, .helloAck)
        let ack = try m.decodeJSON(HelloAck.self)
        XCTAssertEqual(ack.protocol, 1)
        XCTAssertEqual(ack.clock, "host_us")
        XCTAssertEqual(ack.codecs, [.h264, .hevc])
        XCTAssertEqual(ack.device?.model, "iPad8,1")
        XCTAssertEqual(ack.config?.maxFps, 60)
        XCTAssertEqual(ack.config?.bitrateBps, 30_000_000)
        XCTAssertEqual(ack.config?.codec, .h264)
        XCTAssertEqual(recorder.changes.last?.maxFps, 60)
        XCTAssertTrue(s.keyframes.isPending, "uma ligação nova pede logo um keyframe")
    }

    func testNothingIsSentBeforeHelloAndTimeoutClosesWithBye() {
        let s = makeSender()
        let pc = connect(s)
        let bye = pc.bye(timeout: 2)
        XCTAssertEqual(bye?.reason, .timeout)
        XCTAssertFalse(s.hasClient)
    }

    func testOldProtocolIsRejected() {
        let s = makeSender()
        let pc = connect(s)
        pc.hello(protocol: 0)
        XCTAssertEqual(pc.bye()?.reason, .protocolVersion)
    }

    func testLanPinAndLockout() {
        let s = makeSender(transport: .lan, token: "482913")
        for _ in 0 ..< 3 {
            let pc = connect(s)
            pc.hello(transport: .lan, token: "000000")
            XCTAssertEqual(pc.bye()?.reason, .unauthorized)
        }
        // Bloqueado durante 30 s, mesmo com o PIN certo.
        let locked = connect(s)
        locked.hello(transport: .lan, token: "482913")
        let bye = locked.bye()
        XCTAssertEqual(bye?.reason, .unauthorized)
        XCTAssertTrue(bye?.message?.contains("30 s") ?? false)
    }

    func testCorrectPinIsAccepted() {
        let s = makeSender(transport: .lan, token: "482913")
        let pc = connect(s)
        pc.hello(transport: .lan, token: "482913")
        XCTAssertEqual(pc.receive()?.kind, .helloAck)
    }

    func testNewestConnectionWins() {
        let s = makeSender()
        let first = connect(s)
        first.hello()
        XCTAssertEqual(first.receive()?.kind, .helloAck)
        let second = connect(s)
        second.hello()
        XCTAssertEqual(second.receive()?.kind, .helloAck)
        XCTAssertEqual(first.bye()?.reason, .replaced)
    }

    // ── Comandos ────────────────────────────────────────────────────────────

    func testPingPong() throws {
        let s = makeSender()
        let pc = connect(s)
        pc.hello()
        _ = pc.receive()
        let before = HostClock.system.nowUs()
        _ = pc.command(.ping, payload: Ping(t0: 123_456).encode())
        let pong = try Pong.decode(XCTUnwrap(pc.receive(.pong)).payload)
        XCTAssertEqual(pong.t0, 123_456)
        XCTAssertGreaterThanOrEqual(pong.t1, before)
        XCTAssertGreaterThanOrEqual(pong.t2, pong.t1)
    }

    func testConfigureAckAndSetBitrate() throws {
        let s = makeSender()
        let pc = connect(s)
        pc.hello()
        _ = pc.receive()
        let cfg = JSONCoding.encode(StreamConfig(codec: .hevc, maxFps: 500))
        let seq = pc.command(.configure, payload: cfg)
        let ack = try XCTUnwrap(pc.receive(.configureAck)).decodeJSON(ConfigureAck.self)
        XCTAssertEqual(ack.requestSeq, seq)
        XCTAssertTrue(ack.applied)
        XCTAssertEqual(ack.config?.codec, .hevc)
        XCTAssertEqual(ack.config?.maxFps, 120)
        XCTAssertEqual(ack.message, "max_fps ajustado a 120")
        XCTAssertEqual(s.settings.codec, .hevc)

        _ = pc.command(.setBitrate, payload: SetBitrate(bitrateBps: 35_000_000).encode())
        let deadline = Date(timeIntervalSinceNow: 2)
        while s.settings.bitrateBps != 35_000_000, Date() < deadline { Thread.sleep(forTimeInterval: 0.01) }
        XCTAssertEqual(s.settings.bitrateBps, 35_000_000)
        XCTAssertEqual(recorder.changes.last?.bitrateBps, 35_000_000)
    }

    func testBadPayloadGivesNonFatalErrorAndUnknownTypesAreIgnored() throws {
        let s = makeSender()
        let pc = connect(s)
        pc.hello()
        _ = pc.receive()
        pc.send(Message(header: Header(type: 0x9a), payload: Data([1, 2, 3])))
        let seq = pc.command(.ping, payload: Data([1, 2])) // PING curto
        let err = try XCTUnwrap(pc.receive(.error)).decodeJSON(ErrorMessage.self)
        XCTAssertEqual(err.code, "bad_payload")
        XCTAssertEqual(err.fatal, false)
        XCTAssertEqual(err.refType, 0x03)
        XCTAssertEqual(err.refSeq, seq)
        _ = pc.command(.ping, payload: Ping(t0: 7).encode())
        XCTAssertNotNil(pc.receive(.pong), "a ligação continua")
    }

    func testIdleTimeout() {
        let s = makeSender()
        let pc = connect(s)
        pc.hello()
        _ = pc.receive()
        XCTAssertEqual(pc.bye(timeout: 3)?.reason, .timeout)
    }

    func testStopSendsUserStopped() {
        let s = makeSender()
        let pc = connect(s)
        pc.hello()
        _ = pc.receive()
        waitForClient(s)
        s.stop()
        XCTAssertEqual(pc.bye()?.reason, .userStopped)
    }

    func testBroadcastStateIsForwarded() throws {
        let s = makeSender()
        let pc = connect(s)
        pc.hello()
        _ = pc.receive()
        waitForClient(s)
        s.setBroadcastState(.paused)
        let st = try XCTUnwrap(pc.receive(.senderState)).decodeJSON(SenderState.self)
        XCTAssertEqual(st.state, .paused)
    }

    // ── Vídeo ───────────────────────────────────────────────────────────────

    func testVideoStartsOnKeyframeWithConfigFirst() throws {
        let s = makeSender()
        let pc = connect(s)
        pc.hello()
        _ = pc.receive()
        waitForClient(s)
        _ = s.keyframes.shouldForce(nowUs: HostClock.system.nowUs()) // consome o pedido da ligação

        s.submitVideo(frame(key: false)) // antes do primeiro keyframe: não sai e pede keyframe
        XCTAssertTrue(s.keyframes.isPending)
        let now = HostClock.system.nowUs()
        s.submitVideo(frame(key: true, pts: now - 5000, rotation: .clockwise90))
        s.submitVideo(frame(key: false))
        s.submitVideo(frame(key: true)) // mesma configuração: sem VIDEO_CONFIG

        let cfg = try XCTUnwrap(pc.receive())
        XCTAssertEqual(cfg.kind, .videoConfig)
        XCTAssertEqual(try VideoConfig.decode(cfg.payload).width, 1920)
        let k = try XCTUnwrap(pc.receive())
        XCTAssertEqual(k.kind, .videoFrame)
        XCTAssertEqual(k.header.seq, 0)
        XCTAssertEqual(VideoFlags(rawValue: k.header.flags), VideoFlags(keyframe: true, paramsInband: true, rotation: .clockwise90))
        XCTAssertGreaterThanOrEqual(k.header.aux, 5000, "latência no emissor = fila − PTS")
        XCTAssertLessThan(k.header.aux, 500_000)
        XCTAssertEqual(NAL.split(annexB: k.payload).map { NAL.type(.h264, firstByte: $0.first!) }, [7, 8, 5])
        let p = try XCTUnwrap(pc.receive())
        XCTAssertEqual(p.header.seq, 1)
        XCTAssertEqual(p.header.flags, 0)
        let k2 = try XCTUnwrap(pc.receive())
        XCTAssertEqual(k2.kind, .videoFrame)
        XCTAssertEqual(k2.header.seq, 2)

        // Configuração nova (ex.: sessão VT recriada): VIDEO_CONFIG antes do keyframe seguinte.
        s.submitVideo(frame(key: true, configId: 2))
        XCTAssertEqual(pc.receive()?.kind, .videoConfig)
        XCTAssertEqual(pc.receive()?.kind, .videoFrame)
    }

    func testDropUntilKeyframeWithDiscontinuity() throws {
        let s = makeSender()
        let pc = connect(s)
        pc.hello()
        _ = pc.receive()
        waitForClient(s)
        s.submitVideo(frame(key: true))
        _ = pc.receive(.videoFrame)
        _ = s.keyframes.shouldForce(nowUs: HostClock.system.nowUs())

        s.simulateDrop()
        s.submitVideo(frame(key: false)) // seq 1: descartado
        s.submitVideo(frame(key: false)) // seq 2: descartado
        XCTAssertTrue(s.keyframes.isPending)
        s.submitVideo(frame(key: true)) // seq 3
        let k = try XCTUnwrap(pc.receive(.videoFrame))
        XCTAssertEqual(k.header.seq, 3, "o salto em seq mostra os frames perdidos")
        XCTAssertTrue(VideoFlags(rawValue: k.header.flags).discontinuity)
        XCTAssertEqual(s.collectStats(elapsedS: 1).framesDropped, 2)
    }

    func testQueueOverflowFlushesAndRecoversWithKeyframe() throws {
        // Frames grandes a pressionar a fila de 8 MiB (a política em si é testada em SendQueueTests).
        let s = makeSender()
        let pc = connect(s)
        pc.hello()
        _ = pc.receive()
        waitForClient(s)
        s.submitVideo(frame(key: true, size: 7 * 1024 * 1024))
        s.submitVideo(frame(key: false, size: 2 * 1024 * 1024))
        // Conforme a velocidade do escritor, o segundo frame entra ou provoca um descarte; em ambos os
        // casos o PC só vê frames inteiros, por ordem, e um keyframe recupera.
        s.submitVideo(frame(key: true))
        var frames: [Message] = []
        while let m = pc.receive(.videoFrame, timeout: 1) { frames.append(m) }
        XCTAssertFalse(frames.isEmpty)
        XCTAssertEqual(frames.map(\.header.seq), frames.map(\.header.seq).sorted())
        XCTAssertTrue(VideoFlags(rawValue: frames.last!.header.flags).keyframe)
    }

    func testPeriodicStatsWithSources() throws {
        final class Fixed: StatsSource {
            func contribute(to stats: inout SenderStats) {
                stats.thermal = .fair
                stats.withDiag { $0.cpuPercent = 12.5 }
            }
        }
        let s = makeSender(stats: 0.1)
        let src = Fixed()
        s.addStatsSource(src)
        s.start()
        let pc = connect(s)
        pc.hello()
        _ = pc.receive()
        waitForClient(s)
        s.submitVideo(frame(key: true))
        let stats = try XCTUnwrap(pc.receive(.senderStats)).decodeJSON(SenderStats.self)
        XCTAssertEqual(stats.thermal, .fair)
        XCTAssertEqual(stats.diag?.cpuPercent, 12.5)
        XCTAssertEqual(stats.targetBitrateBps, 50_000_000)
        XCTAssertNotNil(stats.framesDropped)
    }
}

final class SendQueueTests: XCTestCase {
    func testPriorityControlThenAudioThenVideo() {
        let q = SendQueue(limits: .usb, clock: ManualClock().clock)
        _ = q.pushVideo(Data([3]), droppable: true)
        q.pushAudio(Data([2]), droppable: true)
        q.pushControl(.data(Data([1])))
        var order: [UInt8] = []
        for _ in 0 ..< 3 {
            if case let .data(d)? = q.pop() { order.append(d[d.startIndex]) }
        }
        XCTAssertEqual(order, [1, 2, 3])
    }

    func testVideoFlushWhenTooBigKeepsConfigs() {
        var limits = SendQueue.Limits.usb
        limits.maxVideoBytes = 100
        let q = SendQueue(limits: limits, clock: ManualClock().clock)
        _ = q.pushVideo(Data(repeating: 0, count: 10), droppable: false) // VIDEO_CONFIG
        XCTAssertNil(q.pushVideo(head: Data(count: 24), Data(repeating: 1, count: 40), droppable: true))
        XCTAssertEqual(q.pushVideo(head: Data(count: 24), Data(repeating: 2, count: 40), droppable: true), 1)
        XCTAssertEqual(q.videoBacklog().bytes, 10, "só a configuração fica")
    }

    func testVideoFlushWhenTooOld() {
        let clock = ManualClock()
        let q = SendQueue(limits: .usb, clock: clock.clock)
        XCTAssertNil(q.pushVideo(Data([1]), droppable: true))
        clock.advance(ms: 50)
        XCTAssertNil(q.pushVideo(Data([2]), droppable: true))
        XCTAssertEqual(q.videoBacklog().ageMs, 50, accuracy: 0.001)
        clock.advance(ms: 60) // o mais antigo tem 110 ms > 100 ms (USB)
        XCTAssertEqual(q.pushVideo(Data([3]), droppable: true), 2)
        let lan = SendQueue(limits: .lan, clock: clock.clock)
        XCTAssertNil(lan.pushVideo(Data([1]), droppable: true))
        clock.advance(ms: 200)
        XCTAssertNil(lan.pushVideo(Data([2]), droppable: true), "na rede o limite é 250 ms")
    }

    func testAudioDropsOldest() {
        var limits = SendQueue.Limits.usb
        limits.maxAudioBytes = 10
        let q = SendQueue(limits: limits, clock: ManualClock().clock)
        q.pushAudio(Data([0xc0]), droppable: false) // AUDIO_CONFIG
        q.pushAudio(Data(repeating: 1, count: 4), droppable: true)
        q.pushAudio(Data(repeating: 2, count: 4), droppable: true)
        XCTAssertEqual(q.pushAudio(Data(repeating: 3, count: 4), droppable: true), 1)
        var firstBytes: [UInt8] = []
        for _ in 0 ..< 3 {
            if case let .data(d)? = q.pop() { firstBytes.append(d[d.startIndex]) }
        }
        XCTAssertEqual(firstBytes, [0xc0, 2, 3])
    }

    func testCloseUnblocksPop() {
        let q = SendQueue(limits: .usb, clock: .system)
        let done = expectation(description: "pop devolve nil")
        Thread { if q.pop() == nil { done.fulfill() } }.start()
        Thread.sleep(forTimeInterval: 0.05)
        q.close()
        wait(for: [done], timeout: 2)
    }
}

final class KeyframeGateTests: XCTestCase {
    func testCoalescesRequestsTo250ms() {
        let g = KeyframeGate()
        XCTAssertFalse(g.shouldForce(nowUs: 1_000_000))
        g.request()
        g.request()
        XCTAssertTrue(g.shouldForce(nowUs: 1_000_000))
        XCTAssertFalse(g.shouldForce(nowUs: 1_000_000))
        g.request()
        XCTAssertFalse(g.shouldForce(nowUs: 1_100_000), "menos de 250 ms depois do último")
        XCTAssertTrue(g.shouldForce(nowUs: 1_250_000))
    }

    func testPeriodicKeyframes() {
        let g = KeyframeGate()
        g.setPeriodic(seconds: 2)
        g.keyframeProduced(nowUs: 0)
        XCTAssertFalse(g.shouldForce(nowUs: 1_000_000))
        XCTAssertTrue(g.shouldForce(nowUs: 2_000_000))
        g.keyframeProduced(nowUs: 2_000_000)
        XCTAssertFalse(g.shouldForce(nowUs: 3_000_000))
    }
}

final class SocketTests: XCTestCase {
    func testHandshakeAndLargeFrameOverRealTCP() throws {
        var o = Sender.Options()
        o.statsIntervalS = 60
        let sender = Sender(options: o, settings: .usb)
        let server = try TCPServer(interface: .loopback, port: 0)
        defer {
            sender.stop()
            server.stop()
        }
        XCTAssertNotEqual(server.port, 0)
        server.start { stream in sender.serve(stream, peer: stream.peer) }

        let pc = TestPeer(try Socket.connectLoopback(port: server.port))
        pc.hello()
        XCTAssertEqual(pc.receive(timeout: 3)?.kind, .helloAck)
        let deadline = Date(timeIntervalSinceNow: 2)
        while !sender.hasClient, Date() < deadline { Thread.sleep(forTimeInterval: 0.005) }

        // Keyframe de 3 MB: tem de chegar inteiro e igual.
        var big = Data([0x65])
        big.append(Data((0 ..< 3_000_000).map { UInt8(truncatingIfNeeded: $0 &* 31) }))
        let packet = FramePacket.fromNALs([big], codec: .h264, paramSets: [Data([0x67, 1]), Data([0x68, 2])])
        let cfg = VideoConfig(codec: .h264, fpsMax: 120, width: 16, height: 16, bitrateBps: 1, paramSets: Data())
        sender.submitVideo(EncodedFrame(packet: packet, keyframe: true, ptsUs: HostClock.system.nowUs(), rotation: .none, configId: 1, config: cfg))
        XCTAssertEqual(pc.receive(timeout: 3)?.kind, .videoConfig)
        let f = try XCTUnwrap(pc.receive(timeout: 5))
        XCTAssertEqual(f.kind, .videoFrame)
        XCTAssertEqual(f.payload, packet.payload)

        _ = pc.command(.ping, payload: Ping(t0: 1).encode())
        XCTAssertNotNil(pc.receive(.pong, timeout: 3))
        pc.stream.close()
    }

    func testPortInUseIsReported() throws {
        // Ex.: uma extensão anterior ainda a escutar. SO_REUSEADDR só tolera TIME_WAIT, não uma porta ativa.
        let a = try TCPServer(interface: .loopback, port: 0)
        defer { a.stop() }
        XCTAssertThrowsError(try TCPServer(interface: .loopback, port: a.port)) { error in
            XCTAssertTrue("\(error)".contains("em uso"), "\(error)")
        }
    }
}
