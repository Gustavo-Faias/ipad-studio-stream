//! Comandos chamados pela UI (`invoke`). Erros voltam como texto pronto a mostrar.

use std::time::Instant;

use padstudio_audio::{InputId, MicSelection, default_input_device, input_devices};
use padstudio_engine::recorder::{RecordedFile, Recorder, RecorderOptions};
use padstudio_protocol::json::StreamConfig;
use serde::Serialize;
use tauri::State;

use crate::preview::PreviewInfo;
use crate::settings::{self, DestinationSettings, EndpointSetting, Settings};
use crate::state::{AppState, Recording, Snapshot};

type Result<T> = std::result::Result<T, String>;

/// Destino como a UI o vê: definições + se há chave no cofre (a chave nunca sai do backend).
#[derive(Debug, Clone, Serialize)]
pub struct DestinationView {
    #[serde(flatten)]
    pub settings: DestinationSettings,
    pub has_key: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsView {
    pub endpoint: EndpointSetting,
    pub ipad: StreamConfig,
    pub mic: MicSelection,
    pub gains_db: [f32; InputId::COUNT],
    pub muted: [bool; InputId::COUNT],
    pub destinations: Vec<DestinationView>,
    pub record_dir: String,
}

fn view(s: &Settings, default_dir: &std::path::Path) -> SettingsView {
    SettingsView {
        endpoint: s.endpoint.clone(),
        ipad: s.ipad.clone(),
        mic: s.mic.clone(),
        gains_db: s.gains_db,
        muted: s.muted,
        destinations: s
            .destinations
            .iter()
            .map(|d| DestinationView { settings: d.clone(), has_key: settings::has_key(&d.id) })
            .collect(),
        record_dir: s.record_dir.as_deref().unwrap_or(default_dir).display().to_string(),
    }
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> SettingsView {
    view(&state.settings.lock().unwrap(), &state.default_record_dir)
}

#[tauri::command]
pub fn get_snapshot(state: State<'_, AppState>) -> Snapshot {
    state.snapshot()
}

#[tauri::command]
pub fn get_preview(state: State<'_, AppState>) -> Result<PreviewInfo> {
    state.preview.clone().ok_or_else(|| state.preview_error.clone().unwrap_or_default())
}

#[tauri::command]
pub fn set_endpoint(state: State<'_, AppState>, endpoint: EndpointSetting) -> Result<()> {
    state.ctrl.set_endpoint(endpoint.to_endpoint());
    state.settings.lock().unwrap().endpoint = endpoint;
    state.save_settings()
}

/// Muda o encoder do iPad (codec, fps, bitrate, resolução máxima).
#[tauri::command]
pub fn set_ipad_config(state: State<'_, AppState>, config: StreamConfig) -> Result<()> {
    state.ctrl.configure(config.clone());
    state.settings.lock().unwrap().ipad.merge(&config);
    state.save_settings()
}

#[tauri::command]
pub fn request_keyframe(state: State<'_, AppState>) {
    state.ctrl.request_keyframe();
}

#[tauri::command]
pub fn set_gain(state: State<'_, AppState>, input: InputId, db: f32) -> Result<()> {
    state.mixer.set_gain_db(input, db);
    state.settings.lock().unwrap().gains_db[input as usize] = state.mixer.gain_db(input);
    state.save_settings()
}

#[tauri::command]
pub fn set_muted(state: State<'_, AppState>, input: InputId, muted: bool) -> Result<()> {
    state.mixer.set_muted(input, muted);
    state.settings.lock().unwrap().muted[input as usize] = muted;
    state.save_settings()
}

#[derive(Debug, Clone, Serialize)]
pub struct MicList {
    pub devices: Vec<String>,
    pub default: Option<String>,
}

#[tauri::command]
pub fn list_mics() -> Result<MicList> {
    Ok(MicList { devices: input_devices()?, default: default_input_device() })
}

#[tauri::command]
pub fn set_mic(state: State<'_, AppState>, selection: MicSelection) -> Result<()> {
    state.mixer.set_mic(selection.clone());
    state.settings.lock().unwrap().mic = selection;
    state.save_settings()
}

/// Guarda as definições de um destino; com `key`, guarda também a chave no cofre.
#[tauri::command]
pub fn save_destination(state: State<'_, AppState>, destination: DestinationSettings, key: Option<String>) -> Result<SettingsView> {
    if let Some(k) = key.as_deref().filter(|k| !k.trim().is_empty()) {
        settings::store_key(&destination.id, k)?;
    }
    {
        let mut s = state.settings.lock().unwrap();
        match s.destinations.iter_mut().find(|d| d.id == destination.id) {
            Some(d) => *d = destination,
            None => s.destinations.push(destination),
        }
    }
    state.save_settings()?;
    Ok(get_settings(state))
}

#[tauri::command]
pub fn delete_stream_key(state: State<'_, AppState>, id: String) -> Result<SettingsView> {
    settings::delete_key(&id)?;
    Ok(get_settings(state))
}

#[tauri::command]
pub fn start_destination(state: State<'_, AppState>, id: String) -> Result<()> {
    let settings = state.settings.lock().unwrap().destination(&id).cloned().ok_or("destino desconhecido")?;
    let key = settings::load_key(&id)?.ok_or_else(|| format!("{}: guarda primeiro a chave de stream", settings.name))?;
    let destination = settings.to_destination(&key)?;
    state.streams()?.start(destination);
    Ok(())
}

#[tauri::command]
pub fn stop_destination(state: State<'_, AppState>, id: String) -> Result<()> {
    state.streams()?.stop(&id);
    Ok(())
}

#[tauri::command]
pub fn start_recording(state: State<'_, AppState>) -> Result<()> {
    let mut rec = state.recording.lock().unwrap();
    if rec.is_some() {
        return Ok(());
    }
    let dir = state.settings.lock().unwrap().record_dir.clone().unwrap_or_else(|| state.default_record_dir.clone());
    let recorder = Recorder::start(&state.ctrl, RecorderOptions::new(&dir))
        .map_err(|e| format!("não foi possível gravar em {}: {e}", dir.display()))?;
    *rec = Some(Recording { recorder, started: Instant::now(), dir });
    Ok(())
}

#[tauri::command]
pub fn stop_recording(state: State<'_, AppState>) -> Result<Vec<RecordedFile>> {
    let Some(r) = state.recording.lock().unwrap().take() else { return Ok(Vec::new()) };
    let files = r.recorder.stop().map_err(|e| format!("erro ao fechar a gravação: {e}"))?;
    let mut last = state.last_recordings.lock().unwrap();
    last.extend(files.iter().cloned());
    let excess = last.len().saturating_sub(10);
    last.drain(..excess);
    Ok(files)
}
