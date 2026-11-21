"use client";

import type { ReactNode } from "react";
import { formatNumber } from "@/lib/format";
import { cn } from "@/lib/utils";

/** The frame every chart tooltip uses: a title line, then TooltipRows. */
export function TooltipShell({ title, className, children }: { title?: ReactNode; className?: string; children: ReactNode }) {
  return (
    <div className={cn("min-w-44 rounded-lg border border-border bg-chart-tooltip px-3 py-2 text-xs shadow-xl", className)}>
      {title !== undefined && title !== "" && <p className="mb-1.5 font-mono text-muted-foreground">{title}</p>}
      {children}
    </div>
  );
}

/** One label and value in a tooltip, with an optional colour swatch for its series. */
export function TooltipRow({ label, value, swatch, muted, className }: { label: ReactNode; value: ReactNode; swatch?: string; muted?: boolean; className?: string }) {
  return (
    <div className={cn("flex items-center justify-between gap-6", muted && "text-muted-foreground", className)}>
      <span className="inline-flex items-center gap-1.5">
        {swatch && <span className="h-2 w-2 shrink-0 rounded-[2px]" style={{ background: swatch }} />}
        {label}
      </span>
      <span className="font-mono tabular-nums">{value}</span>
    </div>
  );
}

interface TooltipDatum {
  name?: string | number;
  value?: number | string;
  color?: string;
  dataKey?: string | number;
  payload?: Record<string, unknown>;
}

export interface ChartTooltipProps {
  active?: boolean;
  label?: string | number;
  payload?: TooltipDatum[];
  hideLabel?: boolean;
  valueFormatter?: (v: number) => string;
  labelFormatter?: (label: string | number | undefined, payload: TooltipDatum[]) => ReactNode;
}

/**
 * The tooltip for charts that need nothing of their own: one row per series
 * with its swatch. Recharts' default colours item text by the series colour,
 * which turns dark series into near-invisible labels.
 */
export function ChartTooltip({ active, payload, label, hideLabel, valueFormatter, labelFormatter }: ChartTooltipProps) {
  if (!active || !payload || payload.length === 0) return null;
  const heading = labelFormatter ? labelFormatter(label, payload) : label;
  const fmt = valueFormatter ?? ((v: number) => formatNumber(v));
  return (
    <TooltipShell title={hideLabel ? undefined : heading} className="min-w-0">
      <div className="space-y-1">
        {payload.map((item, i) => (
          <TooltipRow key={`${item.dataKey}-${i}`} swatch={item.color} label={<span className="text-muted-foreground">{item.name}</span>} value={fmt(Number(item.value))} />
        ))}
      </div>
    </TooltipShell>
  );
}

/** Adapt ChartTooltip to Recharts' `content` render prop without leaking `any`. */
export function tip(opts?: Omit<ChartTooltipProps, "active" | "payload" | "label">) {
  return (props: unknown) => <ChartTooltip {...(props as ChartTooltipProps)} {...opts} />;
}
