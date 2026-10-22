import { mbps, ms, THERMAL_LABEL } from "../lib/format";
import type { PreviewStats } from "../lib/preview";
import type { Snapshot } from "../lib/types";

function Tile({ label, value, sub, tone = "normal" }: { label: string; value: string; sub?: string; tone?: "normal" | "warn" | "bad" }) {
  const color = tone === "bad" ? "text-red-300" : tone === "warn" ? "text-amber-300" : "text-zinc-100";
  return (
    <div className="min-w-0 rounded-lg border border-zinc-800 bg-zinc-900/80 px-3 py-2">
      <div className="truncate text-[11px] uppercase tracking-wide text-zinc-500">{label}</div>
      <div className={`font-mono text-lg tabular-nums ${color}`}>{value}</div>
      {sub && <div className="truncate text-[11px] text-zinc-500">{sub}</div>}
    </div>
  );
}

export function TelemetryBar({ snap, preview }: { snap: Snapshot | null; preview: PreviewStats | null }) {
  const t = snap?.telemetry;
  const on = !!t?.connected;
  const thermal = t?.sender?.thermal;
  return (
    <div className="grid grid-cols-3 gap-2 xl:grid-cols-6">
      <Tile
        label="FPS recebidos"
        value={on ? t!.fps.toFixed(0) : "—"}
        sub={on ? `máx. ${t!.fps_max} · preview ${preview?.fps ?? 0}` : undefined}
      />
      <Tile label="Bitrate USB" value={on ? mbps(t!.link_bitrate_bps) : "—"} sub={on ? `vídeo ${mbps(t!.video_bitrate_bps)}` : undefined} />
      <Tile
        label="Latência"
        value={on ? ms(t!.latency_total_p50_ms) : "—"}
        sub={on ? `iPad ${ms(t!.latency_sender_p50_ms)} · cabo ${ms(t!.latency_network_p50_ms)}` : undefined}
        tone={on && (t!.latency_total_p50_ms ?? 0) > 50 ? "warn" : "normal"}
      />
      <Tile label="Frames perdidos" value={on ? String(t!.frames_lost) : "—"} sub={on ? `${t!.keyframes_total} keyframes` : undefined} tone={on && t!.frames_lost > 0 ? "warn" : "normal"} />
      <Tile label="RTT" value={on ? ms(t!.rtt_ms, 2) : "—"} sub={on && preview ? `decode ${preview.decodeMs.toFixed(1)} ms` : undefined} />
      <Tile
        label="Temperatura iPad"
        value={on && thermal ? THERMAL_LABEL[thermal] : "—"}
        sub={on && t!.sender?.send_queue_ms != null ? `fila ${t!.sender.send_queue_ms.toFixed(1)} ms` : undefined}
        tone={thermal === "critical" ? "bad" : thermal === "serious" ? "warn" : "normal"}
      />
    </div>
  );
}
