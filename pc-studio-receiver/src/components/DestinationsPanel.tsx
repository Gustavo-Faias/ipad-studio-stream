import { useState } from "react";
import { api, errorText } from "../lib/api";
import { OUTPUT_LABEL } from "../lib/format";
import type { DestinationView, Layout, OutputStatus, SettingsView } from "../lib/types";
import { Badge, Button, Dot, Field, inputClass, Panel } from "./ui";

type Notify = (text: string, kind?: "info" | "warning") => void;

const RESOLUTIONS = [
  { label: "1080p · 16:9", w: 1920, h: 1080 },
  { label: "720p · 16:9", w: 1280, h: 720 },
  { label: "1080×1920 · 9:16 (vertical)", w: 1080, h: 1920 },
  { label: "720×1280 · 9:16 (vertical)", w: 720, h: 1280 },
];

const LAYOUTS: { value: Layout; label: string }[] = [
  { value: "fit_blur", label: "Ecrã inteiro + fundo desfocado" },
  { value: "fit_black", label: "Ecrã inteiro + barras pretas" },
  { value: "crop", label: "Preencher (corta as margens)" },
];

const SERVER_HINT: Record<string, string> = {
  twitch: "Predefinido: ingest automático da Twitch.",
  youtube: "Predefinido: RTMPS do YouTube.",
  tiktok: "Copia o “URL do servidor” do TikTok LIVE Producer.",
  custom: "rtmp:// ou rtmps:// de qualquer servidor.",
};

function stateTone(o: OutputStatus | undefined): "live" | "busy" | "warn" | "off" {
  if (!o) return "off";
  if (o.state === "live") return "live";
  if (o.state === "reconnecting" || o.state === "standby") return "warn";
  if (o.state === "stopped") return "off";
  return "busy";
}

function DestinationCard({
  dest,
  output,
  ready,
  onSettings,
  notify,
}: {
  dest: DestinationView;
  output: OutputStatus | undefined;
  ready: boolean;
  onSettings: (s: SettingsView) => void;
  notify: Notify;
}) {
  const [open, setOpen] = useState(!dest.has_key && dest.kind !== "youtube");
  const [draft, setDraft] = useState(dest);
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const running = !!output && output.state !== "stopped";
  const tone = stateTone(output);

  const run = async (f: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await f();
    } catch (e) {
      notify(errorText(e), "warning");
    } finally {
      setBusy(false);
    }
  };

  const save = () =>
    run(async () => {
      const s = await api.saveDestination(draft, key || undefined);
      setKey("");
      onSettings(s);
      notify(`${draft.name}: definições guardadas${key ? " (chave no cofre do sistema)" : ""}`);
    });

  const res = RESOLUTIONS.findIndex((r) => r.w === draft.video.width && r.h === draft.video.height);

  return (
    <div className={`rounded-lg border p-3 ${running ? "border-red-500/40 bg-red-500/5" : "border-zinc-800 bg-zinc-950/60"}`}>
      <div className="flex items-center gap-2">
        <Dot tone={tone} />
        <div className="min-w-0 flex-1">
          <div className="font-medium">{dest.name}</div>
          <div className="truncate text-xs text-zinc-500">
            {dest.video.width}×{dest.video.height} · {dest.video.fps} fps · {dest.video.bitrate_kbps} kbps
          </div>
        </div>
        <Button
          variant={running ? "live" : "primary"}
          disabled={busy || !ready || (!running && !dest.has_key)}
          title={!dest.has_key ? "Guarda primeiro a chave de stream" : undefined}
          onClick={() => run(() => (running ? api.stopDestination(dest.id) : api.startDestination(dest.id)))}
        >
          {running ? "Parar" : "Ir em direto"}
        </Button>
      </div>

      {output && output.state !== "stopped" && (
        <div className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-zinc-400">
          <Badge tone={tone}>
            {OUTPUT_LABEL[output.state]}
            {output.retry_in_s ? ` em ${output.retry_in_s} s` : ""}
          </Badge>
          {output.state !== "reconnecting" && (
            <span className="font-mono tabular-nums">
              {output.fps.toFixed(0)} fps · {output.bitrate_kbps.toFixed(0)} kbps ·{" "}
              <span className={output.speed > 0 && output.speed < 0.98 ? "text-amber-300" : ""}>{output.speed.toFixed(2)}x</span>
              {output.drop_frames > 0 && ` · ${output.drop_frames} perdidos`}
            </span>
          )}
        </div>
      )}
      {(output?.last_error || output?.last_message) && output.state !== "live" && (
        <p className="mt-1 text-xs text-amber-300/90">{output.last_error ?? output.last_message}</p>
      )}

      <button className="mt-2 text-xs text-zinc-400 hover:text-zinc-200" onClick={() => setOpen(!open)}>
        {open ? "▾ Definições" : "▸ Definições"}
        {!dest.has_key && <span className="ml-2 text-amber-300">falta a chave</span>}
      </button>

      {open && (
        <div className="mt-2 space-y-3">
          <Field label="Servidor" hint={SERVER_HINT[dest.kind]}>
            <input className={inputClass} value={draft.server} placeholder="rtmp://…" onChange={(e) => setDraft({ ...draft, server: e.target.value })} />
          </Field>
          <Field label="Chave de stream" hint={dest.has_key ? "Guardada no cofre do sistema. Escreve uma nova para a substituir." : "Fica no cofre do sistema, nunca em ficheiros."}>
            <div className="flex gap-2">
              <input
                className={inputClass}
                type="password"
                autoComplete="off"
                value={key}
                placeholder={dest.has_key ? "••••••••••••" : "cola aqui a chave"}
                onChange={(e) => setKey(e.target.value)}
              />
              {dest.has_key && (
                <Button
                  disabled={busy || running}
                  title="Apagar a chave do cofre"
                  onClick={() => run(async () => onSettings(await api.deleteStreamKey(dest.id)))}
                >
                  Apagar
                </Button>
              )}
            </div>
          </Field>
          <div className="grid grid-cols-2 gap-2">
            <Field label="Formato">
              <select
                className={inputClass}
                value={res}
                onChange={(e) => {
                  const r = RESOLUTIONS[Number(e.target.value)];
                  setDraft({ ...draft, video: { ...draft.video, width: r.w, height: r.h } });
                }}
              >
                {res < 0 && <option value={-1}>{draft.video.width}×{draft.video.height}</option>}
                {RESOLUTIONS.map((r, i) => (
                  <option key={r.label} value={i}>
                    {r.label}
                  </option>
                ))}
              </select>
            </Field>
            <Field label="Imagens por segundo">
              <select className={inputClass} value={draft.video.fps} onChange={(e) => setDraft({ ...draft, video: { ...draft.video, fps: Number(e.target.value) } })}>
                <option value={60}>60 fps</option>
                <option value={30}>30 fps</option>
              </select>
            </Field>
            <Field label="Bitrate de vídeo (kbps)">
              <input
                className={inputClass}
                type="number"
                min={1000}
                max={51000}
                step={500}
                value={draft.video.bitrate_kbps}
                onChange={(e) => setDraft({ ...draft, video: { ...draft.video, bitrate_kbps: Number(e.target.value) } })}
              />
            </Field>
            <Field label="Enquadramento">
              <select className={inputClass} value={draft.video.layout} onChange={(e) => setDraft({ ...draft, video: { ...draft.video, layout: e.target.value as Layout } })}>
                {LAYOUTS.map((l) => (
                  <option key={l.value} value={l.value}>
                    {l.label}
                  </option>
                ))}
              </select>
            </Field>
          </div>
          <div className="flex items-center justify-end gap-2">
            {running && <span className="text-xs text-zinc-500">Aplica-se da próxima vez que fores em direto.</span>}
            <Button variant="primary" disabled={busy} onClick={save}>
              Guardar
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

export function DestinationsPanel({
  destinations,
  outputs,
  encoder,
  streamingError,
  onSettings,
  notify,
}: {
  destinations: DestinationView[];
  outputs: OutputStatus[];
  encoder: string | null;
  streamingError: string | null;
  onSettings: (s: SettingsView) => void;
  notify: Notify;
}) {
  const ready = !!encoder;
  return (
    <Panel title="Destinos" action={<span className="text-xs text-zinc-500">{encoder ? `Encoder: ${encoder}` : streamingError ? "" : "A verificar o FFmpeg…"}</span>}>
      {streamingError && <p className="mb-3 rounded-lg bg-red-500/10 p-2 text-sm text-red-300">{streamingError}</p>}
      <div className="space-y-2">
        {destinations.map((d) => (
          <DestinationCard key={`${d.id}-${d.has_key}`} dest={d} output={outputs.find((o) => o.id === d.id)} ready={ready} onSettings={onSettings} notify={notify} />
        ))}
      </div>
    </Panel>
  );
}
