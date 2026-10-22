import type { ReactNode } from "react";
import type { IntervalHistogram, SenderStats } from "../lib/types";
import { Badge, Panel } from "./ui";

// Diagnóstico dos spikes do passo 4, enviado pela extensão do iPad em SENDER_STATS.diag:
// S1 ritmo de captura do ReplayKit · S2 custo do encoder · S6 memória da extensão.

const ms = (v?: number) => (v == null ? "—" : `${v.toFixed(1)} ms`);

const BUCKETS: { key: keyof IntervalHistogram; label: string; tone: string }[] = [
  { key: "lt6", label: "< 6 ms", tone: "bg-sky-500" },
  { key: "6to10", label: "6–10 ms (120 Hz)", tone: "bg-emerald-500" },
  { key: "10to14", label: "10–14 ms", tone: "bg-lime-500" },
  { key: "14to20", label: "14–20 ms (60 Hz)", tone: "bg-amber-500" },
  { key: "20to40", label: "20–40 ms", tone: "bg-orange-500" },
  { key: "ge40", label: "≥ 40 ms", tone: "bg-red-500" },
];

function Histogram({ h }: { h: IntervalHistogram }) {
  const total = BUCKETS.reduce((n, b) => n + h[b.key], 0);
  if (total === 0) return <p className="text-xs text-zinc-500">Ecrã parado: o ReplayKit não entregou frames nesta janela.</p>;
  return (
    <div className="space-y-1">
      <div className="flex h-2.5 overflow-hidden rounded-full bg-zinc-800">
        {BUCKETS.map((b) => (h[b.key] > 0 ? <div key={b.key} className={b.tone} style={{ width: `${(100 * h[b.key]) / total}%` }} /> : null))}
      </div>
      <div className="flex flex-wrap gap-x-3 text-[11px] text-zinc-400">
        {BUCKETS.filter((b) => h[b.key] > 0).map((b) => (
          <span key={b.key} className="inline-flex items-center gap-1">
            <span className={`size-2 rounded-sm ${b.tone}`} />
            {b.label}: {h[b.key]}
          </span>
        ))}
      </div>
    </div>
  );
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <>
      <dt className="text-zinc-500">{label}</dt>
      <dd className="tabular-nums">{children}</dd>
    </>
  );
}

export function DiagnosticsPanel({ sender }: { sender: SenderStats | null | undefined }) {
  const d = sender?.diag;
  if (!d) {
    return (
      <Panel title="Diagnóstico do iPad">
        <p className="text-sm text-zinc-500">Sem diagnóstico: o emissor ligado não o envia (o fake-sender em Rust, por exemplo).</p>
      </Panel>
    );
  }
  const { capture: c, encode: e, memory: m } = d;
  const memPct = m?.footprint_mb != null && m.limit_mb ? (100 * m.footprint_mb) / m.limit_mb : null;
  const memTone = memPct == null ? "bg-zinc-600" : memPct > 85 ? "bg-red-500" : memPct > 65 ? "bg-amber-500" : "bg-emerald-500";

  return (
    <Panel
      title="Diagnóstico do iPad"
      action={
        <div className="flex gap-1.5">
          {sender?.thermal && <Badge tone={sender.thermal === "nominal" ? "ok" : "warn"}>térmico: {sender.thermal}</Badge>}
          {sender?.low_power_mode && <Badge tone="warn">poupança de energia</Badge>}
        </div>
      }
    >
      <div className="space-y-4 text-sm">
        {c && (
          <section className="space-y-2">
            <h3 className="text-xs font-medium text-zinc-300">S1 · Captura do ReplayKit</h3>
            <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-0.5">
              <Row label="Ritmo">{c.fps != null ? `${c.fps.toFixed(1)} fps` : "—"}</Row>
              <Row label="Intervalo">
                p50 {ms(c.interval_ms_p50)} · p95 {ms(c.interval_ms_p95)} · máx {ms(c.interval_ms_max)}
              </Row>
              <Row label="PTS → callback">
                p50 {ms(c.lag_ms_p50)} · máx {ms(c.lag_ms_max)}
              </Row>
              <Row label="Imagem">
                {c.width}×{c.height} {c.pixel_format ?? ""}
                {c.orientation != null && ` · EXIF ${c.orientation} → ${c.rotation ?? 0}°`}
              </Row>
              {(c.repeats ?? 0) > 0 && <Row label="Ecrã parado">{c.repeats} frames repetidos</Row>}
            </dl>
            {c.intervals && <Histogram h={c.intervals} />}
          </section>
        )}

        {e && (
          <section className="space-y-2">
            <h3 className="text-xs font-medium text-zinc-300">S2 · Encoder VideoToolbox</h3>
            <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-0.5">
              <Row label="Saída">
                {e.codec?.toUpperCase()} {e.output_width}×{e.output_height}{" "}
                {e.scaled ? `(escalado de ${e.input_width}×${e.input_height})` : "(nativo)"}
              </Row>
              <Row label="Tempo por frame">
                p50 {ms(e.ms_p50)} · p95 {ms(e.ms_p95)} · máx {ms(e.ms_max)}
              </Row>
              <Row label="Em voo / erros">
                {e.in_flight_max ?? 0} · {e.errors ?? 0} erros · {e.encoder_drops ?? 0} descartados pelo VT · {e.sessions ?? 0} sessões
              </Row>
              {d.cpu_percent != null && <Row label="CPU da extensão">{d.cpu_percent.toFixed(0)} %</Row>}
            </dl>
          </section>
        )}

        {m && (
          <section className="space-y-2">
            <h3 className="text-xs font-medium text-zinc-300">S6 · Memória da extensão</h3>
            <div className="flex h-2.5 overflow-hidden rounded-full bg-zinc-800">
              <div className={memTone} style={{ width: `${Math.min(100, memPct ?? 0)}%` }} />
            </div>
            <p className="text-xs tabular-nums text-zinc-400">
              {m.footprint_mb?.toFixed(1) ?? "—"} MB de {m.limit_mb?.toFixed(0) ?? "?"} MB · pico {m.peak_mb?.toFixed(1) ?? "—"} MB · margem{" "}
              {m.available_mb?.toFixed(1) ?? "—"} MB
            </p>
          </section>
        )}

        {d.notes && d.notes.length > 0 && (
          <ul className="list-inside list-disc text-xs text-amber-300/90">
            {d.notes.map((n) => (
              <li key={n}>{n}</li>
            ))}
          </ul>
        )}
      </div>
    </Panel>
  );
}
