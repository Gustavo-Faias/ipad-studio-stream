// Tipos espelhados do backend Rust (serde). Manter em sincronia com src-tauri e crates/*.

export type WaitReason =
  | "no_device"
  | "broadcast_inactive"
  | "usbmux_unavailable"
  | "unreachable"
  | "session_ended"
  | "error";

export type EngineStatus =
  | { state: "starting" }
  | { state: "waiting"; reason: WaitReason; message: string; device: string | null }
  | { state: "connecting"; target: string }
  | { state: "streaming"; target: string; device: string | null }
  | { state: "stopped" };

export type EngineEvent =
  | { kind: "status"; data: EngineStatus }
  | { kind: "info"; data: string }
  | { kind: "warning"; data: string };

export type Thermal = "nominal" | "fair" | "serious" | "critical" | "unknown";

export interface SenderStats {
  fps_capture?: number;
  fps_encoded?: number;
  bitrate_bps?: number;
  target_bitrate_bps?: number;
  frames_dropped?: number;
  keyframes?: number;
  send_queue_bytes?: number;
  send_queue_ms?: number;
  encode_ms_avg?: number;
  thermal?: Thermal;
  memory_mb?: number;
  low_power_mode?: boolean;
  /** Diagnóstico dos spikes do passo 4 (só a extensão Swift o envia). */
  diag?: SenderDiagnostics;
}

/** Intervalos entre frames do ReplayKit por faixa (ms): 120 Hz ≈ 8,3 ms, 60 Hz ≈ 16,7 ms. */
export interface IntervalHistogram {
  lt6: number;
  "6to10": number;
  "10to14": number;
  "14to20": number;
  "20to40": number;
  ge40: number;
}

export interface SenderDiagnostics {
  capture?: {
    fps?: number;
    interval_ms_p50?: number;
    interval_ms_p95?: number;
    interval_ms_max?: number;
    intervals?: IntervalHistogram;
    lag_ms_p50?: number;
    lag_ms_max?: number;
    repeats?: number;
    width?: number;
    height?: number;
    pixel_format?: string;
    orientation?: number;
    rotation?: number;
  };
  encode?: {
    codec?: string;
    input_width?: number;
    input_height?: number;
    output_width?: number;
    output_height?: number;
    scaled?: boolean;
    ms_p50?: number;
    ms_p95?: number;
    ms_max?: number;
    in_flight_max?: number;
    errors?: number;
    encoder_drops?: number;
    sessions?: number;
  };
  memory?: {
    footprint_mb?: number;
    peak_mb?: number;
    available_mb?: number;
    limit_mb?: number;
  };
  cpu_percent?: number;
  notes?: string[];
}

export interface Telemetry {
  connected: boolean;
  target: string | null;
  device_name: string | null;
  device_system: string | null;
  codec: string | null;
  width: number;
  height: number;
  fps_max: number;
  rotation_deg: number;
  fps: number;
  link_bitrate_bps: number;
  video_bitrate_bps: number;
  audio_packets_per_s: number;
  latency_total_p50_ms: number | null;
  latency_total_p95_ms: number | null;
  latency_sender_p50_ms: number | null;
  latency_network_p50_ms: number | null;
  rtt_ms: number | null;
  clock_offset_ms: number | null;
  frames_total: number;
  keyframes_total: number;
  frames_lost: number;
  discontinuities: number;
  sender: SenderStats | null;
  session_uptime_s: number;
}

export type InputId = "ipad_app" | "ipad_mic" | "pc_mic";

export interface InputLevel {
  active: boolean;
  peak_db: number;
  gain_db: number;
  muted: boolean;
  buffered_ms: number;
  drift_ppm: number;
  late_frames: number;
  resyncs: number;
}

export interface MicInfo {
  device: string;
  sample_rate: number;
  channels: number;
  format: string;
}

export interface MixerLevels {
  ipad_app: InputLevel;
  ipad_mic: InputLevel;
  pc_mic: InputLevel;
  master_peak_db: number;
  blocks: number;
  mic: MicInfo | null;
  mic_error: string | null;
  mic_xruns: number;
}

export type OutputState = "waiting_video" | "starting" | "live" | "standby" | "reconnecting" | "stopped";

export interface OutputStatus {
  id: string;
  name: string;
  target: string;
  encoder: string;
  width: number;
  height: number;
  fps_target: number;
  bitrate_target_kbps: number;
  state: OutputState;
  fps: number;
  bitrate_kbps: number;
  speed: number;
  dup_frames: number;
  drop_frames: number;
  out_time_s: number;
  restarts: number;
  retry_in_s: number | null;
  silence_blocks: number;
  last_message: string | null;
  last_error: string | null;
}

export interface RecordedFile {
  path: string;
  video_frames: number;
  audio_blocks: number;
  duration_s: number;
  bytes: number;
}

export interface Snapshot {
  status: EngineStatus;
  telemetry: Telemetry;
  mixer: MixerLevels;
  outputs: OutputStatus[];
  encoder: string | null;
  streaming_error: string | null;
  recording: { elapsed_s: number; dir: string } | null;
  last_recordings: RecordedFile[];
}

export type Layout = "fit_blur" | "fit_black" | "crop";

export interface VideoProfile {
  width: number;
  height: number;
  fps: number;
  bitrate_kbps: number;
  layout: Layout;
}

export type DestinationKind = "twitch" | "youtube" | "tiktok" | "custom";

export interface DestinationSettings {
  id: string;
  kind: DestinationKind;
  name: string;
  server: string;
  video: VideoProfile;
  audio_bitrate_kbps: number;
}

export interface DestinationView extends DestinationSettings {
  has_key: boolean;
}

export type Codec = "h264" | "hevc";

export interface StreamConfig {
  codec?: Codec;
  max_fps?: number;
  min_fps?: number;
  bitrate_bps?: number;
  bitrate_mode?: "vbr" | "cbr";
  max_width?: number;
  max_height?: number;
  keyframe_interval_s?: number;
  audio_app?: boolean;
  audio_mic?: boolean;
}

export interface EndpointSetting {
  kind: "usb" | "tcp";
  addr: string;
  token: string;
}

export type MicSelection = { kind: "default" } | { kind: "named"; name: string } | { kind: "off" };

export interface SettingsView {
  endpoint: EndpointSetting;
  ipad: StreamConfig;
  mic: MicSelection;
  gains_db: [number, number, number];
  muted: [boolean, boolean, boolean];
  destinations: DestinationView[];
  record_dir: string;
}

export interface PreviewInfo {
  port: number;
  token: string;
}

export const INPUT_INDEX: Record<InputId, 0 | 1 | 2> = { ipad_app: 0, ipad_mic: 1, pc_mic: 2 };
