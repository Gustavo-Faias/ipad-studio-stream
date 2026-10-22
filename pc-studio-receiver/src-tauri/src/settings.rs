//! Definições persistentes (JSON na pasta de configuração da app) e chaves de stream no cofre do
//! sistema operativo (Gestor de Credenciais do Windows / Keychain do macOS).
//!
//! O ficheiro de definições **nunca** contém chaves: só o servidor e o perfil de cada destino.

use std::fs;
use std::path::{Path, PathBuf};

use padstudio_audio::{InputId, MicSelection};
use padstudio_engine::Endpoint;
use padstudio_protocol::json::StreamConfig;
use padstudio_streaming::{Destination, Layout, VideoProfile};
use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "PadStudio";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKind {
    Usb,
    Tcp,
}

/// Como chegar ao iPad.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EndpointSetting {
    pub kind: EndpointKind,
    /// `host:porta` (rede local ou fake-sender).
    pub addr: String,
    /// PIN da rede local.
    pub token: String,
}

impl Default for EndpointSetting {
    fn default() -> Self {
        Self { kind: EndpointKind::Usb, addr: "127.0.0.1:23456".into(), token: String::new() }
    }
}

impl EndpointSetting {
    pub fn to_endpoint(&self) -> Endpoint {
        match self.kind {
            EndpointKind::Usb => Endpoint::usb(),
            EndpointKind::Tcp => Endpoint::Tcp {
                addr: self.addr.trim().to_string(),
                token: (!self.token.trim().is_empty()).then(|| self.token.trim().to_string()),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationKind {
    Twitch,
    Youtube,
    Tiktok,
    Custom,
}

/// Um destino sem a chave.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DestinationSettings {
    pub id: String,
    pub kind: DestinationKind,
    pub name: String,
    /// Servidor RTMP(S) sem a chave (ex.: `rtmp://live.twitch.tv/app`).
    pub server: String,
    pub video: VideoProfile,
    pub audio_bitrate_kbps: u32,
}

impl DestinationSettings {
    fn preset(kind: DestinationKind) -> Self {
        let (id, name, server, width, height, kbps) = match kind {
            DestinationKind::Twitch => ("twitch", "Twitch", "rtmp://live.twitch.tv/app", 1920, 1080, 6000),
            DestinationKind::Youtube => ("youtube", "YouTube", "rtmps://a.rtmps.youtube.com/live2", 1920, 1080, 12000),
            DestinationKind::Tiktok => ("tiktok", "TikTok", "", 1080, 1920, 6000),
            DestinationKind::Custom => ("custom", "RTMP personalizado", "", 1920, 1080, 6000),
        };
        Self {
            id: id.into(),
            kind,
            name: name.into(),
            server: server.into(),
            video: VideoProfile { width, height, fps: 60, bitrate_kbps: kbps, layout: Layout::FitBlur },
            audio_bitrate_kbps: 160,
        }
    }

    /// Destino completo (com a chave do cofre) para o `StreamManager`.
    pub fn to_destination(&self, key: &str) -> Result<Destination, String> {
        let server = self.server.trim();
        if server.is_empty() {
            return Err(format!("{}: falta o endereço do servidor RTMP", self.name));
        }
        if !(server.starts_with("rtmp://") || server.starts_with("rtmps://")) {
            return Err(format!("{}: o servidor tem de começar por rtmp:// ou rtmps://", self.name));
        }
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("{}: falta a chave de stream", self.name));
        }
        Ok(Destination {
            id: self.id.clone(),
            name: self.name.clone(),
            url: format!("{}/{key}", server.trim_end_matches('/')),
            video: self.video.clone(),
            audio_bitrate_kbps: self.audio_bitrate_kbps,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub endpoint: EndpointSetting,
    /// Configuração pedida ao encoder do iPad.
    pub ipad: StreamConfig,
    pub mic: MicSelection,
    pub gains_db: [f32; InputId::COUNT],
    pub muted: [bool; InputId::COUNT],
    pub destinations: Vec<DestinationSettings>,
    pub record_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            endpoint: EndpointSetting::default(),
            ipad: StreamConfig::default(),
            // Desligado até o utilizador escolher: não abrir o microfone sem pedir.
            mic: MicSelection::Off,
            gains_db: [0.0; InputId::COUNT],
            muted: [false; InputId::COUNT],
            destinations: [DestinationKind::Twitch, DestinationKind::Youtube, DestinationKind::Tiktok]
                .into_iter()
                .map(DestinationSettings::preset)
                .collect(),
            record_dir: None,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // Escreve num temporário e troca, para nunca deixar um ficheiro a meio.
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text).map_err(|e| e.to_string())?;
        fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    pub fn destination(&self, id: &str) -> Option<&DestinationSettings> {
        self.destinations.iter().find(|d| d.id == id)
    }
}

// ── Cofre ────────────────────────────────────────────────────────────────────

fn entry(id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, &format!("stream-key:{id}")).map_err(|e| format!("cofre indisponível: {e}"))
}

pub fn store_key(id: &str, key: &str) -> Result<(), String> {
    entry(id)?.set_password(key.trim()).map_err(|e| format!("não foi possível guardar a chave no cofre: {e}"))
}

/// `Ok(None)` se não houver chave guardada.
pub fn load_key(id: &str) -> Result<Option<String>, String> {
    match entry(id)?.get_password() {
        Ok(k) => Ok(Some(k)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("não foi possível ler a chave do cofre: {e}")),
    }
}

pub fn delete_key(id: &str) -> Result<(), String> {
    match entry(id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("não foi possível apagar a chave: {e}")),
    }
}

pub fn has_key(id: &str) -> bool {
    matches!(load_key(id), Ok(Some(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_url_and_validation() {
        let d = DestinationSettings::preset(DestinationKind::Twitch);
        assert_eq!(d.to_destination(" live_123 ").unwrap().url, "rtmp://live.twitch.tv/app/live_123");
        assert!(d.to_destination("").is_err());
        let t = DestinationSettings::preset(DestinationKind::Tiktok);
        assert!(t.to_destination("k").unwrap_err().contains("servidor"));
        let mut c = DestinationSettings::preset(DestinationKind::Custom);
        c.server = "http://x".into();
        assert!(c.to_destination("k").unwrap_err().contains("rtmp://"));
    }

    /// Usa o cofre real do sistema (cria e apaga uma entrada de teste): `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn os_keychain_roundtrip() {
        let id = format!("teste-{}", std::process::id());
        assert_eq!(load_key(&id).unwrap(), None);
        store_key(&id, " segredo-123 ").unwrap();
        assert_eq!(load_key(&id).unwrap().as_deref(), Some("segredo-123"));
        assert!(has_key(&id));
        delete_key(&id).unwrap();
        assert_eq!(load_key(&id).unwrap(), None);
        delete_key(&id).unwrap(); // apagar o que não existe não é erro
    }

    #[test]
    fn settings_roundtrip_without_secrets() {
        let dir = std::env::temp_dir().join(format!("padstudio-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut s = Settings::default();
        s.gains_db[0] = -6.0;
        s.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.to_lowercase().contains("key\""), "o ficheiro não guarda chaves");
        assert_eq!(Settings::load(&path), s);
        let _ = fs::remove_dir_all(dir);
    }
}
