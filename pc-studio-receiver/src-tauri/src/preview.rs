//! Servidor WebSocket local do preview (ver docs/ARCHITECTURE.md, ADR-05).
//!
//! Só escuta em 127.0.0.1, numa porta aleatória, e exige um token aleatório gerado no arranque.
//! Envia o vídeo tal como chega do iPad (Anexo-B, sem recodificar); o WebView descodifica-o por
//! hardware com WebCodecs.
//!
//! Mensagens:
//! - texto `{"type":"config","codec":"avc1.640034","width":…,"height":…,"id":…}` antes do vídeo e
//!   quando a configuração muda;
//! - texto `{"type":"ended"}` quando a sessão do iPad acaba;
//! - binário: `[flags u8][rotação u8 (0–3)][recv_us u64 LE][access unit Anexo-B]`, flags bit 0 = keyframe.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io;
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use padstudio_engine::mkv::codec_string;
use padstudio_engine::{Controller, MediaEvent};
use padstudio_protocol::payload::VideoConfig;
use serde::Serialize;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{Message, WebSocket};

/// Fila curta: o preview quer o frame mais recente, não todos.
const QUEUE_EVENTS: usize = 24;

#[derive(Debug, Clone, Serialize)]
pub struct PreviewInfo {
    pub port: u16,
    pub token: String,
}

fn random_token() -> String {
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(i ^ std::process::id() as u64);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

pub fn start(ctrl: Controller) -> io::Result<PreviewInfo> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let info = PreviewInfo { port: listener.local_addr()?.port(), token: random_token() };
    let token = info.token.clone();
    thread::Builder::new().name("padstudio-preview".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let (ctrl, token) = (ctrl.clone(), token.clone());
            let _ = thread::Builder::new().name("padstudio-preview-client".into()).spawn(move || serve(stream, ctrl, &token));
        }
    })?;
    Ok(info)
}

fn config_message(cfg: &VideoConfig, id: u64) -> Message {
    let text = serde_json::json!({
        "type": "config",
        "codec": codec_string(cfg),
        "width": cfg.width,
        "height": cfg.height,
        "fps": cfg.fps_max,
        "id": id,
    });
    Message::text(text.to_string())
}

// O tipo de erro do callback de handshake é imposto pelo tungstenite.
#[allow(clippy::result_large_err)]
fn serve(stream: TcpStream, ctrl: Controller, token: &str) {
    let _ = stream.set_nodelay(true);
    let expected = format!("token={token}");
    let check = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        let ok = req.uri().query().is_some_and(|q| q.split('&').any(|kv| kv == expected));
        if ok {
            Ok(resp)
        } else {
            let mut deny = ErrorResponse::new(Some("token inválido".into()));
            *deny.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
            Err(deny)
        }
    };
    let Ok(mut ws) = tungstenite::accept_hdr(stream, check) else { return };
    let _ = run(&mut ws, &ctrl);
    let _ = ws.close(None);
}

fn run(ws: &mut WebSocket<TcpStream>, ctrl: &Controller) -> tungstenite::Result<()> {
    // Subscrever pede logo um keyframe, para o preview aparecer de imediato.
    let (sub, state) = ctrl.subscribe(QUEUE_EVENTS);
    let mut waiting_key = true;
    if let Some((cfg, id)) = &state.video {
        ws.send(config_message(cfg, *id))?;
    }
    loop {
        match sub.recv_timeout(Duration::from_secs(1)) {
            Ok(Some(MediaEvent::VideoConfig { config, id })) => {
                ws.send(config_message(&config, id))?;
                waiting_key = true;
            }
            Ok(Some(MediaEvent::Video(p))) => {
                if waiting_key && !p.flags.keyframe {
                    continue;
                }
                waiting_key = false;
                let mut buf = Vec::with_capacity(10 + p.data.len());
                buf.push(u8::from(p.flags.keyframe));
                buf.push(p.flags.rotation as u8);
                buf.extend_from_slice(&p.recv_us.to_le_bytes());
                buf.extend_from_slice(&p.data);
                ws.send(Message::binary(buf))?;
            }
            Ok(Some(MediaEvent::SessionEnded { .. })) => {
                ws.send(Message::text(r#"{"type":"ended"}"#))?;
                waiting_key = true;
            }
            Ok(Some(MediaEvent::Gap(_))) => {
                // O WebView não acompanhou: recomeça no próximo keyframe.
                waiting_key = true;
                ctrl.request_keyframe();
            }
            Ok(Some(_)) => {}
            Ok(None) => ws.send(Message::Ping(Vec::new().into()))?, // deteta clientes que fecharam
            Err(_) => return Ok(()),
        }
    }
}
