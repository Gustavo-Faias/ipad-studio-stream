//! Uma sessão PSWP: thread de leitura, estatísticas por segundo e fotografia de telemetria.

use std::io::{self, ErrorKind, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use padstudio_protocol::clock::ClockSync;
use padstudio_protocol::json::{
    BroadcastState, Bye, ByeReason, ConfigureAck, ErrorMsg, HelloAck, ReceiverReport, SenderState, SenderStats,
};
use padstudio_protocol::payload::{AudioConfig, Ping, Pong, Rotation, VideoConfig};
use padstudio_protocol::{Message, MessageType, ProtocolError, SeqTracker, StreamParser};

use crate::devices::display_name;
use crate::engine::EngineShared;
use crate::media::{AudioPacket, MediaEvent, SessionInfo, TransportKind, VideoPacket};
use crate::telemetry::{Telemetry, percentile, sorted};

pub(crate) type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

pub(crate) fn send(writer: &SharedWriter, msg: &Message) -> io::Result<()> {
    let mut w = writer.lock().unwrap();
    msg.write_to(&mut *w)
}

/// Acumuladores do segundo atual.
#[derive(Default)]
struct Window {
    frames: u64,
    video_bytes: u64,
    link_bytes: u64,
    audio_packets: u64,
    total_ms: Vec<f64>,
    sender_ms: Vec<f64>,
    network_ms: Vec<f64>,
}

#[derive(Default)]
struct Stats {
    window: Window,
    frames_total: u64,
    keyframes: u64,
    discontinuities: u64,
    video_seq: SeqTracker,
    rotation: Rotation,
    video: Option<VideoConfig>,
    sender: Option<SenderStats>,
}

pub(crate) struct Session {
    pub target: String,
    pub transport: TransportKind,
    device_name: Option<String>,
    started: Instant,
    alive: AtomicBool,
    end_reason: Mutex<Option<String>>,
    pub last_rx: Mutex<Instant>,
    pub hello_ack: Mutex<Option<HelloAck>>,
    clock_sync: Mutex<ClockSync>,
    stats: Mutex<Stats>,
    seqs: [AtomicU32; 0x40],
    config_id: AtomicU64,
}

impl Session {
    pub fn new(target: String, transport: TransportKind, device_name: Option<String>) -> Self {
        Self {
            target,
            transport,
            device_name,
            started: Instant::now(),
            alive: AtomicBool::new(true),
            end_reason: Mutex::new(None),
            last_rx: Mutex::new(Instant::now()),
            hello_ack: Mutex::new(None),
            clock_sync: Mutex::new(ClockSync::default()),
            stats: Mutex::new(Stats::default()),
            seqs: std::array::from_fn(|_| AtomicU32::new(0)),
            config_id: AtomicU64::new(0),
        }
    }

    pub fn next_seq(&self, t: MessageType) -> u32 {
        self.seqs[(t as usize) & 0x3f].fetch_add(1, Ordering::Relaxed)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Termina a sessão; fica o primeiro motivo registado.
    pub fn end(&self, reason: String) {
        let mut r = self.end_reason.lock().unwrap();
        if r.is_none() {
            *r = Some(reason);
        }
        self.alive.store(false, Ordering::SeqCst);
    }

    pub fn end_reason(&self) -> String {
        self.end_reason.lock().unwrap().clone().unwrap_or_else(|| "terminada".into())
    }

    /// Nome do dispositivo: do lockdownd (USB) ou do modelo no HELLO_ACK.
    pub fn device_label(&self) -> Option<String> {
        self.device_name.clone().or_else(|| {
            let ack = self.hello_ack.lock().unwrap();
            let model = ack.as_ref()?.device.as_ref()?.model.clone()?;
            Some(display_name(Some(&model), &model))
        })
    }

    /// Fotografia do último segundo (esvazia a janela) e o RECEIVER_REPORT correspondente.
    pub fn snapshot(&self) -> (Telemetry, ReceiverReport) {
        let best = self.clock_sync.lock().unwrap().best();
        let device_system = self
            .hello_ack
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|a| a.device.as_ref())
            .and_then(|d| d.system.clone());
        let device_name = self.device_label();

        let mut st = self.stats.lock().unwrap();
        let w = std::mem::take(&mut st.window);
        let total = sorted(w.total_ms);
        let sender = sorted(w.sender_ms);
        let network = sorted(w.network_ms);
        let video = st.video.as_ref();
        let t = Telemetry {
            connected: true,
            target: Some(self.target.clone()),
            device_name,
            device_system,
            codec: video.map(|v| v.codec.name().to_string()),
            width: video.map_or(0, |v| v.width),
            height: video.map_or(0, |v| v.height),
            fps_max: video.map_or(0, |v| v.fps_max),
            rotation_deg: st.rotation.degrees(),
            fps: w.frames as f64,
            link_bitrate_bps: w.link_bytes * 8,
            video_bitrate_bps: w.video_bytes * 8,
            audio_packets_per_s: w.audio_packets as f64,
            latency_total_p50_ms: percentile(&total, 0.5),
            latency_total_p95_ms: percentile(&total, 0.95),
            latency_sender_p50_ms: percentile(&sender, 0.5),
            latency_network_p50_ms: percentile(&network, 0.5),
            rtt_ms: best.map(|b| b.rtt_us as f64 / 1000.0),
            clock_offset_ms: best.map(|b| b.offset_us as f64 / 1000.0),
            frames_total: st.frames_total,
            keyframes_total: st.keyframes,
            frames_lost: st.video_seq.lost(),
            discontinuities: st.discontinuities,
            sender: st.sender.clone(),
            session_uptime_s: self.started.elapsed().as_secs_f64(),
        };
        let report = ReceiverReport {
            fps_received: Some(t.fps),
            fps_decoded: None,
            recv_bitrate_bps: Some(t.link_bitrate_bps),
            latency_ms_p50: t.latency_total_p50_ms,
            latency_ms_p95: t.latency_total_p95_ms,
            rtt_ms: t.rtt_ms,
            decode_errors: None,
        };
        (t, report)
    }
}

/// Lê do transporte até a ligação acabar; publica media no bus e atualiza as estatísticas.
pub(crate) fn reader_loop(mut reader: Box<dyn Read + Send>, s: Arc<Session>, eng: Arc<EngineShared>, writer: SharedWriter) {
    let mut parser = StreamParser::new();
    let reason = 'outer: loop {
        match parser.read_from(&mut reader) {
            Ok(0) => break "o iPad fechou a ligação".to_string(),
            Ok(n) => {
                *s.last_rx.lock().unwrap() = Instant::now();
                s.stats.lock().unwrap().window.link_bytes += n as u64;
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
            Err(e) => break format!("ligação perdida ({e})"),
        }
        if !s.is_alive() {
            break s.end_reason();
        }
        let recv_us = eng.clock.now_us();
        loop {
            match parser.next_message() {
                Ok(Some(msg)) => match handle(&s, &eng, &writer, &msg, recv_us) {
                    Ok(Some(reason)) => break 'outer reason,
                    Ok(None) => {}
                    Err(e) => eng.warn(format!(
                        "mensagem {} inválida do iPad: {e}",
                        msg.kind().map_or("?", MessageType::name)
                    )),
                },
                Ok(None) => break,
                Err(e) => break 'outer format!("erro de protocolo: {e}"),
            }
        }
    };
    s.end(reason);
}

fn bye_text(b: &Bye) -> String {
    let what = match b.reason {
        ByeReason::UserStopped => "a transmissão foi terminada no iPad",
        ByeReason::Replaced => "outra ligação substituiu esta",
        ByeReason::Unauthorized => "PIN errado",
        ByeReason::ProtocolVersion => "versão do protocolo incompatível",
        ByeReason::Timeout => "o iPad deixou de receber mensagens do PC",
        ByeReason::Shutdown => "o iPad encerrou a transmissão",
        ByeReason::Error => "erro no iPad",
        ByeReason::Unknown => "motivo desconhecido",
    };
    match &b.message {
        Some(m) if !m.is_empty() => format!("{what} ({m})"),
        _ => what.to_string(),
    }
}

/// Trata uma mensagem. `Ok(Some(motivo))` termina a sessão.
fn handle(s: &Session, eng: &EngineShared, writer: &SharedWriter, msg: &Message, recv_us: u64) -> Result<Option<String>, ProtocolError> {
    let Some(kind) = msg.kind() else {
        return Ok(None); // tipo desconhecido: ignorar (PROTOCOL.md §2.2)
    };
    match kind {
        MessageType::HelloAck => {
            let ack: HelloAck = msg.decode_payload()?;
            let device = ack.device.clone().unwrap_or_default();
            *s.hello_ack.lock().unwrap() = Some(ack.clone());
            let info = Arc::new(SessionInfo {
                session_id: ack.session_id,
                target: s.target.clone(),
                transport: s.transport,
                server: ack.server,
                device,
                device_name: s.device_label(),
                config: ack.config.unwrap_or_default(),
            });
            eng.media.lock().unwrap().session = Some(info.clone());
            eng.bus.publish(MediaEvent::SessionStarted(info));
        }
        MessageType::VideoConfig => {
            let config: VideoConfig = msg.decode_payload()?;
            let id = eng.next_config_id.fetch_add(1, Ordering::SeqCst) + 1;
            s.config_id.store(id, Ordering::SeqCst);
            eng.media.lock().unwrap().video = Some((config.clone(), id));
            s.stats.lock().unwrap().video = Some(config.clone());
            eng.bus.publish(MediaEvent::VideoConfig { config, id });
        }
        MessageType::VideoFrame => {
            let config_id = s.config_id.load(Ordering::SeqCst);
            if config_id == 0 {
                return Ok(None); // frames antes de VIDEO_CONFIG: ignorar (§4.3)
            }
            let flags = msg.video_flags();
            let local_pts = s.clock_sync.lock().unwrap().remote_to_local(msg.header.pts_us);
            {
                let mut st = s.stats.lock().unwrap();
                st.video_seq.observe(msg.header.seq);
                st.frames_total += 1;
                st.keyframes += u64::from(flags.keyframe);
                st.discontinuities += u64::from(flags.discontinuity);
                st.rotation = flags.rotation;
                st.window.frames += 1;
                st.window.video_bytes += msg.payload.len() as u64;
                if let Some(local) = local_pts {
                    let total = (recv_us as i64 - local) as f64 / 1000.0;
                    let sender = f64::from(msg.header.aux) / 1000.0;
                    st.window.total_ms.push(total);
                    st.window.sender_ms.push(sender);
                    st.window.network_ms.push(total - sender);
                }
            }
            eng.bus.publish(MediaEvent::Video(VideoPacket {
                data: msg.payload.clone(),
                flags,
                seq: msg.header.seq,
                pts_us: msg.header.pts_us,
                recv_us,
                config_id,
            }));
        }
        MessageType::AudioConfig => {
            let config: AudioConfig = msg.decode_payload()?;
            eng.media.lock().unwrap().audio.insert(config.source.0, config);
            eng.bus.publish(MediaEvent::AudioConfig(config));
        }
        MessageType::AudioFrame => {
            let flags = msg.audio_flags();
            if !eng.media.lock().unwrap().audio.contains_key(&flags.source.0) {
                return Ok(None); // fonte sem AUDIO_CONFIG
            }
            s.stats.lock().unwrap().window.audio_packets += 1;
            eng.bus.publish(MediaEvent::Audio(AudioPacket {
                source: flags.source,
                data: msg.payload.clone(),
                discontinuity: flags.discontinuity,
                seq: msg.header.seq,
                pts_us: msg.header.pts_us,
                recv_us,
            }));
        }
        MessageType::Pong => {
            let pong: Pong = msg.decode_payload()?;
            let mut sync = s.clock_sync.lock().unwrap();
            sync.add(&pong, recv_us);
            *eng.clock_offset_us.lock().unwrap() = sync.best().map(|b| b.offset_us);
        }
        MessageType::Ping => {
            let ping: Ping = msg.decode_payload()?;
            let pong = Pong { t0: ping.t0, t1: recv_us, t2: eng.clock.now_us() };
            let _ = send(writer, &Message::from_payload(&pong).with_seq(s.next_seq(MessageType::Pong)));
        }
        MessageType::SenderStats => s.stats.lock().unwrap().sender = Some(msg.decode_payload()?),
        MessageType::SenderState => {
            let st: SenderState = msg.decode_payload()?;
            eng.info(match st.state {
                BroadcastState::Paused => "iPad: transmissão em pausa",
                BroadcastState::Active => "iPad: transmissão retomada",
                BroadcastState::Finishing => "iPad: transmissão a terminar",
                BroadcastState::Unknown => "iPad: estado de transmissão desconhecido",
            });
        }
        MessageType::ConfigureAck => {
            let ack: ConfigureAck = msg.decode_payload()?;
            let note = ack.message.map(|m| format!(" ({m})")).unwrap_or_default();
            if ack.applied {
                eng.info(format!("o iPad aplicou a nova configuração{note}"));
            } else {
                eng.warn(format!("o iPad não aplicou a configuração{note}"));
            }
        }
        MessageType::Error => {
            let e: ErrorMsg = msg.decode_payload()?;
            eng.warn(format!("erro do iPad: {} {}", e.code, e.message.unwrap_or_default()));
        }
        MessageType::Bye => {
            let bye: Bye = msg.decode_payload()?;
            return Ok(Some(bye_text(&bye)));
        }
        _ => {} // mensagens do sentido PC → iPad: ignorar
    }
    Ok(None)
}
