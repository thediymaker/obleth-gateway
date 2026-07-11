"use client";

import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

export type MetricTone = "ok" | "warn" | "hot" | "neutral";

export interface MetricTile {
  label: string;
  value: string;
  sub: string;
  tone: MetricTone;
}

const TONE_TEXT: Record<MetricTone, string> = {
  ok: "text-foreground",
  warn: "text-[hsl(38_65%_62%)]",
  hot: "text-[hsl(350_55%_64%)]",
  neutral: "text-foreground",
};

export function MetricCard({ item }: { item: MetricTile }) {
  return (
    <div className="rounded-md border border-border bg-card/55 px-4 py-3">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{item.label}</p>
      <p className={cn("mt-1 truncate text-xl font-semibold tabular-nums", TONE_TEXT[item.tone])}>{item.value}</p>
      <p className="mt-0.5 truncate text-[11px] tabular-nums text-muted-foreground/75">{item.sub}</p>
    </div>
  );
}

export function MetricToggle({ active, onClick, label }: { active: boolean; onClick: () => void; label: string }) {
  return (
    <button
      type="button"
      aria-pressed={active}
      onClick={onClick}
      className={cn(
        "inline-flex h-7 items-center rounded-sm px-2.5 text-xs transition-colors",
        active ? "bg-muted text-foreground" : "text-muted-foreground hover:text-foreground",
      )}
    >
      {label}
    </button>
  );
}

export function StripMetric({ label, value, tone = "neutral" }: { label: string; value: string; tone?: MetricTone }) {
  return (
    <div className="shrink-0 text-right">
      <p className={cn("text-xs font-medium tabular-nums", TONE_TEXT[tone])}>{value}</p>
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
    </div>
  );
}

export function DetailStat({
  label,
  value,
  tone = "neutral",
  className,
}: {
  label: string;
  value: string;
  tone?: MetricTone;
  className?: string;
}) {
  return (
    <div className={cn("min-w-0 rounded-sm border border-border/70 bg-card/35 px-2 py-1.5", className)}>
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <p className={cn("mt-0.5 truncate font-mono text-[11px] tabular-nums", TONE_TEXT[tone])}>{value}</p>
    </div>
  );
}

export function EmptyState({ children, className = "h-64" }: { children: ReactNode; className?: string }) {
  return (
    <p className={cn("flex items-center justify-center text-center text-sm text-muted-foreground", className)}>
      {children}
    </p>
  );
}
