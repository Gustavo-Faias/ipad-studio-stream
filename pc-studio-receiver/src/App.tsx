import { useCallback, useEffect, useRef, useState } from "react";
import { AudioPanel } from "./components/AudioPanel";
import { DestinationsPanel } from "./components/DestinationsPanel";
import { Header } from "./components/Header";
import { IpadPanel } from "./components/IpadPanel";
import { PreviewPanel } from "./components/PreviewPanel";
import { TelemetryBar } from "./components/TelemetryBar";
import { api, errorText, onEngineEvent, onSnapshot } from "./lib/api";
import type { PreviewStats } from "./lib/preview";
import type { SettingsView, Snapshot } from "./lib/types";

type Toast = { id: number; kind: "info" | "warning"; text: string };
type Tab = "destinos" | "audio" | "ipad";

const TABS: { id: Tab; label: string }[] = [
  { id: "destinos", label: "Destinos" },
  { id: "audio", label: "Áudio" },
  { id: "ipad", label: "iPad" },
];

export default function App() {
  const [settings, setSettings] = useState<SettingsView | null>(null);
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [preview, setPreview] = useState<PreviewStats | null>(null);
  const [tab, setTab] = useState<Tab>("destinos");
  const [toasts, setToasts] = useState<Toast[]>([]);
  const nextId = useRef(0);

  const notify = useCallback((text: string, kind: "info" | "warning" = "info") => {
    const id = nextId.current++;
    setToasts((t) => [...t.slice(-3), { id, kind, text }]);
    setTimeout(() => setToasts((t) => t.filter((x) => x.id !== id)), kind === "warning" ? 8000 : 4000);
  }, []);

  useEffect(() => {
    api.getSettings().then(setSettings).catch((e) => notify(errorText(e), "warning"));
    api.getSnapshot().then(setSnap).catch(() => {});
    const subs = [
      onSnapshot(setSnap),
      onEngineEvent((e) => {
        if (e.kind === "info") notify(e.data);
        if (e.kind === "warning") notify(e.data, "warning");
      }),
    ];
    return () => subs.forEach((p) => p.then((unlisten) => unlisten()));
  }, [notify]);

  const toggleRecording = useCallback(async () => {
    try {
      if (snap?.recording) {
        const files = await api.stopRecording();
        notify(files.length ? `Gravado: ${files.map((f) => f.path.split(/[\\/]/).pop()).join(", ")}` : "Nada gravado (sem vídeo do iPad).");
      } else {
        await api.startRecording();
        notify(`A gravar em ${settings?.record_dir ?? "Vídeos/PadStudio"}`);
      }
    } catch (e) {
      notify(errorText(e), "warning");
    }
  }, [snap?.recording, settings?.record_dir, notify]);

  const onPreviewStats = useCallback((s: PreviewStats) => setPreview(s), []);

  return (
    <div className="flex h-full flex-col bg-zinc-950 text-zinc-100">
      <Header snap={snap} onToggleRecording={toggleRecording} />
      <main className="grid min-h-0 flex-1 grid-cols-[minmax(0,1fr)_380px] gap-4 p-4">
        <section className="flex min-h-0 flex-col gap-3">
          <PreviewPanel snap={snap} onStats={onPreviewStats} />
          <TelemetryBar snap={snap} preview={preview} />
        </section>
        <aside className="flex min-h-0 flex-col gap-3">
          <nav className="grid grid-cols-3 gap-1 rounded-xl border border-zinc-800 bg-zinc-900/80 p-1">
            {TABS.map((t) => (
              <button
                key={t.id}
                onClick={() => setTab(t.id)}
                className={`rounded-lg py-1.5 text-sm font-medium ${tab === t.id ? "bg-zinc-800 text-white" : "text-zinc-400 hover:text-zinc-200"}`}
              >
                {t.label}
              </button>
            ))}
          </nav>
          <div className="min-h-0 flex-1 overflow-y-auto pr-1">
            {!settings ? (
              <p className="p-4 text-sm text-zinc-500">A carregar…</p>
            ) : tab === "destinos" ? (
              <DestinationsPanel
                destinations={settings.destinations}
                outputs={snap?.outputs ?? []}
                encoder={snap?.encoder ?? null}
                streamingError={snap?.streaming_error ?? null}
                onSettings={setSettings}
                notify={notify}
              />
            ) : tab === "audio" ? (
              <AudioPanel settings={settings} levels={snap?.mixer ?? null} onSettings={setSettings} notify={notify} />
            ) : (
              <IpadPanel settings={settings} snap={snap} onSettings={setSettings} notify={notify} />
            )}
          </div>
        </aside>
      </main>
      <div className="pointer-events-none fixed bottom-4 right-4 flex w-96 flex-col gap-2">
        {toasts.map((t) => (
          <div
            key={t.id}
            className={`rounded-lg border px-3 py-2 text-sm shadow-lg ${t.kind === "warning" ? "border-amber-500/40 bg-amber-950/90 text-amber-100" : "border-zinc-700 bg-zinc-900/95 text-zinc-200"}`}
          >
            {t.text}
          </div>
        ))}
      </div>
    </div>
  );
}
