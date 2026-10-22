//! Shell Tauri do PadStudio: liga o motor, o mixer, o streaming e o gravador à UI React.
//!
//! - Comandos (`invoke`): ver `commands.rs`.
//! - Eventos: `snapshot` (~10 Hz, estado completo) e `engine-event` (mensagens do motor).
//! - Preview: WebSocket local com token (ver `preview.rs`).

mod commands;
mod preview;
mod settings;
mod state;

use std::thread;
use std::time::Duration;

use tauri::{Emitter, Manager, RunEvent};

use crate::state::AppState;

const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(100);

pub fn run() {
    let app = tauri::Builder::default()
        .setup(|app| {
            let config_dir = app.path().app_config_dir()?;
            let video_dir = app.path().video_dir().unwrap_or_else(|_| config_dir.clone());
            app.manage(AppState::new(config_dir.join("settings.json"), video_dir.join("PadStudio")));

            // Deteção do FFmpeg/encoder sem atrasar a janela.
            let handle = app.handle().clone();
            thread::spawn(move || handle.state::<AppState>().init_streaming());

            // Estado para a UI e mensagens do motor.
            let handle = app.handle().clone();
            thread::Builder::new().name("padstudio-ui-pump".into()).spawn(move || {
                loop {
                    thread::sleep(SNAPSHOT_INTERVAL);
                    let state = handle.state::<AppState>();
                    while let Ok(ev) = state.engine.events().try_recv() {
                        let _ = handle.emit("engine-event", ev);
                    }
                    if handle.emit("snapshot", state.snapshot()).is_err() {
                        break;
                    }
                }
            })?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::get_snapshot,
            commands::get_preview,
            commands::set_endpoint,
            commands::set_ipad_config,
            commands::request_keyframe,
            commands::set_gain,
            commands::set_muted,
            commands::list_mics,
            commands::set_mic,
            commands::save_destination,
            commands::delete_stream_key,
            commands::start_destination,
            commands::stop_destination,
            commands::start_recording,
            commands::stop_recording,
        ])
        .build(tauri::generate_context!())
        .expect("erro ao iniciar a aplicação Tauri");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            // Fecha gravação e lives com ordem (o FFmpeg fecha a ligação RTMP corretamente).
            handle.state::<AppState>().shutdown();
        }
    });
}
