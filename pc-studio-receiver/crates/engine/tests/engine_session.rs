//! Motor contra um iPad simulado em memória (TCP local): handshake, ordem dos eventos no bus,
//! telemetria com relógios diferentes, comandos, fim de sessão + religação e gravação.

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use padstudio_engine::recorder::{Recorder, RecorderOptions};
use padstudio_engine::{Endpoint, Engine, EngineConfig, EngineEvent, EngineStatus, MediaEvent, Subscription, WaitReason};
use padstudio_protocol::json::{Bye, ByeReason, Configure, ConfigureAck, DeviceInfo, Hello, HelloAck, StreamConfig};
use padstudio_protocol::payload::{
    AudioConfig, AudioFlags, AudioFormat, AudioSource, Codec, Ping, Pong, VideoConfig, VideoFlags,
};
use padstudio_protocol::{Message, MessageType, StreamParser};

/// O relógio do iPad simulado está 50 000 s à frente: a sincronização tem de o compensar.
const IPAD_CLOCK_OFFSET_US: u64 = 50_000_000_000;
const SENDER_AUX_US: u32 = 3_000;
const PARAMS: &[u8] = &[0, 0, 0, 1, 0x67, 0x64, 0x00, 0x34, 0xAC, 0, 0, 0, 1, 0x68, 0xEE, 0x3C, 0xB0];
const IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00, 0x33];
const P_FRAME: &[u8] = &[0, 0, 0, 1, 0x41, 0x9A, 0x02];

#[derive(Default)]
struct MockStats {
    hellos: AtomicU32,
    keyframe_requests: AtomicU32,
    configures: Mutex<Vec<StreamConfig>>,
}

struct MockIpad {
    addr: String,
    stats: Arc<MockStats>,
}

/// Aceita `sessions` ligações seguidas; cada uma dura `session_len` e termina com BYE.
fn mock_ipad(sessions: usize, session_len: Duration) -> MockIpad {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let stats = Arc::new(MockStats::default());
    let st = stats.clone();
    thread::spawn(move || {
        let start = Instant::now();
        for _ in 0..sessions {
            let (stream, _) = listener.accept().unwrap();
            serve(stream, &st, start, session_len);
        }
    });
    MockIpad { addr, stats }
}

fn serve(mut stream: TcpStream, st: &MockStats, start: Instant, session_len: Duration) {
    let now = || IPAD_CLOCK_OFFSET_US + start.elapsed().as_micros() as u64;
    stream.set_nodelay(true).unwrap();
    stream.set_read_timeout(Some(Duration::from_millis(2))).unwrap();
    let mut reader = stream.try_clone().unwrap();
    let mut parser = StreamParser::new();
    let mut seqs = [0u32; 0x40];
    let mut next_seq = |t: MessageType| {
        let s = &mut seqs[t as usize];
        *s += 1;
        *s - 1
    };

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(m) = parser.next_message().unwrap()
            && m.kind() == Some(MessageType::Hello) {
                assert_eq!(m.decode_payload::<Hello>().unwrap().protocol, 1);
                st.hellos.fetch_add(1, Ordering::SeqCst);
                break;
            }
        assert!(Instant::now() < deadline, "sem HELLO");
        let _ = parser.read_from(&mut reader);
    }

    let ack = HelloAck {
        protocol: 1,
        server: Some("mock".into()),
        server_version: None,
        session_id: Some("mock-session".into()),
        clock: Some("host_us".into()),
        device: Some(DeviceInfo {
            model: Some("iPad8,1".into()),
            system: Some("iPadOS 26.6.2".into()),
            screen_width: Some(2388),
            screen_height: Some(1668),
            max_fps: Some(120),
        }),
        codecs: Some(vec![Codec::H264]),
        config: None,
    };
    let audio = AudioConfig { source: AudioSource::APP, format: AudioFormat::S16Le, channels: 2, sample_rate: 48_000 };
    let video = VideoConfig {
        codec: Codec::H264,
        bit_depth: 8,
        fps_max: 120,
        width: 1920,
        height: 1342,
        bitrate_bps: 30_000_000,
        color_primaries: 1,
        transfer: 1,
        matrix: 1,
        full_range: false,
        param_sets: Bytes::from_static(PARAMS),
    };
    for m in [
        Message::from_payload(&ack).with_seq(next_seq(MessageType::HelloAck)),
        Message::from_payload(&audio).with_seq(next_seq(MessageType::AudioConfig)),
        Message::from_payload(&video).with_seq(next_seq(MessageType::VideoConfig)).with_pts(now()),
    ] {
        m.write_to(&mut stream).unwrap();
    }

    let session_start = Instant::now();
    let (mut vseq, mut aseq, mut force_key) = (0u32, 0u32, true);
    let (mut next_frame, mut next_audio) = (Instant::now(), Instant::now());
    loop {
        if session_start.elapsed() > session_len {
            let bye = Bye { reason: ByeReason::UserStopped, message: Some("fim do teste".into()) };
            let _ = Message::from_payload(&bye).write_to(&mut stream);
            return;
        }
        match parser.read_from(&mut reader) {
            Ok(0) => return,
            Ok(_) => {}
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return,
        }
        while let Some(m) = parser.next_message().unwrap() {
            match m.kind() {
                Some(MessageType::Ping) => {
                    let t0 = m.decode_payload::<Ping>().unwrap().t0;
                    let pong = Pong { t0, t1: now(), t2: now() };
                    let _ = Message::from_payload(&pong).with_seq(next_seq(MessageType::Pong)).write_to(&mut stream);
                }
                Some(MessageType::RequestKeyframe) => {
                    st.keyframe_requests.fetch_add(1, Ordering::SeqCst);
                    force_key = true;
                }
                Some(MessageType::Configure) => {
                    let c = m.decode_payload::<Configure>().unwrap().0;
                    st.configures.lock().unwrap().push(c);
                    let ack = ConfigureAck { request_seq: m.header.seq, applied: true, config: None, message: None };
                    let _ = Message::from_payload(&ack).write_to(&mut stream);
                }
                Some(MessageType::Bye) => return,
                _ => {}
            }
        }
        // No Windows o timeout de 2 ms dura ~15 ms: envia de seguida tudo o que estiver em atraso.
        while Instant::now() >= next_frame {
            let key = force_key || vseq % 60 == 0;
            force_key = false;
            let data = if key { Bytes::from([PARAMS, IDR].concat()) } else { Bytes::from_static(P_FRAME) };
            let flags = VideoFlags { keyframe: key, params_inband: key, ..Default::default() };
            if Message::video_frame(data, flags, vseq, SENDER_AUX_US, now()).write_to(&mut stream).is_err() {
                return;
            }
            vseq += 1;
            next_frame += Duration::from_micros(8_333);
        }
        while Instant::now() >= next_audio {
            let flags = AudioFlags { source: AudioSource::APP, discontinuity: false };
            let _ = Message::audio_frame(Bytes::from(vec![0u8; 4096]), flags, aseq, 0, now()).write_to(&mut stream);
            aseq += 1;
            next_audio += Duration::from_micros(21_333);
        }
    }
}

fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(v) = f() {
            return Some(v);
        }
        thread::sleep(Duration::from_millis(10));
    }
    None
}

fn label(e: &MediaEvent) -> &'static str {
    match e {
        MediaEvent::SessionStarted(_) => "start",
        MediaEvent::SessionEnded { .. } => "end",
        MediaEvent::VideoConfig { .. } => "vcfg",
        MediaEvent::Video(p) if p.flags.keyframe => "key",
        MediaEvent::Video(_) => "p",
        MediaEvent::AudioConfig(_) => "acfg",
        MediaEvent::Audio(_) => "a",
        MediaEvent::MixedAudio(_) => "mix",
        MediaEvent::Gap(_) => "gap",
    }
}

/// Junta eventos até `done(&labels)` ou até ao fim do tempo.
fn drain(sub: &Subscription, timeout: Duration, labels: &mut Vec<&'static str>, events: &mut Vec<MediaEvent>, done: impl Fn(&[&str]) -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(Some(e)) = sub.recv_timeout(Duration::from_millis(20)) {
            labels.push(label(&e));
            events.push(e);
            if done(labels) {
                return true;
            }
        }
    }
    false
}

#[test]
fn streams_measures_controls_and_reconnects() {
    let ipad = mock_ipad(2, Duration::from_millis(2000));
    let engine = Engine::start(EngineConfig {
        endpoint: Endpoint::tcp(&ipad.addr),
        retry_interval: Duration::from_millis(200),
        ..Default::default()
    });
    let ctrl = engine.controller();
    let (sub, state) = ctrl.subscribe(4096);
    assert!(state.video.is_none());

    // Ordem: SessionStarted → AUDIO_CONFIG → VIDEO_CONFIG → keyframe.
    let (mut labels, mut events) = (Vec::new(), Vec::new());
    assert!(drain(&sub, Duration::from_secs(3), &mut labels, &mut events, |l| l.contains(&"key")), "{labels:?}");
    assert_eq!(labels[0], "start", "{labels:?}");
    let pos = |x: &str| labels.iter().position(|l| *l == x).unwrap();
    assert!(pos("acfg") < pos("vcfg") && pos("vcfg") < pos("key"), "{labels:?}");
    assert!(!labels[..pos("key")].contains(&"p"), "o primeiro frame tem de ser keyframe");
    let MediaEvent::SessionStarted(info) = &events[0] else { unreachable!() };
    assert_eq!(info.device_name.as_deref(), Some("iPad Pro 11\" (2018)"));
    let streaming = wait_for(Duration::from_secs(1), || matches!(ctrl.status(), EngineStatus::Streaming { .. }).then_some(()));
    assert!(streaming.is_some(), "{:?}", ctrl.status());

    // Telemetria do primeiro segundo completo.
    let t = wait_for(Duration::from_secs(2), || {
        let t = ctrl.telemetry();
        (t.connected && t.fps > 60.0 && t.rtt_ms.is_some()).then_some(t)
    })
    .unwrap_or_else(|| panic!("telemetria incompleta: {:?}", ctrl.telemetry()));
    assert_eq!(t.codec.as_deref(), Some("h264"));
    assert_eq!((t.width, t.height, t.fps_max), (1920, 1342, 120));
    assert_eq!(t.device_system.as_deref(), Some("iPadOS 26.6.2"));
    let offset_ms = t.clock_offset_ms.unwrap();
    assert!((offset_ms - IPAD_CLOCK_OFFSET_US as f64 / 1000.0).abs() < 1_000.0, "offset {offset_ms}");
    // A conversão iPad → local também está disponível para outros componentes (mixer, streaming).
    let ipad_now = IPAD_CLOCK_OFFSET_US + 10_000_000;
    let local = ctrl.remote_to_local(ipad_now).expect("offset conhecido");
    assert!((local - (ipad_now as i64 - (offset_ms * 1000.0) as i64)).abs() < 2_000);
    let total = t.latency_total_p50_ms.unwrap();
    assert!((-5.0..50.0).contains(&total), "latência {total} ms apesar dos relógios diferentes");
    assert_eq!(t.latency_sender_p50_ms, Some(f64::from(SENDER_AUX_US) / 1000.0));
    assert!(t.audio_packets_per_s > 30.0, "{t:?}");
    assert_eq!(t.frames_lost, 0);

    // Comandos chegam ao iPad.
    ctrl.request_keyframe();
    ctrl.configure(StreamConfig { bitrate_bps: Some(10_000_000), ..Default::default() });
    let got = wait_for(Duration::from_secs(1), || {
        let configs = ipad.stats.configures.lock().unwrap();
        (ipad.stats.keyframe_requests.load(Ordering::SeqCst) >= 1 && configs.len() == 1).then(|| configs[0].clone())
    });
    assert_eq!(got.and_then(|c| c.bitrate_bps), Some(10_000_000));

    // O iPad termina a sessão (BYE) e o motor volta a ligar sozinho.
    assert!(
        drain(&sub, Duration::from_secs(5), &mut labels, &mut events, |l| {
            l.iter().position(|x| *x == "end").is_some_and(|e| l[e..].contains(&"start"))
        }),
        "sem religação: {:?}",
        &labels[labels.len().saturating_sub(10)..]
    );
    assert_eq!(ipad.stats.hellos.load(Ordering::SeqCst), 2);
    engine.shutdown();
}

#[test]
fn explains_why_it_is_waiting() {
    // Porta onde ninguém escuta.
    let addr = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let engine = Engine::start(EngineConfig {
        endpoint: Endpoint::tcp(addr.to_string()),
        retry_interval: Duration::from_millis(100),
        ..Default::default()
    });
    let reason = wait_for(Duration::from_secs(8), || match engine.events().try_recv() {
        Ok(EngineEvent::Status(EngineStatus::Waiting { reason, .. })) => Some(reason),
        _ => None,
    });
    assert_eq!(reason, Some(WaitReason::BroadcastInactive));
    engine.shutdown();
}

#[test]
fn recorder_started_mid_session_begins_on_requested_keyframe() {
    let ipad = mock_ipad(1, Duration::from_secs(30));
    let engine = Engine::start(EngineConfig { endpoint: Endpoint::tcp(&ipad.addr), ..Default::default() });
    let ctrl = engine.controller();
    wait_for(Duration::from_secs(3), || ctrl.media_state().video.map(|_| ())).expect("sem vídeo");
    let requests_before = ipad.stats.keyframe_requests.load(Ordering::SeqCst);

    let dir = std::env::temp_dir().join(format!("padstudio-rec-test-{}", std::process::id()));
    let rec = Recorder::start(&ctrl, RecorderOptions::new(&dir)).unwrap();
    thread::sleep(Duration::from_millis(1200));
    let files = rec.stop().unwrap();

    assert!(ipad.stats.keyframe_requests.load(Ordering::SeqCst) > requests_before, "o gravador devia pedir um keyframe");
    assert_eq!(files.len(), 1, "{files:?}");
    let f = &files[0];
    assert!(f.video_frames > 100, "{f:?}");
    assert!(f.audio_blocks > 30, "{f:?}");
    assert!(f.duration_s > 0.8, "{f:?}");
    let bytes = std::fs::read(&f.path).unwrap();
    assert_eq!(&bytes[..4], &[0x1A, 0x45, 0xDF, 0xA3], "cabeçalho EBML");
    assert_eq!(bytes.len() as u64, f.bytes);
    let _ = std::fs::remove_dir_all(&dir);
    engine.shutdown();
}
