"use client";

import type { ReactNode } from "react";
import { Image as ImageIcon, MessageSquare, Route, Scale, Sparkles, type LucideIcon } from "lucide-react";
import type { ModelRoute } from "@/lib/obleth";
import { cn } from "@/lib/utils";
import type { PlaygroundSession } from "./playground";

/** The four modes a session can be in, in tab order. Chat covers both one model and a comparison. */
export const MODES: { mode: PlaygroundSession["mode"]; label: string; icon: LucideIcon }[] = [
  { mode: "compare", label: "Chat", icon: MessageSquare },
  { mode: "image", label: "Image", icon: ImageIcon },
  { mode: "router", label: "Router", icon: Route },
  { mode: "verdicts", label: "Verdicts", icon: Scale },
];

export const modeInfo = (mode: PlaygroundSession["mode"]) =>
  MODES.find((m) => m.mode === mode || (mode === "chat" && m.mode === "compare")) ?? MODES[0];

/** "charo" and "auto" are lanes, not registry models; give them names a person reads. */
export const modelLabel = (name: string) =>
  name === "charo" ? "Assistant" : name === "auto" ? "Automatic routing" : name || "Choose a model";

/** A square monogram standing in for a model's logo. */
export function ModelAvatar({ name, size = "sm", className }: { name: string; size?: "xs" | "sm" | "md" | "lg"; className?: string }) {
  const Icon = name === "auto" ? Route : name === "charo" ? Sparkles : null;
  const letter = (name.match(/[a-z0-9]/i)?.[0] ?? "?").toUpperCase();
  return (
    <span
      aria-hidden="true"
      className={cn(
        "inline-flex shrink-0 items-center justify-center border border-border bg-secondary font-semibold text-foreground",
        Icon && "border-dashed",
        size === "xs" && "h-[18px] w-[18px] rounded text-[10px]",
        size === "sm" && "h-[22px] w-[22px] rounded-md text-[11px]",
        size === "md" && "h-8 w-8 rounded-lg text-[13px]",
        size === "lg" && "h-10 w-10 rounded-[10px] text-base",
        className,
      )}
    >
      {Icon ? <Icon className={size === "lg" ? "h-5 w-5" : size === "md" ? "h-4 w-4" : "h-3 w-3"} /> : letter}
    </span>
  );
}

/** Uppercase section label used across every settings panel. */
export function SectionLabel({ children, className, htmlFor }: { children: ReactNode; className?: string; htmlFor?: string }) {
  const cls = cn("text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", className);
  return htmlFor ? <label htmlFor={htmlFor} className={cls}>{children}</label> : <div className={cls}>{children}</div>;
}

/**
 * The right-hand settings column every mode shares. Beside the work surface on
 * wide screens; below `lg` it floats over the surface instead of squeezing it.
 */
export function SettingsPanel({ title, label, action, footer, children }: {
  title: string; label: string; action?: ReactNode; footer?: ReactNode; children: ReactNode;
}) {
  return (
    <aside
      aria-label={label}
      className="absolute inset-y-0 right-0 z-20 flex w-80 max-w-[calc(100vw-2rem)] shrink-0 flex-col border-l border-border bg-card shadow-2xl lg:static lg:z-auto lg:shadow-none"
    >
      <div className="flex h-12 shrink-0 items-center justify-between gap-2 border-b border-border pl-4 pr-3">
        <span className="text-[13px] font-semibold">{title}</span>
        {action}
      </div>
      <div className="min-h-0 flex-1 space-y-6 overflow-y-auto p-4">{children}</div>
      {footer && <div className="shrink-0 space-y-2 border-t border-border px-4 py-3.5">{footer}</div>}
    </aside>
  );
}

/** A monochrome pill. `inverted` is the one attention state; `off` marks something absent. */
export function Pill({ children, inverted, off, className }: { children: ReactNode; inverted?: boolean; off?: boolean; className?: string }) {
  return (
    <span
      className={cn(
        "inline-flex h-[22px] shrink-0 items-center gap-1 whitespace-nowrap rounded-full border px-2 text-[11.5px] font-medium",
        inverted ? "border-foreground bg-foreground text-background" : "border-border text-secondary-foreground",
        off && "border-dashed text-muted-foreground",
        className,
      )}
    >
      {children}
    </span>
  );
}

/** A row of mutually exclusive buttons. */
export function Segmented<T extends string>({ value, options, onChange, label, className }: {
  value: T; options: { value: T; label: string }[]; onChange: (value: T) => void; label: string; className?: string;
}) {
  return (
    <div role="group" aria-label={label} className={cn("flex gap-0.5 rounded-lg border border-border p-[3px]", className)}>
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          aria-pressed={value === o.value}
          onClick={() => onChange(o.value)}
          className={cn(
            "flex h-7 flex-1 items-center justify-center whitespace-nowrap rounded-md px-2 text-xs transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring",
            value === o.value ? "bg-secondary text-foreground" : "text-muted-foreground hover:text-foreground",
          )}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** On/off switch, monochrome. */
export function Switch({ checked, onChange, label, disabled }: { checked: boolean; onChange: (checked: boolean) => void; label: string; disabled?: boolean }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={cn(
        "relative inline-block h-[18px] w-8 shrink-0 rounded-full transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50",
        checked ? "bg-foreground" : "bg-muted",
      )}
    >
      <span className={cn("absolute top-0.5 h-3.5 w-3.5 rounded-full transition-all", checked ? "left-4 bg-background" : "left-0.5 bg-muted-foreground")} />
    </button>
  );
}

/** A labelled range input with its value printed on the right. */
export function SliderField({ id, label, value, display, min, max, step, onChange, hint, live, ariaLabel }: {
  id: string; label: ReactNode; value: number; display?: string; min: number; max: number; step: number;
  onChange: (value: number) => void; hint?: ReactNode;
  /** The live value, struck through beside the edited one when they differ. */
  live?: string; ariaLabel?: string;
}) {
  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between gap-2 text-[13px]">
        <label htmlFor={id}>{label}</label>
        <span className="font-mono text-xs tabular-nums">
          {live !== undefined && <span className="mr-1.5 text-muted-foreground line-through" title="Live value">{live}</span>}
          {display ?? value.toFixed(2)}
        </span>
      </div>
      <input
        id={id}
        type="range"
        aria-label={ariaLabel}
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="h-1.5 w-full cursor-pointer accent-foreground"
      />
      {hint && <p className="text-[11.5px] text-muted-foreground">{hint}</p>}
    </div>
  );
}

/** "131K", "8K", "1M" — context windows the way people say them. */
export function formatTokens(n: number): string {
  if (!n) return "—";
  if (n >= 1_000_000) return `${+(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}K`;
  return String(n);
}

/** Price per million tokens, trimmed: "$0.20", "$0.0015". */
export function perMillion(perToken: number): string {
  const v = perToken * 1_000_000;
  if (!v) return "free";
  return `$${v >= 0.01 ? v.toFixed(2) : +v.toPrecision(2)}`;
}

/** Small dollar amounts without a wall of zeros: "$0.0003", "$1.24". */
export function formatCost(usd: number): string {
  if (!Number.isFinite(usd) || usd <= 0) return "$0";
  if (usd >= 0.01) return `$${usd.toFixed(2)}`;
  return `$${+usd.toPrecision(2)}`;
}

/** Capability flags a model advertises, in the order the picker shows them. */
export function capabilities(m: ModelRoute | undefined) {
  if (!m) return [];
  return [
    { key: "tools", label: "Tools", on: m.supports_function_calling },
    { key: "schema", label: "JSON schema", on: m.supports_response_schema },
    { key: "tool_choice", label: "Forced tool choice", on: m.supports_tool_choice },
    { key: "system", label: "System prompt", on: m.supports_system_messages },
    { key: "vision", label: "Vision", on: m.supports_vision },
  ];
}

/** "now", "12m", "3h", "2d" — compact enough for the session rail. */
export function shortAgo(ts: number | undefined, now = Date.now()): string {
  if (!ts) return "";
  const s = Math.max(0, Math.round((now - ts) / 1000));
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86_400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86_400)}d`;
}
