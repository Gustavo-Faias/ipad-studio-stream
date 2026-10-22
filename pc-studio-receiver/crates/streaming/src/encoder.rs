//! Escolha do encoder H.264 do PC: NVENC / AMF / Quick Sync / VideoToolbox, ou x264 como último recurso.

use std::path::Path;
use std::process::{Command, Stdio};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EncoderKind {
    Nvenc,
    Amf,
    Qsv,
    VideoToolbox,
    Software,
}

impl EncoderKind {
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::Nvenc => "h264_nvenc",
            Self::Amf => "h264_amf",
            Self::Qsv => "h264_qsv",
            Self::VideoToolbox => "h264_videotoolbox",
            Self::Software => "libx264",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Nvenc => "NVENC (NVIDIA)",
            Self::Amf => "AMF (AMD)",
            Self::Qsv => "Quick Sync (Intel)",
            Self::VideoToolbox => "VideoToolbox (Apple)",
            Self::Software => "x264 (CPU)",
        }
    }

    pub fn is_hardware(self) -> bool {
        self != Self::Software
    }

    /// Opções do encoder para uma live: CBR, qualidade antes de latência (o RTMP já tem segundos de atraso).
    pub(crate) fn args(self, kbps: u32) -> Vec<String> {
        let b = format!("{kbps}k");
        let buf = format!("{}k", kbps * 2);
        let v: &[&str] = match self {
            Self::Nvenc => &[
                "-preset", "p5", "-tune", "hq", "-rc", "cbr", "-b:v", &b, "-maxrate", &b, "-bufsize", &buf, "-bf", "2",
                "-spatial-aq", "1", "-profile:v", "high",
            ],
            Self::Amf => &[
                "-usage", "transcoding", "-quality", "balanced", "-rc", "cbr", "-b:v", &b, "-maxrate", &b, "-bufsize", &buf,
                "-profile:v", "high",
            ],
            Self::Qsv => &["-preset", "medium", "-b:v", &b, "-maxrate", &b, "-bufsize", &buf, "-profile:v", "high"],
            Self::VideoToolbox => &["-b:v", &b, "-maxrate", &b, "-bufsize", &buf, "-profile:v", "high", "-realtime", "1"],
            Self::Software => &[
                "-preset", "veryfast", "-b:v", &b, "-maxrate", &b, "-bufsize", &buf, "-profile:v", "high", "-x264-params",
                "nal-hrd=cbr",
            ],
        };
        v.iter().map(|s| s.to_string()).collect()
    }
}

/// Esconde a janela de consola dos processos filhos no Windows (a app é gráfica).
pub(crate) fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

fn works(ffmpeg: &Path, encoder: EncoderKind) -> bool {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "color=black:s=1280x720:r=60:d=0.2"])
        .args(["-pix_fmt", "yuv420p", "-c:v", encoder.ffmpeg_name(), "-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    no_window(&mut cmd);
    cmd.status().is_ok_and(|s| s.success())
}

/// Encoders de hardware a experimentar, por ordem de preferência.
fn candidates() -> &'static [EncoderKind] {
    if cfg!(target_os = "macos") {
        &[EncoderKind::VideoToolbox]
    } else {
        &[EncoderKind::Nvenc, EncoderKind::Amf, EncoderKind::Qsv]
    }
}

/// Verifica o FFmpeg e escolhe o primeiro encoder de hardware que funciona.
pub fn detect(ffmpeg: &Path) -> Result<EncoderKind, String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-version").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    no_window(&mut cmd);
    cmd.status().map_err(|e| format!("não encontrei o FFmpeg em \"{}\" ({e})", ffmpeg.display()))?;

    for &kind in candidates() {
        if works(ffmpeg, kind) {
            return Ok(kind);
        }
    }
    if works(ffmpeg, EncoderKind::Software) {
        return Ok(EncoderKind::Software);
    }
    Err("o FFmpeg não tem nenhum encoder H.264 utilizável".into())
}
