import { duration, statusText, statusTone } from "../lib/format";
import type { Snapshot } from "../lib/types";
import { Button, Dot } from "./ui";

export function Header({
  snap,
  onToggleRecording,
}: {
  snap: Snapshot | null;
  onToggleRecording: () => void;
}) {
  const live = snap?.outputs.filter((o) => o.state === "live" || o.state === "standby").length ?? 0;
  const tone = snap ? statusTone(snap.status) : "off";
  const recording = snap?.recording;
  return (
    <header className="flex items-center gap-4 border-b border-zinc-800 px-5 py-3">
      <div className="flex items-center gap-2">
        <div className="grid size-7 place-items-center rounded-lg bg-gradient-to-br from-violet-500 to-blue-600 text-xs font-bold">P</div>
        <h1 className="text-base font-semibold tracking-tight">PadStudio</h1>
      </div>
      <div className="flex min-w-0 flex-1 items-center gap-2 rounded-full bg-zinc-900 px-3 py-1 text-sm text-zinc-300" title={snap ? statusText(snap.status) : ""}>
        <Dot tone={tone} />
        <span className="truncate">{snap ? statusText(snap.status) : "A iniciar…"}</span>
      </div>
      {live > 0 && (
        <span className="flex items-center gap-2 text-sm font-medium text-red-300">
          <Dot tone="live" /> {live} em direto
        </span>
      )}
      <Button variant={recording ? "danger" : "ghost"} onClick={onToggleRecording} title={recording ? `A gravar em ${recording.dir}` : "Gravar em MKV a 120 fps (sem recodificar)"}>
        <span className={`size-2.5 rounded-full ${recording ? "animate-pulse bg-white" : "bg-red-500"}`} />
        {recording ? `REC ${duration(recording.elapsed_s)}` : "Gravar"}
      </Button>
    </header>
  );
}
