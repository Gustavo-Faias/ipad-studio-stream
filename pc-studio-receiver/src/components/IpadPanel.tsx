import { useState } from "react";
import { api, errorText } from "../lib/api";
import type { Codec, EndpointSetting, SettingsView, Snapshot } from "../lib/types";
import { DiagnosticsPanel } from "./DiagnosticsPanel";
import { Button, Field, inputClass, Panel } from "./ui";

type Notify = (text: string, kind?: "info" | "warning") => void;

const MAX_SIZES = [
  { label: "Nativa (recomendado com HEVC)", w: 0, h: 0 },
  { label: "Até 1920×1440", w: 1920, h: 1440 },
  { label: "Até 1280×960", w: 1280, h: 960 },
];

export function IpadPanel({
  settings,
  snap,
  onSettings,
  notify,
}: {
  settings: SettingsView;
  snap: Snapshot | null;
  onSettings: (s: SettingsView) => void;
  notify: Notify;
}) {
  const [endpoint, setEndpoint] = useState<EndpointSetting>(settings.endpoint);
  const [codec, setCodec] = useState<Codec>(settings.ipad.codec ?? "h264");
  const [fps, setFps] = useState(settings.ipad.max_fps ?? 120);
  const [mbps, setMbps] = useState(Math.round((settings.ipad.bitrate_bps ?? 50_000_000) / 1e6));
  const sizeIndex = MAX_SIZES.findIndex((s) => s.w === (settings.ipad.max_width ?? 1920) && s.h === (settings.ipad.max_height ?? 1440));
  const [size, setSize] = useState(sizeIndex < 0 ? 1 : sizeIndex);

  const t = snap?.telemetry;
  const sender = t?.sender;

  const applyEndpoint = () =>
    api
      .setEndpoint(endpoint)
      .then(() => {
        onSettings({ ...settings, endpoint });
        notify(endpoint.kind === "usb" ? "A procurar o iPad por cabo…" : `A ligar a ${endpoint.addr}…`);
      })
      .catch((e) => notify(errorText(e), "warning"));

  const applyEncoder = () => {
    const s = MAX_SIZES[size];
    const config = { codec, max_fps: fps, bitrate_bps: mbps * 1_000_000, max_width: s.w, max_height: s.h };
    api
      .setIpadConfig(config)
      .then(() => {
        onSettings({ ...settings, ipad: { ...settings.ipad, ...config } });
        notify("Configuração enviada ao iPad");
      })
      .catch((e) => notify(errorText(e), "warning"));
  };

  return (
    <div className="space-y-4">
      <Panel title="Ligação ao iPad">
        <div className="space-y-3">
          <div className="grid grid-cols-2 gap-1 rounded-lg bg-zinc-950 p-1">
            {(["usb", "tcp"] as const).map((k) => (
              <button
                key={k}
                className={`rounded-md py-1.5 text-sm ${endpoint.kind === k ? "bg-zinc-800 text-white" : "text-zinc-400 hover:text-zinc-200"}`}
                onClick={() => setEndpoint({ ...endpoint, kind: k })}
              >
                {k === "usb" ? "Cabo USB" : "Rede / endereço"}
              </button>
            ))}
          </div>
          {endpoint.kind === "tcp" && (
            <div className="grid grid-cols-[1fr_7rem] gap-2">
              <Field label="Endereço (IP:porta)">
                <input className={inputClass} value={endpoint.addr} onChange={(e) => setEndpoint({ ...endpoint, addr: e.target.value })} />
              </Field>
              <Field label="PIN">
                <input className={inputClass} value={endpoint.token} inputMode="numeric" onChange={(e) => setEndpoint({ ...endpoint, token: e.target.value })} />
              </Field>
            </div>
          )}
          <div className="flex justify-end">
            <Button variant="primary" onClick={applyEndpoint}>
              Aplicar
            </Button>
          </div>
        </div>
      </Panel>

      <Panel title="Encoder do iPad">
        <div className="space-y-3">
          <div className="grid grid-cols-2 gap-2">
            <Field label="Codec">
              <select className={inputClass} value={codec} onChange={(e) => setCodec(e.target.value as Codec)}>
                <option value="h264">H.264</option>
                <option value="hevc">HEVC (H.265)</option>
              </select>
            </Field>
            <Field label="Imagens por segundo">
              <select className={inputClass} value={fps} onChange={(e) => setFps(Number(e.target.value))}>
                <option value={120}>120 fps (ProMotion)</option>
                <option value={60}>60 fps</option>
                <option value={30}>30 fps</option>
              </select>
            </Field>
          </div>
          <Field label={`Bitrate: ${mbps} Mbps`} hint="30–80 Mbps por cabo dá qualidade visualmente sem perdas.">
            <input type="range" min={10} max={80} step={5} value={mbps} onChange={(e) => setMbps(Number(e.target.value))} className="w-full accent-violet-500" />
          </Field>
          <Field label="Resolução máxima">
            <select className={inputClass} value={size} onChange={(e) => setSize(Number(e.target.value))}>
              {MAX_SIZES.map((s, i) => (
                <option key={s.label} value={i}>
                  {s.label}
                </option>
              ))}
            </select>
          </Field>
          <div className="flex justify-between gap-2">
            <Button onClick={() => api.requestKeyframe().catch(() => {})} title="Útil se a imagem ficar corrompida">
              Pedir keyframe
            </Button>
            <Button variant="primary" onClick={applyEncoder}>
              Aplicar
            </Button>
          </div>
        </div>
      </Panel>

      <Panel title="Dispositivo">
        {t?.connected ? (
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
            <dt className="text-zinc-500">Modelo</dt>
            <dd>{t.device_name ?? "—"}</dd>
            <dt className="text-zinc-500">Sistema</dt>
            <dd>{t.device_system ?? "—"}</dd>
            <dt className="text-zinc-500">Vídeo</dt>
            <dd>
              {t.codec?.toUpperCase()} {t.width}×{t.height} até {t.fps_max} fps
            </dd>
            <dt className="text-zinc-500">Captura</dt>
            <dd>{sender?.fps_capture != null ? `${sender.fps_capture.toFixed(0)} fps` : "—"}</dd>
            <dt className="text-zinc-500">Ligado há</dt>
            <dd>{Math.floor(t.session_uptime_s)} s</dd>
          </dl>
        ) : (
          <p className="text-sm text-zinc-500">Nenhum iPad ligado.</p>
        )}
      </Panel>

      {t?.connected && <DiagnosticsPanel sender={sender} />}
    </div>
  );
}
