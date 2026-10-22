import type { EngineStatus, OutputState, Thermal } from "./types";

export const ms = (v: number | null | undefined, digits = 1) => (v == null ? "—" : `${v.toFixed(digits)} ms`);

export const mbps = (bps: number) => `${(bps / 1e6).toFixed(1)} Mbps`;

export function duration(s: number): string {
  const t = Math.floor(s);
  const h = Math.floor(t / 3600);
  const m = Math.floor((t % 3600) / 60);
  const sec = t % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(sec).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}

export const THERMAL_LABEL: Record<Thermal, string> = {
  nominal: "Normal",
  fair: "Morno",
  serious: "Quente",
  critical: "Crítico",
  unknown: "—",
};

export const OUTPUT_LABEL: Record<OutputState, string> = {
  waiting_video: "À espera do iPad",
  starting: "A ligar…",
  live: "Ao vivo",
  standby: "Em espera (iPad desligado)",
  reconnecting: "A religar",
  stopped: "Desligado",
};

export function statusTone(status: EngineStatus): "ok" | "warn" | "busy" | "off" {
  switch (status.state) {
    case "streaming":
      return "ok";
    case "connecting":
    case "starting":
      return "busy";
    case "waiting":
      return status.reason === "usbmux_unavailable" || status.reason === "error" ? "warn" : "off";
    default:
      return "off";
  }
}

export function statusText(status: EngineStatus): string {
  switch (status.state) {
    case "streaming":
      return status.target;
    case "connecting":
      return `A ligar a ${status.target}…`;
    case "waiting":
      return status.message;
    case "starting":
      return "A iniciar…";
    case "stopped":
      return "Parado";
  }
}

/// Posição (0–1) de um nível em dBFS num medidor de −60 a 0 dB.
export const meterFraction = (db: number) => Math.max(0, Math.min(1, (db + 60) / 60));
