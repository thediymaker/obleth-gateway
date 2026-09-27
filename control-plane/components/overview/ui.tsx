"use client";

import type { ReactNode } from "react";
import Link from "next/link";
import { ArrowRight } from "lucide-react";
import { cn } from "@/lib/utils";
import type { HealthState } from "@/lib/overview-model";

/** The Overview's card: one border, one radius, one header shape. */
export function Panel({ title, subtitle, action, children, className, label }: {
  title?: ReactNode; subtitle?: ReactNode; action?: ReactNode; children: ReactNode; className?: string; label?: string;
}) {
  return (
    <section aria-label={label ?? (typeof title === "string" ? title : undefined)} className={cn("flex min-w-0 flex-col rounded-xl border border-border bg-card", className)}>
      {(title || action) && (
        <div className="flex flex-wrap items-start justify-between gap-3 px-[18px] pt-4">
          <div className="min-w-0">
            {title && <h2 className="text-sm font-semibold">{title}</h2>}
            {subtitle && <p className="mt-0.5 text-xs text-muted-foreground">{subtitle}</p>}
          </div>
          {action}
        </div>
      )}
      {children}
    </section>
  );
}

export function SectionLabel({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn("text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", className)}>{children}</div>;
}

export function Pill({ children, inverted, className }: { children: ReactNode; inverted?: boolean; className?: string }) {
  return (
    <span className={cn(
      "inline-flex h-[22px] shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full border px-2 text-[11.5px] font-medium",
      inverted ? "border-foreground bg-foreground text-background" : "border-border text-secondary-foreground",
      className,
    )}>
      {children}
    </span>
  );
}

/**
 * Monochrome health: a filled dot for healthy, a ring for no data, a dashed
 * ring for maintenance. Failing models get an inverted "Down" badge instead,
 * which is the one attention mark on the page.
 */
export function HealthGlyph({ state, className }: { state: HealthState; className?: string }) {
  if (state === "unhealthy") return <Pill inverted className={cn("h-4 px-1.5 text-[10px] font-bold tracking-wide", className)}>DOWN</Pill>;
  return (
    <span
      role="img"
      aria-label={state === "healthy" ? "Healthy" : state === "maintenance" ? "In maintenance" : "No health data"}
      className={cn(
        "inline-block h-2 w-2 shrink-0 rounded-full",
        state === "healthy" && "bg-secondary-foreground",
        state === "unknown" && "border-[1.5px] border-muted-foreground",
        state === "maintenance" && "border-[1.5px] border-dashed border-secondary-foreground",
        className,
      )}
    />
  );
}

/** A horizontal fill bar for "x of y": pool load, share of traffic. */
export function Meter({ value, max, className, strong }: { value: number; max: number; className?: string; strong?: boolean }) {
  const pct = max > 0 ? Math.min(100, Math.max(0, (value / max) * 100)) : 0;
  return (
    <span className={cn("block h-1.5 overflow-hidden rounded-full bg-muted", className)} aria-hidden="true">
      <span className={cn("block h-full rounded-full", strong ? "bg-foreground" : "bg-secondary-foreground")} style={{ width: `${pct}%` }} />
    </span>
  );
}

export function Segmented<T extends string>({ value, options, onChange, label, className }: {
  value: T; options: { value: T; label: ReactNode }[]; onChange: (value: T) => void; label: string; className?: string;
}) {
  return (
    <div role="group" aria-label={label} className={cn("inline-flex flex-wrap gap-0.5 rounded-[9px] border border-border p-[3px]", className)}>
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          aria-pressed={value === o.value}
          onClick={() => onChange(o.value)}
          className={cn(
            "inline-flex h-7 items-center justify-center whitespace-nowrap rounded-md px-2.5 text-[12.5px] transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring",
            value === o.value ? "bg-secondary text-foreground" : "text-muted-foreground hover:text-foreground",
          )}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

export function PanelLink({ href, children }: { href: string; children: ReactNode }) {
  return (
    <Link href={href} className="inline-flex items-center gap-1.5 text-[12.5px] text-secondary-foreground hover:text-foreground">
      {children}<ArrowRight className="h-3.5 w-3.5" aria-hidden="true" />
    </Link>
  );
}

/** A line of numbers over time, no axes: the tiles' at-a-glance trend. */
export function Sparkline({ values, className, label }: { values: number[]; className?: string; label?: string }) {
  if (values.length < 2 || values.every((v) => v === 0)) return <div className={cn("h-9", className)} aria-hidden="true" />;
  const max = Math.max(...values);
  const min = Math.min(...values);
  const span = max - min || 1;
  const d = values.map((v, i) => `${i === 0 ? "M" : "L"}${((i / (values.length - 1)) * 180).toFixed(1)} ${(34 - ((v - min) / span) * 32).toFixed(1)}`).join(" ");
  return (
    <svg viewBox="0 0 180 36" preserveAspectRatio="none" className={cn("h-9 w-full", className)} role={label ? "img" : undefined} aria-label={label} aria-hidden={label ? undefined : true}>
      <path d={d} fill="none" stroke="hsl(240 5% 65%)" strokeWidth="1.5" vectorEffect="non-scaling-stroke" strokeLinejoin="round" />
    </svg>
  );
}

export function Kpi({ label, value, detail, trend, href, emphasis }: {
  label: string; value: string; detail?: string | null; trend: number[]; href?: string; emphasis?: boolean;
}) {
  const body = (
    <>
      <SectionLabel className={cn(emphasis && "text-secondary-foreground")}>{label}</SectionLabel>
      <span className="text-[26px] font-semibold leading-tight tracking-tight tabular-nums">{value}</span>
      <span className={cn("min-h-4 truncate text-xs", emphasis ? "text-foreground" : "text-muted-foreground")}>{detail ?? " "}</span>
      <Sparkline values={trend} label={`${label} trend`} />
    </>
  );
  const cls = cn("flex min-w-0 flex-col gap-1.5 rounded-xl border bg-card px-4 pb-3 pt-3.5 transition-colors", emphasis ? "border-muted-foreground" : "border-border", href && "hover:border-muted-foreground/60");
  return href ? <Link href={href} className={cls}>{body}</Link> : <div className={cls}>{body}</div>;
}
