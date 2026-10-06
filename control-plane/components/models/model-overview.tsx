"use client";

import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import Link from "next/link";
import { Area, ComposedChart, Line, ResponsiveContainer, Tooltip, XAxis, YAxis, CartesianGrid } from "recharts";
import { axisTick, chartGrid, compactAxis, timeCursor } from "@/components/chart-tooltip";
import { Meter, Panel, Segmented } from "@/components/overview/ui";
import { Tile } from "@/components/models/ui";
import { MODEL_SERIES_BUCKET_MS, priceLabel, RUNS_LABELS, tagLabels, type ModelOverviewData, type RunsOn } from "@/lib/models-model";
import type { ModelCapacityStatus, ModelHealthCheck, ModelHealthSummary, ModelRoute, UsageLogHistogram } from "@/lib/obleth";
import { logsHref } from "@/lib/log-links";
import { changeLabel, compact, describeAudit, formatMs } from "@/lib/overview-model";
import { cn, getJson } from "@/lib/utils";
import { auditHref } from "@/lib/audit-model";
import { boonsWaitingOnFunctionCalling } from "@/lib/boon-availability";

const DAY_MS = 86_400_000;
const CURRENT = "hsl(240 5% 90%)";
const PREVIOUS = "hsl(240 4% 46%)";

type Measure = "requests" | "ttft" | "throughput";

/** "40 s", "5 min", "3 h", "2 d". */
export function ago(ts: string | number | null | undefined, now = Date.now()): string {
  if (!ts) return "never";
  const s = Math.max(0, Math.round((now - new Date(ts).getTime()) / 1000));
  if (s < 60) return `${s} s ago`;
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86_400) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86_400)} d ago`;
}

function interval(secs: number) {
  return secs % 3600 === 0 ? `${secs / 3600} h` : secs % 60 === 0 ? `${secs / 60} min` : `${secs} s`;
}

interface Row { t: number; label: string; cur: number | null; prev: number | null }

function buildRows(data: ModelOverviewData, measure: Measure): Row[] {
  const b = MODEL_SERIES_BUCKET_MS;
  const byT = new Map(data.series.map((p) => [Math.floor(p.bucket_ms / b) * b, p]));
  const start = Math.floor((data.now - DAY_MS) / b) * b + b;
  const pick = (t: number) => {
    const p = byT.get(t);
    if (!p) return measure === "requests" ? 0 : null;
    const v = measure === "requests" ? p.requests : measure === "ttft" ? p.p50_ttft_ms : p.gen_tokens_per_sec;
    return measure !== "requests" && !v ? null : Number(v);
  };
  const rows: Row[] = [];
  for (let t = start; t <= data.now; t += b) {
    rows.push({ t, label: new Date(t).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }), cur: pick(t), prev: pick(t - DAY_MS) });
  }
  return rows;
}

function ChartTip({ active, payload, measure }: { active?: boolean; payload?: { payload: Row }[]; measure: Measure }) {
  const row = payload?.[0]?.payload;
  if (!active || !row) return null;
  const fmt = (v: number | null) => (v === null ? "—" : measure === "ttft" ? formatMs(v) : measure === "throughput" ? `${compact(v)} tok/s` : v.toLocaleString());
  return (
    <div className="min-w-40 rounded-lg border border-border bg-[hsl(240_5%_9%)] px-3 py-2 text-xs shadow-xl">
      <p className="mb-1.5 font-mono text-muted-foreground">{row.label}</p>
      <div className="flex justify-between gap-6"><span>Today</span><span className="font-mono tabular-nums">{fmt(row.cur)}</span></div>
      <div className="flex justify-between gap-6 text-muted-foreground"><span>Yesterday</span><span className="font-mono tabular-nums">{fmt(row.prev)}</span></div>
    </div>
  );
}

function TrafficChart({ data }: { data: ModelOverviewData | undefined }) {
  const [measure, setMeasure] = useState<Measure>("requests");
  const rows = useMemo(() => (data ? buildRows(data, measure) : []), [data, measure]);
  const empty = rows.every((r) => !r.cur && !r.prev);
  return (
    <Panel
      label="Traffic"
      title={measure === "requests" ? "Requests" : measure === "ttft" ? "Time to first token" : "Generation speed"}
      subtitle={`Every 30 minutes, last 24 hours${measure === "ttft" ? " · median per bucket" : measure === "throughput" ? " · tokens per second" : ""}`}
      action={
        <div className="flex flex-wrap items-center gap-3">
          <span className="hidden items-center gap-3 text-xs text-muted-foreground sm:flex">
            <span className="inline-flex items-center gap-1.5"><span className="h-0.5 w-4 rounded bg-foreground" />Today</span>
            <span className="inline-flex items-center gap-1.5"><span className="w-4 border-t-2 border-dashed border-muted-foreground" />Yesterday</span>
          </span>
          <Segmented label="Measure" value={measure} onChange={setMeasure} options={[{ value: "requests", label: "Requests" }, { value: "ttft", label: "First token" }, { value: "throughput", label: "Speed" }]} />
        </div>
      }
    >
      <div className="px-[18px] pb-4 pt-3">
        {!data ? (
          <div className="skeleton h-[220px] rounded-lg" />
        ) : empty ? (
          <div className="flex h-[220px] items-center justify-center rounded-lg border border-dashed border-border text-sm text-muted-foreground">No traffic in the last two days</div>
        ) : (
          <div className="h-[220px]">
            <ResponsiveContainer width="100%" height="100%">
              <ComposedChart data={rows} margin={{ top: 8, right: 4, left: 0, bottom: 0 }}>
                <CartesianGrid {...chartGrid} vertical={false} />
                <XAxis dataKey="label" tick={axisTick} axisLine={false} tickLine={false} minTickGap={56} height={20} />
                <YAxis tick={axisTick} axisLine={false} tickLine={false} width={44} allowDecimals={false} tickFormatter={measure === "ttft" ? (v: number) => formatMs(v) : compactAxis} />
                <Tooltip cursor={timeCursor} content={<ChartTip measure={measure} />} />
                <Area type="monotone" dataKey="cur" stroke="none" fill={CURRENT} fillOpacity={0.07} isAnimationActive={false} connectNulls={measure === "requests"} />
                <Line type="monotone" dataKey="prev" stroke={PREVIOUS} strokeWidth={2} strokeDasharray="5 4" dot={false} isAnimationActive={false} connectNulls={measure === "requests"} />
                <Line type="monotone" dataKey="cur" stroke={CURRENT} strokeWidth={2} dot={false} activeDot={{ r: 4, strokeWidth: 2, stroke: "hsl(240 5% 7%)" }} isAnimationActive={false} connectNulls={measure === "requests"} />
              </ComposedChart>
            </ResponsiveContainer>
          </div>
        )}
      </div>
    </Panel>
  );
}

const TICK: Record<string, string> = {
  healthy: "bg-muted-foreground/70",
  degraded: "bg-muted-foreground/35",
  unhealthy: "bg-foreground",
  disabled: "bg-muted",
};

export function HealthTicks({ checks, summary }: { checks: ModelHealthCheck[]; summary: ModelHealthSummary }) {
  const recent = checks.slice(0, 30).reverse();
  const failed = recent.filter((c) => c.status === "unhealthy").length;
  return (
    <section aria-label="Health checks" className="flex flex-col gap-2.5 rounded-xl border border-border bg-card px-[18px] py-4">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-sm font-semibold">Health checks</h2>
        <span className="text-xs text-muted-foreground">{summary.checks_enabled ? `every ${interval(summary.check_interval_secs)}` : "scheduled checks off"}</span>
      </div>
      {recent.length === 0 ? (
        <p className="text-xs text-muted-foreground">No checks yet.</p>
      ) : (
        <div className="flex h-[18px] gap-[3px]" role="img" aria-label={`${recent.length} recent checks, ${failed} failed`}>
          {recent.map((c) => <span key={c.id} title={`${new Date(c.checked_at).toLocaleString()} · ${c.status}${c.latency_ms != null ? ` · ${c.latency_ms} ms` : ""}`} className={cn("block flex-1 rounded-[2px]", TICK[c.status] ?? "border border-muted-foreground/50")} />)}
        </div>
      )}
      <p className="text-xs text-secondary-foreground">
        {summary.last_checked_at ? (
          <>Last check {ago(summary.last_checked_at)}{summary.last_http_status ? ` · HTTP ${summary.last_http_status}` : ""}{summary.last_latency_ms != null ? ` in ${summary.last_latency_ms} ms` : ""}</>
        ) : "Not checked yet"}
      </p>
      {summary.last_message && summary.status !== "healthy" && <p className="text-xs text-muted-foreground">{summary.last_message}</p>}
    </section>
  );
}

function WhereItRuns({ model, runs, capacity, endpoints }: { model: ModelRoute; runs: RunsOn; capacity?: ModelCapacityStatus; endpoints: number }) {
  let host = model.api_base;
  try { host = new URL(model.api_base).host; } catch { /* not a URL: show as stored */ }
  return (
    <section aria-label="Where it runs" className="flex flex-col gap-2.5 rounded-xl border border-border bg-card px-[18px] py-4">
      <div className="flex items-center justify-between gap-3">
        <h2 className="text-sm font-semibold">Where it runs</h2>
        <a href="#deployment" className="text-[12.5px] text-secondary-foreground hover:text-foreground">Deployment ›</a>
      </div>
      <p className="text-[13px] text-secondary-foreground">
        {RUNS_LABELS[runs]}
        {runs === "kubernetes" && capacity?.service && <> · Service <span className="font-mono text-[12px]">{capacity.service}</span>{capacity.namespace ? ` in ${capacity.namespace}` : ""}</>}
        {runs === "endpoint" && model.api_base && <> · <span className="font-mono text-[12px]">{host}</span></>}
      </p>
      {capacity && capacity.ready_replicas != null ? (
        <>
          <div className="flex items-center gap-2.5">
            <Meter value={capacity.ready_replicas} max={Math.max(capacity.ready_replicas, 1)} className="h-2 flex-1" />
            <span className="font-mono text-[12px] tabular-nums">{capacity.ready_replicas} ready</span>
          </div>
          <p className="text-xs text-muted-foreground">
            {capacity.per_replica_max_in_flight != null ? `× ${capacity.per_replica_max_in_flight} per replica = ` : ""}{capacity.effective_max_in_flight} slots · follows the backend
          </p>
        </>
      ) : (
        <p className="text-xs text-muted-foreground">
          {model.capacity_mode === "discovered" ? "Waiting for discovery" : model.max_in_flight ? `${model.max_in_flight} slots, set by hand` : "No slot cap"}
          {endpoints > 0 ? ` · ${endpoints} extra endpoint${endpoints === 1 ? "" : "s"}` : ""}
        </p>
      )}
    </section>
  );
}

function TopTenants({ data }: { data: ModelOverviewData | undefined }) {
  const max = Math.max(1, ...(data?.tenants ?? []).map((t) => t.requests));
  return (
    <section aria-label="Top tenants" className="flex flex-col gap-1 rounded-xl border border-border bg-card px-[18px] pb-2.5 pt-4">
      <h2 className="pb-1.5 text-sm font-semibold">Top tenants · 24h</h2>
      {!data ? (
        <div className="space-y-2 pb-2"><div className="skeleton h-4" /><div className="skeleton h-4" /><div className="skeleton h-4" /></div>
      ) : data.tenants.length === 0 ? (
        <p className="pb-2 text-xs text-muted-foreground">No requests in the last 24 hours.</p>
      ) : (
        data.tenants.map((t) => (
          <div key={t.name} className="grid grid-cols-[minmax(0,1fr)_88px_52px] items-center gap-2.5 border-t border-border/70 py-2 text-[13px] first:border-t-0">
            <span className="truncate">{t.name}</span>
            <Meter value={t.requests} max={max} />
            <span className="text-right font-mono text-[12px] tabular-nums">{compact(t.requests)}</span>
          </div>
        ))
      )}
    </section>
  );
}

function SetupRow({ label, href, children }: { label: string; href: string; children: React.ReactNode }) {
  return (
    <a href={href} className="grid grid-cols-[130px_minmax(0,1fr)] items-baseline gap-3 border-t border-border/70 py-2 text-[13px] first:border-t-0 hover:text-foreground">
      <span className="text-muted-foreground">{label}</span>
      <span className="min-w-0 truncate">{children}</span>
    </a>
  );
}

export function ModelOverview({
  model,
  data,
  summary,
  checks,
  runs,
  capacity,
  load,
  endpoints,
}: {
  model: ModelRoute;
  data: ModelOverviewData | undefined;
  summary: ModelHealthSummary;
  checks: ModelHealthCheck[];
  runs: RunsOn;
  capacity?: ModelCapacityStatus;
  load: { inFlight: number; cap: number; queued: number };
  endpoints: number;
}) {
  const u = data?.usage;
  // Failures come from the raw log, which the rollup's per-model read lacks.
  const { data: hist } = useQuery({
    queryKey: ["model-failed-24h", model.model_name],
    queryFn: () => getJson<UsageLogHistogram>(`/api/live/usage/logs/histogram?model=${encodeURIComponent(model.model_name)}&since_ms=${Date.now() - DAY_MS}&bucket_ms=3600000`),
    refetchInterval: 60_000,
  });
  const failed = hist ? hist.buckets.reduce((t, b) => ({ requests: t.requests + b.requests, errors: t.errors + b.errors }), { requests: 0, errors: 0 }) : null;
  const chat = model.model_type === "chat";
  const inactiveBoons = boonsWaitingOnFunctionCalling(model);
  const caps = [
    model.supports_function_calling && "Function calling",
    model.supports_tool_choice && "Tool choice",
    model.supports_response_schema && "Response schema",
    model.supports_system_messages && "System messages",
    model.supports_vision && "Vision",
  ].filter(Boolean) as string[];
  const tags = tagLabels(model.tags);
  const price = priceLabel(model);
  const names = { models: new Map([[model.id, model.model_name]]), tenants: new Map<string, string>() };
  return (
    <div className="flex flex-col gap-4">
      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-5">
        <Tile
          label="Load now"
          value={model.enabled ? (load.cap > 0 ? <>{load.inFlight} <span className="text-[13px] font-normal text-muted-foreground">of {load.cap}</span></> : load.inFlight) : "Off"}
          detail={!model.enabled ? "Takes no requests" : load.queued > 0 ? `${load.queued} waiting` : load.cap > 0 ? `${Math.round((load.inFlight / load.cap) * 100)}% of the pool` : "No slot cap"}
          emphasis={load.queued > 0}
        />
        <Tile label="Requests · 24h" href={u?.requests ? logsHref({ model: model.model_name, window: "24h" }) : undefined} value={u ? compact(u.requests) : data ? "0" : "—"} detail={data && data.previousRequests !== null ? changeLabel(Number(u?.requests ?? 0), data.previousRequests, "yesterday") : null} />
        <Tile
          label="Failed · 24h"
          href={failed?.errors ? logsHref({ model: model.model_name, window: "24h", status: "error" }) : undefined}
          value={failed ? compact(failed.errors) : "—"}
          detail={failed ? (failed.requests ? `${((failed.errors / failed.requests) * 100).toFixed(1)}% of requests${failed.errors ? " · see them" : ""}` : "No requests") : null}
          emphasis={!!failed && failed.requests > 0 && failed.errors / failed.requests >= 0.05}
        />
        <Tile label="First token · p50" value={u?.p50_ttft_ms ? formatMs(u.p50_ttft_ms) : "—"} detail={u?.avg_ttft_ms ? `avg ${formatMs(u.avg_ttft_ms)}` : null} />
        <Tile label="Tokens · 24h" value={u ? compact(u.total_tokens) : "—"} detail={u ? `${compact(u.input_tokens)} in · ${compact(u.output_tokens)} out · ${compact(u.users)} users` : null} />
      </div>

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_340px]">
        <TrafficChart data={data} />
        <div className="flex flex-col gap-4">
          <HealthTicks checks={checks} summary={summary} />
          <WhereItRuns model={model} runs={runs} capacity={capacity} endpoints={endpoints} />
          <TopTenants data={data} />
        </div>
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        <section aria-label="Set up" className="rounded-xl border border-border bg-card px-[18px] pb-2 pt-4">
          <div className="flex items-center justify-between pb-1.5">
            <h2 className="text-sm font-semibold">Set up</h2>
            <a href="#general" className="text-[12.5px] text-secondary-foreground hover:text-foreground">Edit ›</a>
          </div>
          <SetupRow label="Upstream" href="#set-upstream"><span className="font-mono text-[12px]">{model.upstream_model}</span></SetupRow>
          <SetupRow label="API base" href="#set-api-base"><span className="font-mono text-[12px]">{model.api_base || "set by the provisioner"}</span></SetupRow>
          {chat && <SetupRow label="Router tags" href="#set-tags">{tags.length ? tags.join(" · ") : <span className="text-muted-foreground">none</span>}{!model.auto_eligible && <span className="text-muted-foreground"> · not picked by auto</span>}</SetupRow>}
          {chat && <SetupRow label="Capabilities" href="#set-native">{caps.length ? caps.join(" · ") : <span className="text-muted-foreground">none declared</span>}</SetupRow>}
          {chat && <SetupRow label="Boons" href="#set-boons">{model.boons?.length ? model.boons.map((b, i) => <span key={b}>{i > 0 && " · "}{b.replace(/_/g, " ")}{inactiveBoons.includes(b) && <span className="text-muted-foreground"> (off: needs function calling)</span>}</span>) : <span className="text-muted-foreground">none</span>}</SetupRow>}
          <SetupRow label="Price" href="#set-price"><span className="font-mono text-[12px]">{price ? `${price}${chat ? " in · out per 1M" : ""}` : "not set"}</span></SetupRow>
        </section>
        <section id="activity" aria-label="Recent changes" className="scroll-mt-24 rounded-xl border border-border bg-card px-[18px] pb-2 pt-4">
          <div className="flex items-center justify-between pb-1.5">
            <h2 className="text-sm font-semibold">Recent changes</h2>
            <Link href={auditHref("model", model.id)} className="text-[12.5px] text-secondary-foreground hover:text-foreground">Audit log ›</Link>
          </div>
          {!data ? (
            <div className="space-y-2 pb-2"><div className="skeleton h-4" /><div className="skeleton h-4" /></div>
          ) : data.audit.length === 0 ? (
            <p className="pb-2 text-xs text-muted-foreground">No recorded changes.</p>
          ) : (
            data.audit.map((e) => {
              const d = describeAudit(e, names);
              return (
                <div key={e.id} className="grid grid-cols-[minmax(0,1fr)_auto] items-baseline gap-3 border-t border-border/70 py-2 text-[13px] first:border-t-0">
                  <span className="min-w-0 truncate"><span className="font-medium">{d.actor}</span> <span className="text-muted-foreground">{d.action}</span></span>
                  <span className="font-mono text-[11.5px] text-muted-foreground">{ago(e.ts, data.now)}</span>
                </div>
              );
            })
          )}
        </section>
      </div>
    </div>
  );
}
