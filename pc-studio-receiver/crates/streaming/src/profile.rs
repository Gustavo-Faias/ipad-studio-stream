//! Destinos (Twitch, YouTube, TikTok, RTMP personalizado), perfis de vídeo e a linha de comandos
//! do FFmpeg para cada um.

use padstudio_protocol::payload::Rotation;
use serde::{Deserialize, Serialize};

use crate::encoder::EncoderKind;

/// Como encaixar o ecrã do iPad no enquadramento de saída.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// Ecrã inteiro ao centro, com o próprio ecrã desfocado a preencher o resto.
    FitBlur,
    /// Ecrã inteiro ao centro, com barras pretas.
    FitBlack,
    /// Preenche o enquadramento cortando as margens.
    Crop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoProfile {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub layout: Layout,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    /// Identificador estável (ex.: "twitch").
    pub id: String,
    /// Nome para a UI.
    pub name: String,
    /// URL RTMP(S) completo, com a chave. **Segredo**: nunca vai para logs nem para a UI.
    pub url: String,
    pub video: VideoProfile,
    pub audio_bitrate_kbps: u32,
}

impl Destination {
    /// Twitch: 1080p60, 6000 kbps (o máximo recomendado para não parceiros).
    pub fn twitch(stream_key: &str) -> Self {
        Self {
            id: "twitch".into(),
            name: "Twitch".into(),
            url: join_url("rtmp://live.twitch.tv/app", stream_key),
            video: VideoProfile { width: 1920, height: 1080, fps: 60, bitrate_kbps: 6000, layout: Layout::FitBlur },
            audio_bitrate_kbps: 160,
        }
    }

    /// YouTube (RTMPS): 1080p60, 12000 kbps.
    pub fn youtube(stream_key: &str) -> Self {
        Self {
            id: "youtube".into(),
            name: "YouTube".into(),
            url: join_url("rtmps://a.rtmps.youtube.com/live2", stream_key),
            video: VideoProfile { width: 1920, height: 1080, fps: 60, bitrate_kbps: 12000, layout: Layout::FitBlur },
            audio_bitrate_kbps: 160,
        }
    }

    /// TikTok: o servidor e a chave vêm do TikTok LIVE Producer. Vertical (9:16) ou horizontal.
    pub fn tiktok(server_url: &str, stream_key: &str, vertical: bool) -> Self {
        let (width, height) = if vertical { (1080, 1920) } else { (1920, 1080) };
        Self {
            id: "tiktok".into(),
            name: "TikTok".into(),
            url: join_url(server_url, stream_key),
            video: VideoProfile { width, height, fps: 60, bitrate_kbps: 6000, layout: Layout::FitBlur },
            audio_bitrate_kbps: 160,
        }
    }

    /// URL sem a chave, para mostrar.
    pub fn redacted_url(&self) -> String {
        redact_url(&self.url)
    }
}

pub(crate) fn join_url(server: &str, key: &str) -> String {
    if key.is_empty() {
        server.to_string()
    } else if server.ends_with('/') {
        format!("{server}{key}")
    } else {
        format!("{server}/{key}")
    }
}

/// Esconde a chave: o último segmento do caminho e a query string.
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return "****".into() };
    let (path, _query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut parts: Vec<&str> = path.split('/').collect();
    if parts.len() > 2
        && let Some(last) = parts.last_mut()
            && !last.is_empty() {
                *last = "****";
            }
    format!("{scheme}://{}", parts.join("/"))
}

/// Substitui o URL completo (e a chave sozinha) por versões escondidas numa linha de log.
pub(crate) fn redact_line(line: &str, url: &str) -> String {
    let mut out = line.replace(url, &redact_url(url));
    if let Some(key) = url.rsplit('/').next().filter(|k| k.len() >= 6) {
        out = out.replace(key, "****");
    }
    out
}

fn even(v: u32) -> u32 {
    (v / 2).max(1) * 2
}

/// Grafo de filtros: frame rate de saída → escala/enquadramento → rotação → yuv420p, saída em `[v]`.
///
/// A rotação é feita **depois** de reduzir a imagem (é o filtro mais caro em CPU): a escala é
/// calculada na orientação original, com largura e altura trocadas quando a rotação é de 90/270°.
pub(crate) fn filter_graph(p: &VideoProfile, rotation: Rotation) -> String {
    let (w, h) = (p.width, p.height);
    let swap = matches!(rotation, Rotation::R90 | Rotation::R270);
    // Caixa de destino na orientação da imagem antes de rodar.
    let (sw, sh) = if swap { (h, w) } else { (w, h) };
    let rotate = match rotation {
        Rotation::R0 => "",
        Rotation::R90 => ",transpose=clock",
        Rotation::R180 => ",hflip,vflip",
        Rotation::R270 => ",transpose=cclock",
    };
    let fps = format!("fps={}", p.fps);
    let fit = format!("scale={sw}:{sh}:force_original_aspect_ratio=decrease:force_divisible_by=2:flags=bicubic");
    match p.layout {
        Layout::FitBlack => {
            format!("[0:v]{fps},{fit}{rotate},pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black,format=yuv420p[v]")
        }
        Layout::Crop => format!(
            "[0:v]{fps},scale={sw}:{sh}:force_original_aspect_ratio=increase:flags=bicubic,crop={sw}:{sh}{rotate},format=yuv420p[v]"
        ),
        Layout::FitBlur => {
            // Fundo: desfocado em 1/8 da resolução (barato) e ampliado.
            let (bw, bh) = (even(sw / 8), even(sh / 8));
            format!(
                "[0:v]{fps},split=2[bg][fg];\
                 [bg]scale={bw}:{bh}:force_original_aspect_ratio=increase,crop={bw}:{bh}{rotate},boxblur=6:2,scale={w}:{h}[bgb];\
                 [fg]{fit}{rotate}[fgs];\
                 [bgb][fgs]overlay=(W-w)/2:(H-h)/2,format=yuv420p[v]"
            )
        }
    }
}

/// Descodificação por hardware da entrada (cai para software sozinha se falhar).
fn hwaccel() -> &'static [&'static str] {
    if cfg!(windows) {
        &["-hwaccel", "d3d11va"]
    } else if cfg!(target_os = "macos") {
        &["-hwaccel", "videotoolbox"]
    } else {
        &[]
    }
}

/// Linha de comandos completa do FFmpeg para um destino.
pub(crate) fn ffmpeg_args(dest: &Destination, encoder: EncoderKind, rotation: Rotation) -> Vec<String> {
    let p = &dest.video;
    let gop = (p.fps * 2).to_string(); // keyframe a cada 2 s (Twitch/YouTube/TikTok)
    let mut a: Vec<String> = Vec::new();
    let mut push = |items: &[&str]| a.extend(items.iter().map(|s| s.to_string()));
    push(&["-hide_banner", "-nostdin", "-nostats", "-loglevel", "warning", "-progress", "pipe:2", "-stats_period", "1"]);
    push(hwaccel());
    push(&["-f", "matroska", "-i", "pipe:0"]);
    push(&["-filter_complex", &filter_graph(p, rotation), "-map", "[v]", "-map", "0:a:0"]);
    push(&["-c:v", encoder.ffmpeg_name()]);
    a.extend(encoder.args(p.bitrate_kbps));
    let mut push = |items: &[&str]| a.extend(items.iter().map(|s| s.to_string()));
    push(&["-g", &gop, "-keyint_min", &gop, "-force_key_frames", "expr:gte(t,n_forced*2)"]);
    push(&["-af", "aresample=async=1", "-c:a", "aac", "-b:a", &format!("{}k", dest.audio_bitrate_kbps), "-ar", "48000", "-ac", "2"]);
    push(&["-f", "flv", "-flvflags", "no_duration_filesize", &dest.url]);
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_build_expected_urls() {
        assert_eq!(Destination::twitch("live_123_abc").url, "rtmp://live.twitch.tv/app/live_123_abc");
        assert_eq!(Destination::youtube("abcd-efgh").url, "rtmps://a.rtmps.youtube.com/live2/abcd-efgh");
        let t = Destination::tiktok("rtmp://push.tiktokcdn.com/game/", "stream-xyz?expire=1", true);
        assert_eq!(t.url, "rtmp://push.tiktokcdn.com/game/stream-xyz?expire=1");
        assert_eq!((t.video.width, t.video.height), (1080, 1920));
    }

    #[test]
    fn stream_keys_never_leak() {
        let d = Destination::twitch("live_123456_SECRET");
        assert_eq!(d.redacted_url(), "rtmp://live.twitch.tv/app/****");
        assert_eq!(redact_url("rtmp://push.tiktokcdn.com/game/key?sig=abc"), "rtmp://push.tiktokcdn.com/game/****");
        let line = format!("[flv @ 0x1] Failed to connect to {}: I/O error", d.url);
        let redacted = redact_line(&line, &d.url);
        assert!(!redacted.contains("SECRET"), "{redacted}");
        assert!(redact_line("erro com live_123456_SECRET", &d.url).contains("****"));
    }

    #[test]
    fn filters_match_rotation_and_layout() {
        let p = VideoProfile { width: 1080, height: 1920, fps: 60, bitrate_kbps: 6000, layout: Layout::FitBlur };
        let g = filter_graph(&p, Rotation::R90);
        assert!(g.starts_with("[0:v]fps=60,split=2"), "{g}");
        assert!(g.contains("boxblur") && g.contains("overlay=(W-w)/2:(H-h)/2") && g.ends_with("[v]"), "{g}");
        // Escala na orientação original (caixa trocada) e só depois roda: fundo e frente.
        assert!(g.contains("crop=240:134,transpose=clock,boxblur"), "{g}");
        assert!(g.contains("scale=1920:1080:force_original_aspect_ratio=decrease:force_divisible_by=2:flags=bicubic,transpose=clock[fgs]"), "{g}");

        let crop = filter_graph(&VideoProfile { layout: Layout::Crop, ..p.clone() }, Rotation::R0);
        assert_eq!(crop, "[0:v]fps=60,scale=1080:1920:force_original_aspect_ratio=increase:flags=bicubic,crop=1080:1920,format=yuv420p[v]");
        let black = filter_graph(&VideoProfile { layout: Layout::FitBlack, width: 1920, height: 1080, ..p.clone() }, Rotation::R270);
        assert!(black.contains("scale=1080:1920:force_original_aspect_ratio=decrease"), "{black}");
        assert!(black.contains("transpose=cclock,pad=1920:1080"), "{black}");
        assert!(filter_graph(&p, Rotation::R180).contains("hflip,vflip"));
    }

    #[test]
    fn ffmpeg_args_are_complete() {
        let d = Destination::twitch("k");
        let a = ffmpeg_args(&d, EncoderKind::Nvenc, Rotation::R0).join(" ");
        for expected in [
            "-f matroska -i pipe:0",
            "-c:v h264_nvenc",
            "-rc cbr -b:v 6000k -maxrate 6000k -bufsize 12000k",
            "-g 120 -keyint_min 120",
            "-c:a aac -b:a 160k -ar 48000 -ac 2",
            "-progress pipe:2",
            "-f flv -flvflags no_duration_filesize rtmp://live.twitch.tv/app/k",
        ] {
            assert!(a.contains(expected), "falta «{expected}» em: {a}");
        }
    }
}
