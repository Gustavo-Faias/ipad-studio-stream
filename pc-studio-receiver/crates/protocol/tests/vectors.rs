//! Testa o crate contra `protocol/test-vectors/v1.json`, gerado pela implementação de referência
//! (`protocol/reference/gen-vectors.mjs`). O lado Swift testa contra o mesmo ficheiro.

use std::path::PathBuf;

use bytes::Bytes;
use padstudio_protocol::json::{
    Bye, Configure, ConfigureAck, ErrorMsg, Hello, HelloAck, ReceiverReport, SenderState, SenderStats,
};
use padstudio_protocol::payload::{
    AudioConfig, AudioFlags, AudioFormat, AudioSource, Codec, Ping, Pong, SetBitrate, VideoConfig, VideoFlags,
};
use padstudio_protocol::{HEADER_LEN, Header, Message, MessageType, Payload, ProtocolError, StreamParser, annexb};
use serde_json::{Value, json};

fn vectors() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../protocol/test-vectors/v1.json");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).expect("JSON dos vetores")
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or_else(|| panic!("campo {key} em falta"))
}

fn u64_of(v: &Value, key: &str) -> u64 {
    v[key].as_u64().unwrap_or_else(|| panic!("campo {key} em falta"))
}

/// Igualdade semântica de JSON: números comparam-se pelo valor (0 == 0.0).
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| json_eq(a, b)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w)))
        }
        _ => a == b,
    }
}

/// Descodifica um payload JSON para o tipo Rust e volta a serializar (deteta campos em falta).
fn json_roundtrip(kind: MessageType, payload: &Bytes) -> Result<Value, ProtocolError> {
    fn rt<P: Payload + serde::Serialize>(p: &Bytes) -> Result<Value, ProtocolError> {
        Ok(serde_json::to_value(P::decode_payload(p)?).expect("serializar"))
    }
    match kind {
        MessageType::Hello => rt::<Hello>(payload),
        MessageType::HelloAck => rt::<HelloAck>(payload),
        MessageType::Bye => rt::<Bye>(payload),
        MessageType::Error => rt::<ErrorMsg>(payload),
        MessageType::SenderState => rt::<SenderState>(payload),
        MessageType::SenderStats => rt::<SenderStats>(payload),
        MessageType::ReceiverReport => rt::<ReceiverReport>(payload),
        MessageType::Configure => rt::<Configure>(payload),
        MessageType::ConfigureAck => rt::<ConfigureAck>(payload),
        other => panic!("{other:?} não é um tipo JSON"),
    }
}

/// Payload binário → campos no formato dos vetores.
fn binary_fields(kind: MessageType, p: &Bytes) -> Result<Value, ProtocolError> {
    Ok(match kind {
        MessageType::Ping => json!({ "t0": Ping::decode_payload(p)?.t0 }),
        MessageType::Pong => {
            let x = Pong::decode_payload(p)?;
            json!({ "t0": x.t0, "t1": x.t1, "t2": x.t2 })
        }
        MessageType::VideoConfig => {
            let c = VideoConfig::decode_payload(p)?;
            let nal_types: Vec<u8> = annexb::nals(&c.param_sets).map(|n| annexb::nal_type(c.codec, n)).collect();
            json!({
                "codec": c.codec as u8, "bit_depth": c.bit_depth, "fps_max": c.fps_max,
                "width": c.width, "height": c.height, "bitrate_bps": c.bitrate_bps,
                "color_primaries": c.color_primaries, "transfer": c.transfer, "matrix": c.matrix,
                "full_range": c.full_range, "param_sets_hex": to_hex(&c.param_sets), "nal_types": nal_types,
            })
        }
        MessageType::AudioConfig => {
            let c = AudioConfig::decode_payload(p)?;
            json!({ "source": c.source.0, "format": c.format as u8, "channels": c.channels, "sample_rate": c.sample_rate })
        }
        MessageType::SetBitrate => json!({ "bitrate_bps": SetBitrate::decode_payload(p)?.bitrate_bps }),
        other => panic!("{other:?} não tem payload binário estruturado"),
    })
}

/// Campos dos vetores → payload binário.
fn binary_encode(kind: MessageType, f: &Value) -> Bytes {
    let u = |k: &str| u64_of(f, k);
    match kind {
        MessageType::Ping => Ping { t0: u("t0") }.encode_payload(),
        MessageType::Pong => Pong { t0: u("t0"), t1: u("t1"), t2: u("t2") }.encode_payload(),
        MessageType::VideoConfig => VideoConfig {
            codec: Codec::from_u8(u("codec") as u8).expect("codec"),
            bit_depth: u("bit_depth") as u8,
            fps_max: u("fps_max") as u16,
            width: u("width") as u16,
            height: u("height") as u16,
            bitrate_bps: u("bitrate_bps") as u32,
            color_primaries: u("color_primaries") as u8,
            transfer: u("transfer") as u8,
            matrix: u("matrix") as u8,
            full_range: f["full_range"].as_bool().expect("full_range"),
            param_sets: Bytes::from(hex(str_of(f, "param_sets_hex"))),
        }
        .encode_payload(),
        MessageType::AudioConfig => AudioConfig {
            source: AudioSource(u("source") as u8),
            format: AudioFormat::from_u8(u("format") as u8).expect("format"),
            channels: u("channels") as u8,
            sample_rate: u("sample_rate") as u32,
        }
        .encode_payload(),
        MessageType::SetBitrate => SetBitrate { bitrate_bps: u("bitrate_bps") as u32 }.encode_payload(),
        other => panic!("{other:?} não tem payload binário estruturado"),
    }
}

#[test]
fn document_matches_crate_constants() {
    let doc = vectors();
    assert_eq!(doc["protocol"], "PSWP");
    assert_eq!(u64_of(&doc, "version"), u64::from(padstudio_protocol::PROTOCOL_VERSION));
    assert_eq!(u64_of(&doc, "header_len"), HEADER_LEN as u64);
    assert_eq!(u64_of(&doc, "max_payload"), u64::from(padstudio_protocol::MAX_PAYLOAD));
}

#[test]
fn every_message_vector_decodes_and_reencodes() {
    let doc = vectors();
    let mut seen_types = std::collections::HashSet::new();

    for v in doc["messages"].as_array().expect("messages") {
        let name = str_of(v, "name");
        let bytes = hex(str_of(v, "hex"));
        let h = &v["header"];

        let header = Header::decode(bytes[..HEADER_LEN].try_into().unwrap()).unwrap();
        assert_eq!(u64::from(header.msg_type), u64_of(h, "type"), "{name}: type");
        assert_eq!(u64::from(header.flags), u64_of(h, "flags"), "{name}: flags");
        assert_eq!(u64::from(header.payload_len), u64_of(h, "payload_len"), "{name}: payload_len");
        assert_eq!(u64::from(header.seq), u64_of(h, "seq"), "{name}: seq");
        assert_eq!(u64::from(header.aux), u64_of(h, "aux"), "{name}: aux");
        assert_eq!(header.pts_us, u64_of(h, "pts_us"), "{name}: pts_us");
        assert_eq!(header.kind().map(MessageType::name), h["type_name"].as_str(), "{name}: type_name");
        assert_eq!(bytes.len(), HEADER_LEN + header.payload_len as usize, "{name}: tamanho");

        let payload = Bytes::copy_from_slice(&bytes[HEADER_LEN..]);
        assert_eq!(to_hex(&payload), str_of(v, "payload_hex"), "{name}: payload");
        let kind = header.kind();
        if let Some(k) = kind {
            seen_types.insert(k);
        }

        match str_of(v, "payload_kind") {
            "json" => {
                let k = kind.expect("tipo JSON conhecido");
                assert!(k.is_json(), "{name}");
                let got = json_roundtrip(k, &payload).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert!(json_eq(&got, &v["payload_json"]), "{name}: {got} != {}", v["payload_json"]);
            }
            "binary" => {
                let k = kind.expect("tipo binário conhecido");
                assert_eq!(binary_fields(k, &payload).unwrap(), v["fields"], "{name}: campos");
                assert_eq!(to_hex(&binary_encode(k, &v["fields"])), str_of(v, "payload_hex"), "{name}: encode");
            }
            "empty" => assert!(payload.is_empty(), "{name}"),
            "raw" => {}
            other => panic!("{name}: payload_kind desconhecido {other}"),
        }

        if kind == Some(MessageType::VideoFrame) {
            let f = VideoFlags::from_u8(header.flags);
            let decoded = json!({
                "keyframe": f.keyframe, "params_inband": f.params_inband,
                "discontinuity": f.discontinuity, "rotation": f.rotation as u8,
            });
            assert_eq!(decoded, v["flags_decoded"], "{name}: flags");
            assert_eq!(f.to_u8(), header.flags, "{name}: flags re-encode");
            let codec = Codec::from_u8(u64_of(v, "context_codec") as u8).unwrap();
            let nal_types: Vec<u8> = annexb::nals(&payload).map(|n| annexb::nal_type(codec, n)).collect();
            assert_eq!(json!(nal_types), v["nal_types"], "{name}: NALs");
        }
        if kind == Some(MessageType::AudioFrame) {
            let f = AudioFlags::from_u8(header.flags);
            assert_eq!(json!({ "source": f.source.0, "discontinuity": f.discontinuity }), v["flags_decoded"], "{name}");
            assert_eq!(f.to_u8(), header.flags, "{name}: flags re-encode");
        }

        let msg = Message { header, payload };
        assert_eq!(msg.to_vec(), bytes, "{name}: to_vec");
        let mut written = Vec::new();
        msg.write_to(&mut written).unwrap();
        assert_eq!(written, bytes, "{name}: write_to");
    }

    for t in MessageType::ALL {
        assert!(seen_types.contains(&t), "sem vetor para {}", t.name());
    }
}

#[test]
fn stream_vectors_parse_with_any_chunking() {
    let doc = vectors();
    let by_name: std::collections::HashMap<&str, &Value> =
        doc["messages"].as_array().unwrap().iter().map(|m| (str_of(m, "name"), m)).collect();

    for s in doc["streams"].as_array().expect("streams") {
        let name = str_of(s, "name");
        let bytes = hex(str_of(s, "hex"));
        let expected: Vec<&str> =
            s["messages"].as_array().unwrap().iter().map(|n| str_of(by_name[n.as_str().unwrap()], "hex")).collect();

        for size in s["chunk_sizes"].as_array().unwrap().iter().map(|c| c.as_u64().unwrap() as usize) {
            let mut parser = StreamParser::new();
            let mut got = Vec::new();
            for chunk in bytes.chunks(size) {
                parser.push(chunk);
                while let Some(m) = parser.next_message().unwrap() {
                    got.push(to_hex(&m.to_vec()));
                }
            }
            assert_eq!(parser.pending(), 0, "{name} / {size}: bytes pendentes");
            assert_eq!(got, expected, "{name} / pedaços de {size}");
        }

        // Também através de std::io::Read.
        let mut parser = StreamParser::new();
        let mut reader = std::io::Cursor::new(bytes.clone());
        let mut count = 0;
        while parser.read_from(&mut reader).unwrap() > 0 {
            while parser.next_message().unwrap().is_some() {
                count += 1;
            }
        }
        assert_eq!(count, expected.len(), "{name}: read_from");
    }
}

#[test]
fn invalid_vectors_fail_with_expected_code() {
    let doc = vectors();
    for inv in doc["invalid"].as_array().expect("invalid") {
        let name = str_of(inv, "name");
        let bytes = hex(str_of(inv, "hex"));
        let err = match str_of(inv, "stage") {
            "header" => {
                let mut parser = StreamParser::new();
                parser.push(&bytes);
                parser.next_message().expect_err(name)
            }
            "payload" => {
                let header = Header::decode(bytes[..HEADER_LEN].try_into().unwrap()).unwrap();
                let payload = Bytes::copy_from_slice(&bytes[HEADER_LEN..]);
                binary_fields(header.kind().unwrap(), &payload).expect_err(name)
            }
            other => panic!("{name}: stage desconhecido {other}"),
        };
        assert_eq!(err.code(), str_of(inv, "error"), "{name}: {err}");
    }
}

#[test]
fn typed_messages_roundtrip_through_parser() {
    let cfg = padstudio_protocol::json::StreamConfig {
        codec: Some(Codec::Hevc),
        max_fps: Some(60),
        bitrate_bps: Some(20_000_000),
        ..Default::default()
    };
    let msgs = [
        Message::from_payload(&Configure(cfg.clone())).with_seq(3),
        Message::from_payload(&Ping { t0: 7 }),
        Message::new(MessageType::RequestKeyframe, Bytes::new()),
    ];
    let mut parser = StreamParser::new();
    for m in &msgs {
        parser.push(&m.to_vec());
    }
    let a = parser.next_message().unwrap().unwrap();
    assert_eq!(a.decode_payload::<Configure>().unwrap().0, cfg);
    assert_eq!(a.header.seq, 3);
    assert!(a.decode_payload::<Ping>().is_err(), "tipo errado tem de falhar");
    assert_eq!(parser.next_message().unwrap().unwrap().decode_payload::<Ping>().unwrap().t0, 7);
    assert_eq!(parser.next_message().unwrap().unwrap().kind(), Some(MessageType::RequestKeyframe));
    assert!(parser.next_message().unwrap().is_none());
}
