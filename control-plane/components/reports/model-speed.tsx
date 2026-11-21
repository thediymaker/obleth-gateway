"use client";

import { useState } from "react";
import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import { ChevronRight } from "lucide-react";
import { Panel, Segmented } from "@/components/overview/ui";
import type { ModelSpeedPoint } from "@/lib/obleth";
import { linkTime, type LogsLink } from "@/lib/log-links";
import { modelHref } from "@/lib/model-links";
import { compact, formatMs, formatNumber } from "@/lib/format";
import type { DayRange } from "@/lib/reports-model";
import {
  bucketLabel,
  bucketStarts,
  changeLabel,
  columnLabels,
  describeSpeed,
  detailPaths,
  FEW_REQUESTS,
  shade,
  SHADE_STEPS,
  SLOWDOWN,
  sortSpeedRows,
  speedBucket,
  speedRows,
  SPEED_SORTS,
  tps,
  type SpeedBucket,
  type SpeedRow,
  type SpeedSort,
} from "@/lib/speed-model";
import { cn, getJson } from "@/lib/utils";
import { readFailed, ReadFailedNote } from "@/components/ui/read-state";

const COLUMNS = "grid grid-cols-[minmax(12rem,250px)_76px_104px_84px_minmax(0,1fr)_20px] gap-x-3.5";
const CHART_W = 1400;
const CHART_H = 176;

function useSpeed(range: DayRange | null, filters: { tenantId: string; keyId: string; model: string }, bucket: "none" | SpeedBucket) {
  return useQuery({
    queryKey: ["usage-speed", range?.start, range?.end, filters.tenantId, filters.keyId, filters.model, bucket],
    enabled: !!range,
    queryFn: () => {
      const p = new URLSearchParams({
        since_ms: String(linkTime(range!.start, false)),
        until_ms: String(linkTime(range!.end, true)! + 1),
        bucket,
      });
      // Hours and days are UTC, like the daily rollup the rest of Reports reads (the gateway's default).
      if (filters.tenantId) p.set("tenant_id", filters.tenantId);
      if (filters.keyId) p.set("key_id", filters.keyId);
      if (filters.model) p.set("model", filters.model);
      return getJson<ModelSpeedPoint[]>(`/api/live/usage/speed?${p}`);
    },
    staleTime: 60_000,
  });
}

function Squares({ count, children }: { count: number; children: React.ReactNode }) {
  return <span className="grid" style={{ gridTemplateColumns: `repeat(${count}, minmax(0, 1fr))`, gap: count > 31 ? 1 : 3 }}>{children}</span>;
}

function Fact({ label, value, unit }: { label: string; value: string; unit?: string }) {
  return (
    <div>
      <p className="text-[11px] uppercase tracking-[0.07em] text-muted-foreground">{label}</p>
      <p className="mt-0.5 font-mono text-[18px] font-medium">{value}{unit && value !== "—" && <span className="ml-1 text-[12px] text-muted-foreground">{unit}</span>}</p>
    </div>
  );
}

function Detail({ row, bucket, onModel, logsFor, single }: { row: SpeedRow; bucket: SpeedBucket; onModel: (m: string) => void; logsFor: (extra?: LogsLink) => string; single: boolean }) {
  const paths = detailPaths(row.cells, row.tps, CHART_W, CHART_H);
  return (
    <div className={cn(COLUMNS, "bg-muted/20 px-[18px] pb-[18px] pt-1.5")}>
      <div className="col-span-4 flex flex-col gap-3 pt-1.5">
        <div className="grid grid-cols-2 gap-x-4 gap-y-3">
          <Fact label="Median" value={tps(row.tps)} unit="tok/s" />
          <Fact label="Slowest tenth" value={tps(row.slowTps)} unit="tok/s" />
          <Fact label="First token" value={formatMs(row.ttftMs)} />
          <Fact label="Prompt" value={row.promptTps ? compact(row.promptTps) : "—"} unit="tok/s" />
        </div>
        <p className="text-[12.5px] leading-relaxed text-secondary-foreground">{describeSpeed(row, bucket)}</p>
        <div className="flex flex-wrap gap-4 text-[12.5px] text-secondary-foreground">
          <Link href={logsFor({ model: row.model })} className="hover:text-foreground">See its {formatNumber(row.requests)} requests →</Link>
          <Link href={modelHref(row.model)} className="hover:text-foreground">Open the model →</Link>
          {!single && <button type="button" onClick={() => onModel(row.model)} className="hover:text-foreground">Report on this model only</button>}
        </div>
      </div>
      <div className="flex min-w-0 flex-col gap-1.5">
        <div className="relative h-[176px] border-l border-border">
          <svg role="img" aria-label={`${row.model}: median tokens a second ${bucket === "hour" ? "by hour" : "by day"}`} viewBox={`0 0 ${CHART_W} ${CHART_H}`} preserveAspectRatio="none" className="block h-[176px] w-full">
            <path d={`M0 0.5 H${CHART_W} M0 ${CHART_H / 2} H${CHART_W} M0 ${CHART_H - 0.5} H${CHART_W}`} stroke="hsl(240 4% 16%)" strokeWidth={1} vectorEffect="non-scaling-stroke" fill="none" />
            <path d={paths.usual} stroke="hsl(240 4% 34%)" strokeWidth={1} strokeDasharray="4 4" vectorEffect="non-scaling-stroke" fill="none" />
            <path d={paths.band} fill="hsl(240 4% 26%)" fillOpacity={0.55} />
            <path d={paths.median} stroke="hsl(240 5% 90%)" strokeWidth={2} vectorEffect="non-scaling-stroke" fill="none" strokeLinejoin="round" />
          </svg>
          <span className="absolute left-1 top-0.5 font-mono text-[10.5px] text-muted-foreground">{paths.top} tok/s</span>
          <span className="absolute left-1 top-[calc(50%+2px)] font-mono text-[10.5px] text-muted-foreground">{+(paths.top / 2).toFixed(1)}</span>
        </div>
        <div className="flex flex-wrap gap-4 text-[11.5px] text-secondary-foreground">
          <span className="inline-flex items-center gap-1.5"><span className="h-0.5 w-4 bg-[hsl(240_5%_90%)]" />Median request</span>
          <span className="inline-flex items-center gap-1.5"><span className="h-2.5 w-4 rounded-[2px] bg-[hsl(240_4%_26%)]" />Down to the slowest tenth</span>
          <span className="inline-flex items-center gap-1.5"><span className="w-4 border-t border-dashed border-muted-foreground" />Its usual speed</span>
          <span>Gaps are {bucket === "hour" ? "hours" : "days"} with no requests</span>
        </div>
      </div>
      <span />
    </div>
  );
}

/**
 * How fast each model answered over the report's days. One row per model:
 * its median speed against the period before, and a square per hour or day
 * shaded by how that hour or day compared with the model's own usual speed.
 * Opening a model charts its median and slowest tenth in the same columns.
 * Speeds come from the request log, so they cover only the days it keeps.
 */
export function ModelSpeed({ range, previous, filters, against, retentionDays, modelTypes = {}, onModel, logsFor }: {
  range: DayRange | null;
  previous: DayRange | null;
  filters: { tenantId: string; keyId: string; model: string };
  against: string;
  retentionDays: number | null;
  /** Each registered model's type: only chat models have a speed worth showing. */
  modelTypes?: Record<string, string>;
  onModel: (model: string) => void;
  logsFor: (extra?: LogsLink) => string;
}) {
  const [sort, setSort] = useState<SpeedSort>("busiest");
  const [open, setOpen] = useState<string | null>(null);
  const [showFew, setShowFew] = useState(false);
  const bucket = range ? speedBucket(range) : "day";
  const starts = range ? bucketStarts(range, bucket) : [];

  const now = useSpeed(range, filters, "none");
  const before = useSpeed(previous, filters, "none");
  const series = useSpeed(range, filters, bucket);

  const single = !!filters.model;
  const all = speedRows(now.data ?? [], before.data ?? null, series.data ?? [], starts).filter((r) => (modelTypes[r.model] ?? "chat") === "chat");
  const sorted = sortSpeedRows(all, sort);
  const few = sorted.filter((r) => r.few);
  const rows = single || showFew ? sorted : sorted.filter((r) => !r.few);
  const opened = single ? filters.model : open;
  const slowed = all.filter((r) => r.slowedDown);
  const reliable = all.filter((r) => !r.few);
  const fastest = [...reliable].sort((a, b) => b.tps - a.tps)[0];
  const slowest = [...reliable].sort((a, b) => a.tps - b.tps)[0];
  const labels = columnLabels(starts, bucket);
  const olderThanLog = !!(range && retentionDays && linkTime(range.start, false)! < Date.now() - retentionDays * 86_400_000);
  const unit = bucket === "hour" ? "hour" : "day";

  return (
    <Panel
      label="Model speed"
      title={single ? `How fast ${filters.model} answered, ${unit} by ${unit}` : `How fast each model answered, ${unit} by ${unit}`}
      subtitle={`Median output tokens a second per request, after the first token. Each square is one ${unit}, shaded by how it compared with the model's own usual speed.${single ? "" : " Open a model to see its " + unit + "s in detail."}${olderThanLog ? ` The request log keeps ${retentionDays} days, so older days have no speed.` : ""}`}
      action={!single && reliable.length > 1 ? <Segmented<SpeedSort> label="Order" value={sort} onChange={setSort} options={SPEED_SORTS} /> : undefined}
    >
      {!single && reliable.length > 0 && (
        <div className="flex flex-wrap gap-x-7 gap-y-2.5 px-[18px] pb-3.5 text-[12.5px] text-secondary-foreground">
          <span>{reliable.length} model{reliable.length === 1 ? "" : "s"}</span>
          {fastest && <span>Fastest <span className="font-mono text-foreground">{fastest.model}</span> · {tps(fastest.tps)} tok/s</span>}
          {slowest && slowest !== fastest && <span>Slowest <span className="font-mono text-foreground">{slowest.model}</span> · {tps(slowest.tps)} tok/s</span>}
          {previous && <span className={cn(slowed.length > 0 && "font-medium text-foreground")}>{slowed.length ? `${slowed.length} slowed down` : "None slowed down"} by {SLOWDOWN * 100}% or more vs {against}</span>}
        </div>
      )}
      {readFailed(now) || readFailed(series) ? (
        <div className="px-[18px] pb-4"><ReadFailedNote query={readFailed(now) ? now : series} what="the models' speed" className="flex h-[120px] items-center justify-center" /></div>
      ) : now.isLoading || series.isLoading ? (
        <div className="px-[18px] pb-4"><div className="skeleton h-[240px] rounded-lg" /></div>
      ) : all.length === 0 ? (
        <div className="px-[18px] pb-4">
          <div className="flex h-[120px] items-center justify-center rounded-lg border border-dashed border-border text-sm text-muted-foreground">No token-producing requests in these days</div>
        </div>
      ) : (
        <>
          <div className="overflow-x-auto border-t border-border">
            <div className="min-w-[1040px]">
              <div className={cn(COLUMNS, "items-end border-b border-border px-[18px] pb-2 pt-2.5 text-[11px] uppercase tracking-[0.07em] text-muted-foreground")}>
                <span>Model</span>
                <span className="text-right">Requests</span>
                <span className="text-right">Median tok/s</span>
                <span className="text-right">{previous ? "vs before" : "Change"}</span>
                <Squares count={starts.length}>
                  {labels.map((l, i) => <span key={starts[i]} className={cn("overflow-visible whitespace-nowrap text-center font-mono text-[10.5px] normal-case tracking-normal", i === starts.length - 1 && "text-foreground")}>{l}</span>)}
                </Squares>
                <span />
              </div>
              {rows.map((r) => {
                const isOpen = opened === r.model;
                const label = changeLabel(r.change);
                return (
                  <div key={r.model} className="border-b border-border/70">
                      <button
                        type="button"
                        aria-expanded={isOpen}
                        disabled={single}
                        onClick={() => setOpen(isOpen ? null : r.model)}
                        className={cn(COLUMNS, "w-full items-center px-[18px] py-[9px] text-left text-[13px] disabled:cursor-default", !single && "hover:bg-muted/30", isOpen && "bg-muted/20")}
                      >
                        <span className="flex min-w-0 items-center gap-2">
                          <span className={cn("truncate font-mono text-[12.5px]", r.few && "text-muted-foreground")}>{r.model}</span>
                          {r.few && <span className="rounded-[5px] border border-border px-1.5 text-[11px] text-muted-foreground" title={`Fewer than ${FEW_REQUESTS} measured requests, so the median says little`}>few</span>}
                        </span>
                        <span className="text-right font-mono text-[12px] text-secondary-foreground">{compact(r.requests)}</span>
                        <span className={cn("text-right font-mono text-[13px] font-medium", r.few && "text-muted-foreground")}>{tps(r.tps)}</span>
                        <span className={cn("text-right font-mono text-[12px]", r.slowedDown ? "font-semibold text-foreground" : "text-muted-foreground")} title={r.previousTps !== null ? `${tps(r.previousTps)} tok/s ${against}` : `Not used ${against}`}>
                          {label ?? "—"}
                        </span>
                        <Squares count={starts.length}>
                          {r.cells.map((c, i) => c ? (
                            <span key={starts[i]} title={`${bucketLabel(c.t, bucket)}: ${tps(c.tps)} tok/s, ${Math.round(c.ratio * 100)}% of its usual · ${formatNumber(c.rated)} requests`} className="block h-[22px] rounded-[4px]" style={{ background: r.few ? "hsl(240 5% 13%)" : shade(c.ratio) }} />
                          ) : (
                            <span key={starts[i]} title={`${bucketLabel(starts[i], bucket)}: no requests`} className="block h-[22px] rounded-[4px] border border-dashed border-border" />
                          ))}
                        </Squares>
                        <span aria-hidden className="flex justify-end text-muted-foreground">{!single && <ChevronRight className={cn("h-3.5 w-3.5 transition-transform", isOpen && "rotate-90")} />}</span>
                      </button>
                    {isOpen && <Detail row={r} bucket={bucket} onModel={onModel} logsFor={logsFor} single={single} />}
                  </div>
                );
              })}
            </div>
          </div>
          <div className="flex flex-wrap items-center justify-between gap-x-6 gap-y-3 px-[18px] py-3 text-[12px] text-secondary-foreground">
            <div className="flex flex-wrap items-center gap-2.5">
              <span>Compared with its usual speed:</span>
              <span className="inline-flex gap-[3px]" aria-hidden>{SHADE_STEPS.map((c) => <span key={c} className="h-3 w-4 rounded-[3px]" style={{ background: c }} />)}</span>
              <span>usual → half speed or worse</span>
              <span className="ml-2 inline-flex items-center gap-1.5"><span className="h-2.5 w-3.5 rounded-[3px] border border-dashed border-border" />no requests</span>
            </div>
            {!single && few.length > 0 && (
              <button type="button" onClick={() => setShowFew((v) => !v)} className="hover:text-foreground">
                {showFew ? `Hide the ${few.length} with few requests` : `Show ${few.length} more with fewer than ${FEW_REQUESTS} requests`}
              </button>
            )}
          </div>
          <p className="px-[18px] pb-3.5 text-[11.5px] text-muted-foreground">Successful, uncached requests that wrote at least 16 tokens. Audio, image and embedding models aren&apos;t listed: they don&apos;t produce tokens a second.</p>
        </>
      )}
    </Panel>
  );
}
