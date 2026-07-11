"use client";

import type { ReactNode } from "react";
import { formatCompact } from "@/lib/format";
import { formatNumber } from "@/lib/utils";

/** Fixed-height wrapper — Recharts needs an explicit pixel height on the parent. */
export function ChartShell({ heightClass = "h-64", children }: { heightClass?: string; children: ReactNode }) {
  return <div className={`w-full min-h-0 ${heightClass}`}>{children}</div>;
}

/** Compact axis labels so large token counts don't crush the plot area. */
export function compactAxis(value: number) {
  return formatCompact(Number(value));
}

/** Shared chart primitives so hover/axis styling is identical across pages. */
export const axisTick = { fill: "hsl(240 6% 64%)", fontSize: 11 } as const;
export const chartGrid = { stroke: "hsl(240 4% 16%)", strokeDasharray: "3 3" } as const;
/** Subtle vertical guide for time-series; never a bright fill highlight. */
export const timeCursor = { stroke: "hsl(240 5% 30%)", strokeWidth: 1 } as const;

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
 * Readable tooltip: values render in foreground (white), names muted, with a
 * small colour swatch. Recharts' default colours item text by the series
 * colour, which turns dark series into near-invisible labels.
 */
export function ChartTooltip({ active, payload, label, hideLabel, valueFormatter, labelFormatter }: ChartTooltipProps) {
  if (!active || !payload || payload.length === 0) return null;
  const heading = labelFormatter ? labelFormatter(label, payload) : label;
  const fmt = valueFormatter ?? ((v: number) => formatNumber(v));
  return (
    <div className="rounded-lg border border-border bg-[hsl(240_5%_9%)] px-3 py-2 shadow-xl">
      {!hideLabel && heading !== undefined && heading !== "" && (
        <p className="mb-1.5 text-xs font-medium text-foreground">{heading}</p>
      )}
      <div className="space-y-1">
        {payload.map((item, i) => (
          <div key={`${item.dataKey}-${i}`} className="flex items-center gap-2 text-xs">
            <span className="h-2 w-2 shrink-0 rounded-sm" style={{ background: item.color }} />
            <span className="text-muted-foreground">{item.name}</span>
            <span className="ml-auto pl-6 font-medium tabular-nums text-foreground">{fmt(Number(item.value))}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

/** Adapt ChartTooltip to Recharts' `content` render prop without leaking `any`. */
export function tip(opts?: Omit<ChartTooltipProps, "active" | "payload" | "label">) {
  return (props: unknown) => <ChartTooltip {...(props as ChartTooltipProps)} {...opts} />;
}
