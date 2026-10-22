import { useEffect, useState } from "react";
import { api, errorText } from "../lib/api";
import { meterFraction } from "../lib/format";
import type { InputId, InputLevel, MicSelection, MixerLevels, SettingsView } from "../lib/types";
import { INPUT_INDEX } from "../lib/types";
import { Dot, Field, inputClass, Panel } from "./ui";

type Notify = (text: string, kind?: "info" | "warning") => void;

const CHANNELS: { id: InputId; label: string; hint: string }[] = [
  { id: "ipad_app", label: "Jogo (iPad)", hint: "Som da app ou jogo no iPad" },
  { id: "ipad_mic", label: "Microfone (iPad)", hint: "Ativa-o na folha de transmissão do iPad" },
  { id: "pc_mic", label: "Microfone (PC)", hint: "Escolhe o dispositivo abaixo" },
];

function Meter({ db }: { db: number }) {
  const f = meterFraction(db);
  const color = db > -3 ? "bg-red-500" : db > -12 ? "bg-amber-400" : "bg-emerald-400";
  return (
    <div className="h-1.5 w-full overflow-hidden rounded-full bg-zinc-800">
      <div className={`h-full ${color} transition-[width] duration-75`} style={{ width: `${f * 100}%` }} />
    </div>
  );
}

function Channel({
  label,
  hint,
  level,
  gain,
  muted,
  onGain,
  onMute,
}: {
  label: string;
  hint: string;
  level: InputLevel | undefined;
  gain: number;
  muted: boolean;
  onGain: (db: number) => void;
  onMute: (m: boolean) => void;
}) {
  return (
    <div className="space-y-1.5">
      <div className="flex items-center gap-2">
        <Dot tone={level?.active ? "ok" : "off"} />
        <span className="flex-1 text-sm" title={hint}>
          {label}
        </span>
        <span className="w-16 text-right font-mono text-xs tabular-nums text-zinc-400">{gain > 0 ? "+" : ""}{gain.toFixed(1)} dB</span>
        <button
          className={`rounded-md px-2 py-0.5 text-xs font-medium ${muted ? "bg-red-500/20 text-red-300" : "bg-zinc-800 text-zinc-300 hover:bg-zinc-700"}`}
          onClick={() => onMute(!muted)}
        >
          {muted ? "Mudo" : "Som"}
        </button>
      </div>
      <Meter db={level?.active && !muted ? level.peak_db : -120} />
      <input
        type="range"
        min={-60}
        max={12}
        step={0.5}
        value={gain}
        onChange={(e) => onGain(Number(e.target.value))}
        onDoubleClick={() => onGain(0)}
        className="w-full accent-violet-500"
        title="Duplo clique: 0 dB"
      />
    </div>
  );
}

function micValue(m: MicSelection): string {
  return m.kind === "named" ? `named:${m.name}` : m.kind;
}

export function AudioPanel({
  settings,
  levels,
  onSettings,
  notify,
}: {
  settings: SettingsView;
  levels: MixerLevels | null;
  onSettings: (s: SettingsView) => void;
  notify: Notify;
}) {
  const [mics, setMics] = useState<{ devices: string[]; default: string | null }>({ devices: [], default: null });

  useEffect(() => {
    api.listMics().then(setMics).catch((e) => notify(errorText(e), "warning"));
  }, [notify]);

  const setGain = (id: InputId, db: number) => {
    const gains = [...settings.gains_db] as SettingsView["gains_db"];
    gains[INPUT_INDEX[id]] = db;
    onSettings({ ...settings, gains_db: gains });
    api.setGain(id, db).catch((e) => notify(errorText(e), "warning"));
  };
  const setMuted = (id: InputId, m: boolean) => {
    const muted = [...settings.muted] as SettingsView["muted"];
    muted[INPUT_INDEX[id]] = m;
    onSettings({ ...settings, muted });
    api.setMuted(id, m).catch((e) => notify(errorText(e), "warning"));
  };
  const setMic = (value: string) => {
    const sel: MicSelection = value === "off" ? { kind: "off" } : value === "default" ? { kind: "default" } : { kind: "named", name: value.slice(6) };
    onSettings({ ...settings, mic: sel });
    api.setMic(sel).catch((e) => notify(errorText(e), "warning"));
  };

  return (
    <Panel title="Áudio">
      <div className="space-y-4">
        {CHANNELS.map((c) => (
          <Channel
            key={c.id}
            label={c.label}
            hint={c.hint}
            level={levels?.[c.id]}
            gain={settings.gains_db[INPUT_INDEX[c.id]]}
            muted={settings.muted[INPUT_INDEX[c.id]]}
            onGain={(db) => setGain(c.id, db)}
            onMute={(m) => setMuted(c.id, m)}
          />
        ))}
        <Field
          label="Microfone do PC"
          hint={levels?.mic_error ?? (levels?.mic ? `${levels.mic.sample_rate / 1000} kHz · ${levels.mic.channels} canais` : undefined)}
        >
          <select className={inputClass} value={micValue(settings.mic)} onChange={(e) => setMic(e.target.value)}>
            <option value="off">Desligado</option>
            <option value="default">Predefinido do sistema{mics.default ? ` (${mics.default})` : ""}</option>
            {mics.devices.map((d) => (
              <option key={d} value={`named:${d}`}>
                {d}
              </option>
            ))}
          </select>
        </Field>
        <div className="space-y-1.5 border-t border-zinc-800 pt-3">
          <div className="flex justify-between text-xs text-zinc-400">
            <span>Mistura (vai para a live)</span>
            <span className="font-mono tabular-nums">{levels ? `${Math.max(levels.master_peak_db, -60).toFixed(1)} dB` : "—"}</span>
          </div>
          <Meter db={levels?.master_peak_db ?? -120} />
        </div>
      </div>
    </Panel>
  );
}
