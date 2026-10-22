import type { ButtonHTMLAttributes, ReactNode } from "react";

export function Panel({ title, action, children }: { title: string; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="rounded-xl border border-zinc-800 bg-zinc-900/80 p-4">
      <div className="mb-3 flex items-center justify-between gap-2">
        <h2 className="text-xs font-semibold uppercase tracking-wider text-zinc-400">{title}</h2>
        {action}
      </div>
      {children}
    </section>
  );
}

type Variant = "primary" | "danger" | "ghost" | "live";

const VARIANTS: Record<Variant, string> = {
  primary: "bg-violet-600 text-white hover:bg-violet-500 disabled:bg-zinc-700 disabled:text-zinc-400",
  danger: "bg-red-600 text-white hover:bg-red-500",
  live: "bg-red-600/90 text-white hover:bg-red-500 ring-2 ring-red-400/40",
  ghost: "bg-zinc-800 text-zinc-200 hover:bg-zinc-700 disabled:text-zinc-500",
};

export function Button({ variant = "ghost", className = "", ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant }) {
  return (
    <button
      {...props}
      className={`inline-flex items-center justify-center gap-1.5 rounded-lg px-3 py-1.5 text-sm font-medium transition-colors disabled:cursor-not-allowed ${VARIANTS[variant]} ${className}`}
    />
  );
}

export function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <label className="block">
      <span className="mb-1 block text-xs text-zinc-400">{label}</span>
      {children}
      {hint && <span className="mt-1 block text-xs text-zinc-500">{hint}</span>}
    </label>
  );
}

export const inputClass =
  "w-full rounded-lg border border-zinc-700 bg-zinc-950 px-2.5 py-1.5 text-sm text-zinc-100 outline-none focus:border-violet-500";

export function Badge({ tone, children }: { tone: "ok" | "warn" | "busy" | "off" | "live"; children: ReactNode }) {
  const cls = {
    ok: "bg-emerald-500/15 text-emerald-300",
    live: "bg-red-500/20 text-red-300",
    warn: "bg-amber-500/15 text-amber-300",
    busy: "bg-sky-500/15 text-sky-300",
    off: "bg-zinc-800 text-zinc-400",
  }[tone];
  return <span className={`inline-flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium ${cls}`}>{children}</span>;
}

export function Dot({ tone }: { tone: "ok" | "warn" | "busy" | "off" | "live" }) {
  const cls = {
    ok: "bg-emerald-400",
    live: "bg-red-500 animate-pulse",
    warn: "bg-amber-400",
    busy: "bg-sky-400 animate-pulse",
    off: "bg-zinc-600",
  }[tone];
  return <span className={`size-2 shrink-0 rounded-full ${cls}`} />;
}
