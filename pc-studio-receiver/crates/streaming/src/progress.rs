//! Leitura do `-progress pipe:2` do FFmpeg (blocos de `chave=valor` terminados em `progress=`).

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Progress {
    pub frame: u64,
    pub fps: f64,
    pub bitrate_kbps: f64,
    pub out_time_us: i64,
    pub dup_frames: u64,
    pub drop_frames: u64,
    pub speed: f64,
    pub ended: bool,
}

#[derive(Default)]
pub(crate) struct ProgressParser {
    cur: Progress,
}

/// Linha do bloco de progresso (`chave=valor`, chave em minúsculas) ou linha de log?
pub(crate) fn is_progress_line(line: &str) -> bool {
    line.split_once('=')
        .is_some_and(|(k, _)| !k.is_empty() && k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
}

fn number(v: &str) -> f64 {
    let v = v.trim().trim_end_matches("kbits/s").trim_end_matches('x');
    v.parse().unwrap_or(0.0)
}

impl ProgressParser {
    /// Devolve uma fotografia completa quando chega a linha `progress=`.
    pub fn line(&mut self, line: &str) -> Option<Progress> {
        let (key, value) = line.trim().split_once('=')?;
        match key {
            "frame" => self.cur.frame = value.trim().parse().unwrap_or(0),
            "fps" => self.cur.fps = number(value),
            "bitrate" => self.cur.bitrate_kbps = number(value),
            "out_time_us" => self.cur.out_time_us = value.trim().parse().unwrap_or(0),
            "dup_frames" => self.cur.dup_frames = value.trim().parse().unwrap_or(0),
            "drop_frames" => self.cur.drop_frames = value.trim().parse().unwrap_or(0),
            "speed" => self.cur.speed = number(value),
            "progress" => {
                self.cur.ended = value.trim() == "end";
                return Some(self.cur.clone());
            }
            _ => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_progress_block() {
        let block = "frame=123\nfps=59.94\nstream_0_0_q=23.0\nbitrate=6012.3kbits/s\ntotal_size=1500000\nout_time_us=2040000\n\
                     out_time=00:00:02.040000\ndup_frames=2\ndrop_frames=1\nspeed=1.01x\nprogress=continue";
        let mut p = ProgressParser::default();
        let snaps: Vec<Progress> = block.lines().filter_map(|l| p.line(l)).collect();
        assert_eq!(snaps.len(), 1);
        let s = &snaps[0];
        assert_eq!((s.frame, s.dup_frames, s.drop_frames, s.out_time_us), (123, 2, 1, 2_040_000));
        assert!((s.fps - 59.94).abs() < 1e-9 && (s.bitrate_kbps - 6012.3).abs() < 1e-9 && (s.speed - 1.01).abs() < 1e-9);
        assert!(!s.ended);
        assert!(p.line("speed=N/A").is_none());
        assert_eq!(p.line("progress=end").map(|s| (s.ended, s.speed)), Some((true, 0.0)));
    }

    #[test]
    fn separates_progress_from_log_lines() {
        assert!(is_progress_line("out_time_us=123"));
        assert!(!is_progress_line("[flv @ 000001] Failed to update header with correct duration."));
        assert!(!is_progress_line("Error opening output rtmp://x: I/O error"));
        assert!(!is_progress_line("Stream #0:0 -> #0:0 (h264 -> h264_amf)"));
    }
}
