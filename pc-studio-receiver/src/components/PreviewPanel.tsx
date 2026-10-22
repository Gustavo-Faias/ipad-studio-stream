import { useEffect, useRef, useState } from "react";
import { api, errorText } from "../lib/api";
import { statusText } from "../lib/format";
import { PreviewPlayer, type PreviewStats } from "../lib/preview";
import type { Snapshot } from "../lib/types";

export function PreviewPanel({ snap, onStats }: { snap: Snapshot | null; onStats: (s: PreviewStats) => void }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [stats, setStats] = useState<PreviewStats | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let player: PreviewPlayer | null = null;
    let cancelled = false;
    api
      .getPreview()
      .then((p) => {
        if (cancelled || !canvasRef.current) return;
        player = new PreviewPlayer(canvasRef.current, `ws://127.0.0.1:${p.port}/?token=${p.token}`, (s) => {
          setStats(s);
          onStats(s);
        });
      })
      .catch((e) => setError(errorText(e)));
    return () => {
      cancelled = true;
      player?.destroy();
    };
  }, [onStats]);

  const t = snap?.telemetry;
  const streaming = snap?.status.state === "streaming";
  const showVideo = streaming && stats?.hasVideo;
  const message = error ?? stats?.error ?? (snap ? (streaming ? "À espera do primeiro keyframe…" : statusText(snap.status)) : "A iniciar…");

  return (
    <div className="relative min-h-0 flex-1 overflow-hidden rounded-xl border border-zinc-800 bg-black">
      <canvas ref={canvasRef} className={`absolute inset-0 size-full ${showVideo ? "" : "opacity-0"}`} />
      {!showVideo && (
        <div className="absolute inset-0 grid place-items-center p-8 text-center">
          <div className="max-w-md">
            <div className="mx-auto mb-4 grid size-14 place-items-center rounded-2xl border border-zinc-800 text-2xl text-zinc-500">▭</div>
            <p className="text-zinc-300">{message}</p>
            {snap?.status.state === "waiting" && snap.status.reason === "no_device" && (
              <p className="mt-2 text-sm text-zinc-500">Liga o cabo USB-C, desbloqueia o iPad e aceita “Confiar neste computador”.</p>
            )}
          </div>
        </div>
      )}
      {showVideo && t && stats && (
        <div className="pointer-events-none absolute left-3 top-3 flex gap-2 text-xs">
          <span className="rounded-md bg-black/60 px-2 py-1 font-mono text-zinc-200">
            {t.codec?.toUpperCase()} {t.width}×{t.height}
            {t.rotation_deg ? ` · ${t.rotation_deg}°` : ""}
          </span>
          <span className="rounded-md bg-black/60 px-2 py-1 font-mono text-zinc-200">
            preview {stats.fps} fps · decode {stats.decodeMs.toFixed(1)} ms
          </span>
        </div>
      )}
    </div>
  );
}
