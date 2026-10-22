import Foundation
import XCTest
@testable import PadStudioKit

/// O codec Swift contra os vetores partilhados (os mesmos que o Rust e a referência JS usam):
/// os encoders binários têm de produzir exatamente os mesmos bytes; no JSON compara-se o significado.
final class VectorsTests: XCTestCase {
    func testDocumentMatchesConstants() {
        XCTAssertEqual(Vectors.document["protocol"] as? String, "PSWP")
        XCTAssertEqual(uint(Vectors.document["version"]), UInt64(PSWP.version))
        XCTAssertEqual(uint(Vectors.document["header_len"]), UInt64(PSWP.headerLength))
        XCTAssertEqual(uint(Vectors.document["max_payload"]), UInt64(PSWP.maxPayload))
    }

    func testEveryMessageParsesAndReencodesExactly() throws {
        for v in Vectors.messages {
            let name = v["name"] as! String
            let bytes = Data(hex: v["hex"] as! String)
            var parser = StreamParser()
            parser.push(bytes)
            let msg = try XCTUnwrap(try parser.next(), name)
            XCTAssertNil(try parser.next(), name)
            XCTAssertEqual(parser.pending, 0, name)

            let h = v["header"] as! [String: Any]
            XCTAssertEqual(UInt64(msg.header.type), uint(h["type"]), name)
            XCTAssertEqual(UInt64(msg.header.flags), uint(h["flags"]), name)
            XCTAssertEqual(UInt64(msg.header.payloadLength), uint(h["payload_len"]), name)
            XCTAssertEqual(UInt64(msg.header.seq), uint(h["seq"]), name)
            XCTAssertEqual(UInt64(msg.header.aux), uint(h["aux"]), name)
            XCTAssertEqual(msg.header.ptsUs, uint(h["pts_us"]), name)
            XCTAssertEqual(msg.payload, Data(hex: v["payload_hex"] as! String), name)
            XCTAssertEqual(msg.encoded(), bytes, "\(name): reencode")
            if let typeName = h["type_name"] as? String {
                XCTAssertEqual(msg.kind?.name, typeName, name)
            } else {
                XCTAssertNil(msg.kind, name)
            }
        }
    }

    func testUnknownTypeIsReportedAsUnknown() throws {
        let v = Vectors.message("unknown_type_0x9a")
        var parser = StreamParser()
        parser.push(Data(hex: v["hex"] as! String))
        let msg = try XCTUnwrap(try parser.next())
        XCTAssertNil(msg.kind)
        XCTAssertEqual(msg.header.type, 0x9a)
    }

    func testJSONPayloadsRoundTrip() throws {
        for v in Vectors.messages where v["payload_kind"] as? String == "json" {
            let name = v["name"] as! String
            let payload = Data(hex: v["payload_hex"] as! String)
            let msg = Message(header: Header(type: UInt8(uint((v["header"] as! [String: Any])["type"]))), payload: payload)
            let reencoded: Data
            switch msg.kind! {
            case .hello: reencoded = JSONCoding.encode(try msg.decodeJSON(Hello.self))
            case .helloAck: reencoded = JSONCoding.encode(try msg.decodeJSON(HelloAck.self))
            case .bye: reencoded = JSONCoding.encode(try msg.decodeJSON(Bye.self))
            case .error: reencoded = JSONCoding.encode(try msg.decodeJSON(ErrorMessage.self))
            case .senderState: reencoded = JSONCoding.encode(try msg.decodeJSON(SenderState.self))
            case .senderStats: reencoded = JSONCoding.encode(try msg.decodeJSON(SenderStats.self))
            case .receiverReport: reencoded = JSONCoding.encode(try msg.decodeJSON(ReceiverReport.self))
            case .configure: reencoded = JSONCoding.encode(try msg.decodeJSON(StreamConfig.self))
            case .configureAck: reencoded = JSONCoding.encode(try msg.decodeJSON(ConfigureAck.self))
            default:
                XCTFail("\(name): tipo JSON sem teste")
                continue
            }
            XCTAssertTrue(jsonEqual(jsonObject(reencoded), v["payload_json"]!), "\(name): \(String(decoding: reencoded, as: UTF8.self))")
        }
    }

    func testTypedJSONValues() throws {
        let hello = try Message(header: Header(type: 1), payload: Data(hex: Vectors.message("hello_lan")["payload_hex"] as! String))
            .decodeJSON(Hello.self)
        XCTAssertEqual(hello.transport, .lan)
        XCTAssertEqual(hello.token, "482913")

        let ack = try Message(header: Header(type: 2), payload: Data(hex: Vectors.message("hello_ack")["payload_hex"] as! String))
            .decodeJSON(HelloAck.self)
        XCTAssertEqual(ack.codecs, [.h264, .hevc])
        XCTAssertEqual(ack.config?.bitrateMode, .vbr)
        XCTAssertEqual(ack.device?.screenWidth, 2752)

        let stats = try Message(header: Header(type: 0x20), payload: Data(hex: Vectors.message("sender_stats")["payload_hex"] as! String))
            .decodeJSON(SenderStats.self)
        XCTAssertEqual(stats.thermal, .nominal)
        XCTAssertEqual(stats.framesDropped, 3)
        XCTAssertNil(stats.diag)
    }

    func testUnknownEnumValuesAndFieldsAreTolerated() throws {
        let json = #"{"reason":"cosmic_rays","message":"x","extra":{"a":1}}"#
        let bye = try Message(type: .bye, payload: Data(json.utf8)).decodeJSON(Bye.self)
        XCTAssertEqual(bye.reason, .unknown)
        let state = try Message(type: .senderState, payload: Data(#"{"state":"sleeping"}"#.utf8)).decodeJSON(SenderState.self)
        XCTAssertEqual(state.state, .unknown)
    }

    func testBinaryPayloadFields() throws {
        let ping = try Ping.decode(Data(hex: Vectors.message("ping")["payload_hex"] as! String))
        XCTAssertEqual(ping.t0, 5_000_123)
        XCTAssertEqual(ping.encode().hex, Vectors.message("ping")["payload_hex"] as! String)

        let pong = try Pong.decode(Data(hex: Vectors.message("pong")["payload_hex"] as! String))
        XCTAssertEqual(pong, Pong(t0: 5_000_123, t1: 86_400_000_456, t2: 86_400_000_501))
        XCTAssertEqual(pong.encode().hex, Vectors.message("pong")["payload_hex"] as! String)

        for name in ["video_config_h264", "video_config_hevc"] {
            let v = Vectors.message(name)
            let f = v["fields"] as! [String: Any]
            let c = try VideoConfig.decode(Data(hex: v["payload_hex"] as! String))
            XCTAssertEqual(UInt64(c.codec.rawValue), uint(f["codec"]), name)
            XCTAssertEqual(UInt64(c.bitDepth), uint(f["bit_depth"]), name)
            XCTAssertEqual(UInt64(c.fpsMax), uint(f["fps_max"]), name)
            XCTAssertEqual(UInt64(c.width), uint(f["width"]), name)
            XCTAssertEqual(UInt64(c.height), uint(f["height"]), name)
            XCTAssertEqual(UInt64(c.bitrateBps), uint(f["bitrate_bps"]), name)
            XCTAssertEqual(UInt64(c.colorPrimaries), uint(f["color_primaries"]), name)
            XCTAssertEqual(UInt64(c.transfer), uint(f["transfer"]), name)
            XCTAssertEqual(UInt64(c.matrix), uint(f["matrix"]), name)
            XCTAssertEqual(c.fullRange, f["full_range"] as? Bool, name)
            XCTAssertEqual(c.paramSets.hex, f["param_sets_hex"] as! String, name)
            let types = NAL.split(annexB: c.paramSets).map { UInt64(NAL.type(c.codec, firstByte: $0.first!)) }
            XCTAssertEqual(types, (f["nal_types"] as! [Any]).map { uint($0) }, name)
            XCTAssertEqual(c.encode().hex, v["payload_hex"] as! String, name)
        }

        for name in ["audio_config_app", "audio_config_mic"] {
            let v = Vectors.message(name)
            let f = v["fields"] as! [String: Any]
            let c = try AudioConfig.decode(Data(hex: v["payload_hex"] as! String))
            XCTAssertEqual(UInt64(c.source), uint(f["source"]), name)
            XCTAssertEqual(UInt64(c.format), uint(f["format"]), name)
            XCTAssertEqual(UInt64(c.channels), uint(f["channels"]), name)
            XCTAssertEqual(UInt64(c.sampleRate), uint(f["sample_rate"]), name)
            XCTAssertEqual(c.encode().hex, v["payload_hex"] as! String, name)
        }

        let sb = try SetBitrate.decode(Data(hex: Vectors.message("set_bitrate")["payload_hex"] as! String))
        XCTAssertEqual(sb.bitrateBps, 35_000_000)
        XCTAssertEqual(sb.encode().hex, Vectors.message("set_bitrate")["payload_hex"] as! String)
    }

    func testFlagsAndNALTypes() {
        for v in Vectors.messages {
            guard let decoded = v["flags_decoded"] as? [String: Any] else { continue }
            let name = v["name"] as! String
            let raw = UInt8(uint((v["header"] as! [String: Any])["flags"]))
            if decoded["keyframe"] != nil {
                let f = VideoFlags(rawValue: raw)
                XCTAssertEqual(f.keyframe, decoded["keyframe"] as? Bool, name)
                XCTAssertEqual(f.paramsInband, decoded["params_inband"] as? Bool, name)
                XCTAssertEqual(f.discontinuity, decoded["discontinuity"] as? Bool, name)
                XCTAssertEqual(UInt64(f.rotation.rawValue), uint(decoded["rotation"]), name)
                XCTAssertEqual(f.rawValue, raw, name)
                let codec = VideoCodec(rawValue: UInt8(uint(v["context_codec"])))!
                let payload = Data(hex: v["payload_hex"] as! String)
                let types = NAL.split(annexB: payload).map { UInt64(NAL.type(codec, firstByte: $0.first!)) }
                XCTAssertEqual(types, (v["nal_types"] as! [Any]).map { uint($0) }, name)
            } else {
                let f = AudioFlags(rawValue: raw)
                XCTAssertEqual(UInt64(f.source), uint(decoded["source"]), name)
                XCTAssertEqual(f.discontinuity, decoded["discontinuity"] as? Bool, name)
                XCTAssertEqual(f.rawValue, raw, name)
            }
        }
    }

    func testStreamsWithArbitraryChunking() throws {
        for s in Vectors.streams {
            let name = s["name"] as! String
            let bytes = Data(hex: s["hex"] as! String)
            let expected = (s["messages"] as! [String]).map { Vectors.message($0) }
            var sizes = (s["chunk_sizes"] as! [Any]).map { Int(uint($0)) }
            sizes.append(bytes.count)
            for size in sizes {
                var parser = StreamParser()
                var out: [Message] = []
                var i = 0
                while i < bytes.count {
                    let end = min(i + size, bytes.count)
                    parser.push(bytes.subdata(in: i ..< end))
                    while let m = try parser.next() { out.append(m) }
                    i = end
                }
                XCTAssertEqual(out.count, expected.count, "\(name) em pedaços de \(size)")
                for (m, v) in zip(out, expected) {
                    XCTAssertEqual(m.encoded().hex, v["hex"] as! String, "\(name) em pedaços de \(size): \(v["name"]!)")
                }
                XCTAssertEqual(parser.pending, 0)
            }
        }
    }

    func testInvalidInputs() {
        for v in Vectors.invalid {
            let name = v["name"] as! String
            let expected = v["error"] as! String
            var parser = StreamParser()
            parser.push(Data(hex: v["hex"] as! String))
            do {
                guard let msg = try parser.next() else {
                    XCTFail("\(name): devia falhar com \(expected)")
                    continue
                }
                XCTAssertEqual(v["stage"] as? String, "payload", name)
                switch msg.kind {
                case .videoConfig: _ = try VideoConfig.decode(msg.payload)
                case .pong: _ = try Pong.decode(msg.payload)
                default: XCTFail("\(name): tipo sem teste")
                }
                XCTFail("\(name): devia falhar com \(expected)")
            } catch let e as ProtocolError {
                XCTAssertEqual(e.code, expected, name)
            } catch {
                XCTFail("\(name): erro inesperado \(error)")
            }
        }
    }
}
