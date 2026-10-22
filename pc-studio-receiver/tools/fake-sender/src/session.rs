//! Uma ligação de um PC: handshake, leitura de comandos, fila de envio com prioridades e a
//! política de descarte do iPad (PROTOCOL.md §7.1).

use std::collections::VecDeque;
use std::io::{self, ErrorKind};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use padstudio_protocol::json::{
    Bye, ByeReason, Configure, ConfigureAck, DeviceInfo, ErrorMsg, Hello, HelloAck, ReceiverReport,
};
use padstudio_protocol::payload::{AudioFlags, AudioSource, Codec, Ping, Pong, Rotation, SetBitrate, VideoConfig, VideoFlags};
use padstudio_protocol::{Message, MessageType, PROTOCOL_VERSION, ProtocolError, StreamParser};

use crate::encoder::EncodedAu;
use crate::state::Shared;

/// Limites da fila de vídeo (PROTOCOL.md §7.1).
const MAX_VIDEO_BYTES: usize = 8 * 1024 * 1024;
const MAX_VIDEO_AGE_USB: Duration = Duration::from_millis(100);
const MAX_VIDEO_AGE_LAN: Duration = Duration::from_millis(250);
const MAX_AUDIO_BYTES: usize = 512 * 1024;

const HELLO_TIMEOUT: Duration = Duration::from_secs(3);
const IDLE_TIMEOUT: Duration = Duration::from_secs(5);

// ── Fila de envio ────────────────────────────────────────────────────────────

pub enum Outgoing {
    Msg(Message),
    /// O `t2` é carimbado pelo escritor imediatamente antes de escrever.
    Pong {
        t0: u64,
        t1: u64,
        seq: u32,
    },
    /// Escreve e fecha a ligação.
    Bye(Message),
}

struct Queued {
    msg: Message,
    /// Configurações (VIDEO_CONFIG/AUDIO_CONFIG) nunca são descartadas, para manterem a ordem.
    droppable: bool,
    at: Instant,
}

#[derive(Default)]
struct Inner {
    control: VecDeque<Outgoing>,
    audio: VecDeque<Queued>,
    audio_bytes: usize,
    video: VecDeque<Queued>,
    video_bytes: usize,
    closed: bool,
}

/// Três faixas com prioridade: controlo > áudio > vídeo.
pub struct SendQueue {
    inner: Mutex<Inner>,
    cv: Condvar,
    max_video_age: Duration,
}

impl SendQueue {
    fn new(max_video_age: Duration) -> Self {
        Self { inner: Mutex::new(Inner::default()), cv: Condvar::new(), max_video_age }
    }

    pub fn push_control(&self, item: Outgoing) {
        let mut g = self.inner.lock().unwrap();
        if !g.closed {
            g.control.push_back(item);
            self.cv.notify_one();
        }
    }

    /// Devolve quantos pacotes antigos foram descartados para caber este.
    fn push_audio(&self, msg: Message, droppable: bool) -> usize {
        let mut g = self.inner.lock().unwrap();
        if g.closed {
            return 0;
        }
        let mut dropped = 0;
        while g.audio_bytes + msg.encoded_len() > MAX_AUDIO_BYTES {
            let Some(i) = g.audio.iter().position(|q| q.droppable) else { break };
            let q = g.audio.remove(i).expect("índice válido");
            g.audio_bytes -= q.msg.encoded_len();
            dropped += 1;
        }
        g.audio_bytes += msg.encoded_len();
        g.audio.push_back(Queued { msg, droppable, at: Instant::now() });
        self.cv.notify_one();
        dropped
    }

    /// Recusa o frame e esvazia a faixa de vídeo se a fila estiver cheia ou atrasada.
    /// `Err(n)`: frames descartados da fila.
    fn push_video(&self, msg: Message, droppable: bool) -> Result<(), u64> {
        let mut g = self.inner.lock().unwrap();
        if g.closed {
            return Ok(());
        }
        if droppable {
            let too_big = g.video_bytes + msg.encoded_len() > MAX_VIDEO_BYTES;
            let too_old = g.video.iter().find(|q| q.droppable).is_some_and(|q| q.at.elapsed() > self.max_video_age);
            if too_big || too_old {
                return Err(Self::flush(&mut g));
            }
        }
        g.video_bytes += msg.encoded_len();
        g.video.push_back(Queued { msg, droppable, at: Instant::now() });
        self.cv.notify_one();
        Ok(())
    }

    fn flush_video(&self) -> u64 {
        Self::flush(&mut self.inner.lock().unwrap())
    }

    fn flush(g: &mut Inner) -> u64 {
        let before = g.video.len();
        g.video.retain(|q| !q.droppable);
        g.video_bytes = g.video.iter().map(|q| q.msg.encoded_len()).sum();
        (before - g.video.len()) as u64
    }

    fn pop(&self) -> Option<Outgoing> {
        let mut g = self.inner.lock().unwrap();
        loop {
            if g.closed {
                return None;
            }
            if let Some(item) = g.control.pop_front() {
                return Some(item);
            }
            if let Some(q) = g.audio.pop_front() {
                g.audio_bytes -= q.msg.encoded_len();
                return Some(Outgoing::Msg(q.msg));
            }
            if let Some(q) = g.video.pop_front() {
                g.video_bytes -= q.msg.encoded_len();
                return Some(Outgoing::Msg(q.msg));
            }
            g = self.cv.wait(g).unwrap();
        }
    }

    /// Bytes de vídeo na fila e idade do mais antigo (ms).
    pub fn video_backlog(&self) -> (usize, f64) {
        let g = self.inner.lock().unwrap();
        let age = g.video.front().map_or(0.0, |q| q.at.elapsed().as_secs_f64() * 1000.0);
        (g.video_bytes, age)
    }

    fn close(&self) {
        self.inner.lock().unwrap().closed = true;
        self.cv.notify_all();
    }
}

// ── Sessão ───────────────────────────────────────────────────────────────────

#[derive(Default)]
struct VideoTx {
    started: bool,
    seq: u32,
    sent_config_id: u64,
    dropping: bool,
}

pub struct Session {
    pub id: u64,
    pub peer: SocketAddr,
    pub queue: SendQueue,
    video: Mutex<VideoTx>,
    audio_seq: AtomicU32,
    audio_discontinuity: AtomicBool,
    seqs: [AtomicU32; 0x40],
    pub dropped: AtomicU64,
    pub last_report: Mutex<Option<ReceiverReport>>,
    closed: AtomicBool,
    bye_pending: AtomicBool,
    writer_done: AtomicBool,
}

impl Session {
    fn new(id: u64, peer: SocketAddr, lan: bool) -> Self {
        Self {
            id,
            peer,
            queue: SendQueue::new(if lan { MAX_VIDEO_AGE_LAN } else { MAX_VIDEO_AGE_USB }),
            video: Mutex::new(VideoTx::default()),
            audio_seq: AtomicU32::new(0),
            audio_discontinuity: AtomicBool::new(false),
            seqs: std::array::from_fn(|_| AtomicU32::new(0)),
            dropped: AtomicU64::new(0),
            last_report: Mutex::new(None),
            closed: AtomicBool::new(false),
            bye_pending: AtomicBool::new(false),
            writer_done: AtomicBool::new(false),
        }
    }

    /// Contador `seq` por tipo de mensagem (PROTOCOL.md §2.1).
    pub fn next_seq(&self, t: MessageType) -> u32 {
        self.seqs[(t as usize) & 0x3f].fetch_add(1, Ordering::Relaxed)
    }

    fn send(&self, msg: Message) {
        self.queue.push_control(Outgoing::Msg(msg));
    }

    fn bye(&self, reason: ByeReason, message: &str) {
        let msg = Message::from_payload(&Bye { reason, message: Some(message.into()) }).with_seq(self.next_seq(MessageType::Bye));
        self.bye_pending.store(true, Ordering::SeqCst);
        self.queue.push_control(Outgoing::Bye(msg));
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.queue.close();
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Fecha, dando ao escritor até 500 ms para entregar um BYE pendente.
    fn finish(&self) {
        if self.bye_pending.load(Ordering::SeqCst) {
            let until = Instant::now() + Duration::from_millis(500);
            while !self.writer_done.load(Ordering::SeqCst) && Instant::now() < until {
                thread::sleep(Duration::from_millis(10));
            }
        }
        self.close();
    }

    /// Novo access unit do encoder.
    pub fn on_video(&self, au: &EncodedAu, pts: u64, rotation: Rotation, config: &VideoConfig, config_id: u64) {
        let mut v = self.video.lock().unwrap();
        if !v.started {
            if !au.keyframe {
                return; // uma sessão nova começa sempre num keyframe
            }
            v.started = true;
        }
        if au.keyframe && v.sent_config_id != config_id {
            let msg = Message::from_payload(config).with_seq(self.next_seq(MessageType::VideoConfig)).with_pts(pts);
            let _ = self.queue.push_video(msg, false);
            v.sent_config_id = config_id;
        }

        let seq = v.seq;
        v.seq = v.seq.wrapping_add(1); // conta também os frames que vão ser descartados
        if v.dropping && !au.keyframe {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut flags = VideoFlags {
            keyframe: au.keyframe,
            params_inband: au.keyframe && au.param_sets.is_some(),
            discontinuity: std::mem::take(&mut v.dropping),
            rotation,
        };
        let msg = Message::video_frame(au.data.clone(), flags, seq, 0, pts);
        if let Err(flushed) = self.queue.push_video(msg, true) {
            self.dropped.fetch_add(flushed, Ordering::Relaxed);
            if au.keyframe {
                // A fila foi esvaziada: o keyframe entra logo, marcado como descontinuidade.
                flags.discontinuity = true;
                let msg = Message::video_frame(au.data.clone(), flags, seq, 0, pts);
                if self.queue.push_video(msg, true).is_err() {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                    v.dropping = true;
                }
            } else {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                v.dropping = true;
            }
        }
    }

    /// Novo pacote de áudio da app.
    pub fn on_audio(&self, pcm: Bytes, pts: u64) {
        let flags = AudioFlags {
            source: AudioSource::APP,
            discontinuity: self.audio_discontinuity.swap(false, Ordering::Relaxed),
        };
        let seq = self.audio_seq.fetch_add(1, Ordering::Relaxed);
        if self.queue.push_audio(Message::audio_frame(pcm, flags, seq, 0, pts), true) > 0 {
            self.audio_discontinuity.store(true, Ordering::Relaxed);
        }
    }

    /// Simula uma fila cheia: esvazia o vídeo e descarta até ao próximo keyframe.
    pub fn simulate_drop(&self) -> u64 {
        let mut v = self.video.lock().unwrap();
        v.dropping = true;
        let n = self.queue.flush_video();
        self.dropped.fetch_add(n, Ordering::Relaxed);
        n
    }
}

// ── Ligação ──────────────────────────────────────────────────────────────────

enum ReadOutcome {
    Data,
    Timeout,
    Eof,
}

fn read_some(parser: &mut StreamParser, stream: &mut TcpStream) -> io::Result<ReadOutcome> {
    match parser.read_from(stream) {
        Ok(0) => Ok(ReadOutcome::Eof),
        Ok(_) => Ok(ReadOutcome::Data),
        Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {
            Ok(ReadOutcome::Timeout)
        }
        Err(e) if matches!(e.kind(), ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted) => Ok(ReadOutcome::Eof),
        Err(e) => Err(e),
    }
}

/// Recusa uma ligação antes de haver sessão.
fn reject(stream: &TcpStream, reason: ByeReason, message: &str) {
    let msg = Message::from_payload(&Bye { reason, message: Some(message.into()) });
    let _ = msg.write_to(&mut &*stream);
    let _ = stream.shutdown(Shutdown::Both);
}

pub fn handle_connection(shared: Arc<Shared>, stream: TcpStream) {
    let peer = stream.peer_addr().unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
    if let Err(e) = serve(&shared, stream, peer) {
        log!("{peer}: {e:#}");
    }
}

fn serve(shared: &Arc<Shared>, stream: TcpStream, peer: SocketAddr) -> anyhow::Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    let mut reader = stream.try_clone()?;
    let mut parser = StreamParser::new();

    // 1. HELLO nos primeiros 3 s (PROTOCOL.md §5.1).
    let deadline = Instant::now() + HELLO_TIMEOUT;
    let hello = loop {
        match parser.next_message()? {
            Some(m) if m.kind() == Some(MessageType::Hello) => break m.decode_payload::<Hello>()?,
            Some(_) => continue,
            None => {}
        }
        if Instant::now() >= deadline {
            reject(&stream, ByeReason::Timeout, "HELLO não recebido em 3 s");
            log!("{peer}: sem HELLO em 3 s, ligação recusada");
            return Ok(());
        }
        if let ReadOutcome::Eof = read_some(&mut parser, &mut reader)? {
            return Ok(());
        }
    };

    // 2. Validação.
    if hello.protocol < PROTOCOL_VERSION {
        reject(&stream, ByeReason::ProtocolVersion, "versão do protocolo não suportada");
        log!("{peer}: protocolo {} não suportado", hello.protocol);
        return Ok(());
    }
    if let Some(pin) = &shared.token
        && hello.token.as_deref() != Some(pin.as_str()) {
            reject(&stream, ByeReason::Unauthorized, "PIN errado");
            log!("{peer}: PIN errado, ligação recusada");
            return Ok(());
        }
    if let Some(cfg) = &hello.config {
        shared.apply_config(cfg);
    }

    // 3. Sessão: escritor, HELLO_ACK, AUDIO_CONFIG; o vídeo começa no próximo keyframe.
    let id = shared.next_session_id.fetch_add(1, Ordering::Relaxed);
    let session = Arc::new(Session::new(id, peer, shared.token.is_some()));
    {
        let (shared, session, stream) = (shared.clone(), session.clone(), stream.try_clone()?);
        thread::Builder::new().name("writer".into()).spawn(move || writer(shared, session, stream))?;
    }
    let screen = shared.config.lock().unwrap().screen;
    let ack = HelloAck {
        protocol: PROTOCOL_VERSION,
        server: Some("padstudio-fake-sender".into()),
        server_version: Some(env!("CARGO_PKG_VERSION").into()),
        session_id: Some(format!("fake-{id:04}-{:x}", shared.clock.now_us())),
        clock: Some("host_us".into()),
        device: Some(DeviceInfo {
            model: Some("FakeSender".into()),
            system: Some(format!("simulado em {}", std::env::consts::OS)),
            screen_width: Some(screen.0),
            screen_height: Some(screen.1),
            max_fps: Some(120),
        }),
        codecs: Some(vec![Codec::H264, Codec::Hevc]),
        config: Some(shared.effective_config()),
    };
    session.send(Message::from_payload(&ack).with_seq(session.next_seq(MessageType::HelloAck)).with_pts(shared.clock.now_us()));
    if shared.audio_enabled() {
        let msg = Message::from_payload(&crate::audio::CONFIG).with_seq(session.next_seq(MessageType::AudioConfig));
        session.queue.push_audio(msg, false);
    }
    if let Some(old) = shared.client.lock().unwrap().replace(session.clone()) {
        old.bye(ByeReason::Replaced, "nova ligação aceite");
        log!("{}: substituída por {peer}", old.peer);
    }
    log!(
        "{peer}: ligado ({} {}, {:?})",
        hello.client.as_deref().unwrap_or("cliente"),
        hello.client_version.as_deref().unwrap_or(""),
        hello.transport
    );

    // 4. Comandos do PC.
    let result = read_loop(shared, &session, &mut parser, &mut reader);

    {
        let mut current = shared.client.lock().unwrap();
        if current.as_ref().is_some_and(|s| s.id == session.id) {
            *current = None;
        }
    }
    session.finish();
    log!("{peer}: desligado (drops: {})", session.dropped.load(Ordering::Relaxed));
    result
}

fn read_loop(shared: &Shared, session: &Session, parser: &mut StreamParser, reader: &mut TcpStream) -> anyhow::Result<()> {
    let mut last_rx = Instant::now();
    loop {
        while let Some(msg) = parser.next_message()? {
            if let Err(e) = handle_message(shared, session, &msg) {
                log!("{}: {} inválido: {e}", session.peer, msg.kind().map_or("?", MessageType::name));
                let err = ErrorMsg {
                    code: e.code().into(),
                    message: Some(e.to_string()),
                    fatal: Some(false),
                    ref_type: Some(msg.header.msg_type),
                    ref_seq: Some(msg.header.seq),
                };
                session.send(Message::from_payload(&err).with_seq(session.next_seq(MessageType::Error)));
            }
        }
        if session.is_closed() {
            return Ok(());
        }
        if last_rx.elapsed() > IDLE_TIMEOUT {
            session.bye(ByeReason::Timeout, "sem mensagens do PC há 5 s");
            log!("{}: sem mensagens há 5 s", session.peer);
            return Ok(());
        }
        match read_some(parser, reader)? {
            ReadOutcome::Data => last_rx = Instant::now(),
            ReadOutcome::Timeout => {}
            ReadOutcome::Eof => return Ok(()),
        }
    }
}

fn handle_message(shared: &Shared, session: &Session, msg: &Message) -> Result<(), ProtocolError> {
    let Some(kind) = msg.kind() else {
        return Ok(()); // tipo desconhecido: ignorar (PROTOCOL.md §2.2)
    };
    match kind {
        MessageType::Ping => {
            let ping: Ping = msg.decode_payload()?;
            let t1 = shared.clock.now_us();
            session.queue.push_control(Outgoing::Pong { t0: ping.t0, t1, seq: session.next_seq(MessageType::Pong) });
        }
        MessageType::RequestKeyframe => log!("{}: REQUEST_KEYFRAME (o próximo keyframe chega em ≤ 1 s)", session.peer),
        MessageType::SetBitrate => {
            let b: SetBitrate = msg.decode_payload()?;
            log!(
                "{}: SET_BITRATE {:.1} Mbps ignorado (no fake-sender o bitrate muda com CONFIGURE)",
                session.peer,
                f64::from(b.bitrate_bps) / 1e6
            );
        }
        MessageType::Configure => {
            let req: Configure = msg.decode_payload()?;
            let (config, notes) = shared.apply_config(&req.0);
            let ack = ConfigureAck {
                request_seq: msg.header.seq,
                applied: true,
                config: Some(config),
                message: (!notes.is_empty()).then(|| notes.join("; ")),
            };
            session.send(
                Message::from_payload(&ack)
                    .with_seq(session.next_seq(MessageType::ConfigureAck))
                    .with_pts(shared.clock.now_us()),
            );
        }
        MessageType::ReceiverReport => *session.last_report.lock().unwrap() = Some(msg.decode_payload()?),
        MessageType::Bye => {
            let bye: Bye = msg.decode_payload()?;
            log!("{}: BYE {:?} {}", session.peer, bye.reason, bye.message.unwrap_or_default());
            session.close();
        }
        MessageType::Error => {
            let e: ErrorMsg = msg.decode_payload()?;
            log!("{}: ERROR {} {}", session.peer, e.code, e.message.unwrap_or_default());
        }
        MessageType::Pong => {}
        other => log!("{}: {} inesperado do PC, ignorado", session.peer, other.name()),
    }
    Ok(())
}

fn writer(shared: Arc<Shared>, session: Arc<Session>, mut stream: TcpStream) {
    while let Some(item) = session.queue.pop() {
        let (result, last) = match item {
            Outgoing::Msg(m) => (m.write_to(&mut stream), false),
            Outgoing::Pong { t0, t1, seq } => {
                let pong = Pong { t0, t1, t2: shared.clock.now_us() };
                (Message::from_payload(&pong).with_seq(seq).write_to(&mut stream), false)
            }
            Outgoing::Bye(m) => (m.write_to(&mut stream), true),
        };
        if result.is_err() || last {
            break;
        }
    }
    session.writer_done.store(true, Ordering::SeqCst);
    session.close();
    let _ = stream.shutdown(Shutdown::Both);
}
