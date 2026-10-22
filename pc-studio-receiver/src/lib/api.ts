// Chamadas ao backend Rust. Os erros chegam como texto pronto a mostrar.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  DestinationSettings,
  EndpointSetting,
  EngineEvent,
  InputId,
  MicSelection,
  PreviewInfo,
  RecordedFile,
  SettingsView,
  Snapshot,
  StreamConfig,
} from "./types";

export const api = {
  getSettings: () => invoke<SettingsView>("get_settings"),
  getSnapshot: () => invoke<Snapshot>("get_snapshot"),
  getPreview: () => invoke<PreviewInfo>("get_preview"),
  setEndpoint: (endpoint: EndpointSetting) => invoke<void>("set_endpoint", { endpoint }),
  setIpadConfig: (config: StreamConfig) => invoke<void>("set_ipad_config", { config }),
  requestKeyframe: () => invoke<void>("request_keyframe"),
  setGain: (input: InputId, db: number) => invoke<void>("set_gain", { input, db }),
  setMuted: (input: InputId, muted: boolean) => invoke<void>("set_muted", { input, muted }),
  listMics: () => invoke<{ devices: string[]; default: string | null }>("list_mics"),
  setMic: (selection: MicSelection) => invoke<void>("set_mic", { selection }),
  saveDestination: (destination: DestinationSettings, key?: string) =>
    invoke<SettingsView>("save_destination", { destination, key: key ?? null }),
  deleteStreamKey: (id: string) => invoke<SettingsView>("delete_stream_key", { id }),
  startDestination: (id: string) => invoke<void>("start_destination", { id }),
  stopDestination: (id: string) => invoke<void>("stop_destination", { id }),
  startRecording: () => invoke<void>("start_recording"),
  stopRecording: () => invoke<RecordedFile[]>("stop_recording"),
};

export const onSnapshot = (cb: (s: Snapshot) => void): Promise<UnlistenFn> =>
  listen<Snapshot>("snapshot", (e) => cb(e.payload));

export const onEngineEvent = (cb: (e: EngineEvent) => void): Promise<UnlistenFn> =>
  listen<EngineEvent>("engine-event", (e) => cb(e.payload));

export function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e);
}
