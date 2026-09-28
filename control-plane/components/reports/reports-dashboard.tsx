"use client";

import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import type { DateRange } from "react-day-picker";
import { Bar, CartesianGrid, ComposedChart, Line, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";
import { ArrowDown, ArrowUp, Download, Search } from "lucide-react";
import { axisTick, chartGrid, compactAxis } from "@/components/chart-tooltip";
import { ExportSheet } from "@/components/reports/export-sheet";
import { Meter, Panel, Segmented } from "@/components/overview/ui";
import { Tile } from "@/components/models/ui";
import { Button } from "@/components/ui/button";
import { Calendar } from "@/components/ui/calendar";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Select } from "@/components/ui/select";
import type { ApiKey, Tenant, UsageDailyGroupBy, UsageDailyRow } from "@/lib/obleth";
import { compact, formatMs } from "@/lib/overview-model";
import {
  buildChart,
  change,
  daysBetween,
  isoDay,
  MEASURES,
  presetRange,
  previousRange,
  RANGE_PRESETS,
  rangeLabel,
  sortRows,
  TABLE_GROUPS,
  totals,
  type DayRange,
  type Measure,
  type RangePreset,
  type SortColumn,
  type Split,
  type TableGroup,
} from "@/lib/reports-model";
import { toBreakdownRows } from "@/lib/usage-breakdown";
import { cn, getJson } from "@/lib/utils";

const PREVIOUS = "hsl(240 4% 46%)";

function money(v: number) {
  if (!v) return "$0";
  if (v < 0.01) return "< $0.01";
  if (v >= 10_000) return `$${compact(v)}`;
  return `$${v.toLocaleString(undefined, { minimumFractionDigits: v >= 100 ? 0 : 2, maximumFractionDigits: v >= 100 ? 0 : 2 })}`;
}

function energy(wh: number) {
  return wh >= 1000 ? `${(wh / 1000).toLocaleString(undefined, { maximumFractionDigits: 1 })} kWh` : `${Math.round(wh)} Wh`;
}

function co2(g: number) {
  return g >= 1000 ? `${(g / 1000).toLocaleString(undefined, { maximumFractionDigits: 1 })} kg` : `${Math.round(g)} g`;
}

function useDaily(range: DayRange | null, filters: { tenantId: string; keyId: string; model: string }, group: UsageDailyGroupBy, enabled = true) {
  return useQuery({
    queryKey: ["usage-daily", range?.start, range?.end, filters.tenantId, filters.keyId, filters.model, group],
    enabled: enabled && !!range,
    queryFn: () => {
      const p = new URLSearchParams({ start_day: range!.start, end_day: range!.end, group_by: group });
      if (filters.tenantId) p.set("tenant_id", filters.tenantId);
      if (filters.keyId) p.set("key_id", filters.keyId);
      if (filters.model) p.set("model", filters.model);
      return getJson<UsageDailyRow[]>(`/api/live/usage/daily?${p}`);
    },
    staleTime: 60_000,
  });
}

function measureValue(m: Measure, v: number) {
  return m === "spend" ? money(v) : m === "ttft" ? formatMs(v) : compact(v);
}

function ChartTip({ active, payload, label, measure, series }: { active?: boolean; payload?: { dataKey: string; value: number; payload: Record<string, number | string | null> }[]; label?: string; measure: Measure; series: { id: string; label: string; color: string }[] }) {
  if (!active || !payload?.length) return null;
  const row = payload[0].payload;
  return (
    <div className="min-w-48 rounded-lg border border-border bg-[hsl(240_5%_9%)] px-3 py-2 text-xs shadow-xl">
      <p className="mb-1.5 font-mono text-muted-foreground">{label}</p>
      {series.length ? (
        series.map((s) => (
          <div key={s.id} className="flex justify-between gap-6">
            <span className="inline-flex items-center gap-1.5"><span className="h-2 w-2 rounded-[2px]" style={{ background: s.color }} />{s.label}</span>
            <span className="font-mono tabular-nums">{measureValue(measure, Number(row[s.id] ?? 0))}</span>
          </div>
        ))
      ) : (
        <>
          <div className="flex justify-between gap-6"><span>This period</span><span className="font-mono tabular-nums">{measureValue(measure, Number(row.value ?? 0))}</span></div>
          {row.previous !== null && row.previous !== undefined && <div className="flex justify-between gap-6 text-muted-foreground"><span>Same day, previous period</span><span className="font-mono tabular-nums">{measureValue(measure, Number(row.previous))}</span></div>}
        </>
      )}
    </div>
  );
}

export function ReportsDashboard({ tenants, keys, models }: { tenants: Tenant[]; keys: ApiKey[]; models: string[] }) {
  const [preset, setPreset] = useState<RangePreset>("month");
  const [custom, setCustom] = useState<DateRange | undefined>();
  const [picker, setPicker] = useState(false);
  const [tenantId, setTenantId] = useState("");
  const [keyId, setKeyId] = useState("");
  const [model, setModel] = useState("");
  const [measure, setMeasure] = useState<Measure>("spend");
  const [split, setSplit] = useState<Split>("tenant");
  const [group, setGroup] = useState<TableGroup>("tenant");
  const [sort, setSort] = useState<{ col: SortColumn; desc: boolean }>({ col: "cost_usd", desc: true });
  const [rowFilter, setRowFilter] = useState("");
  const [exporting, setExporting] = useState(false);

  const range: DayRange | null = preset === "custom"
    ? custom?.from ? { start: isoDay(custom.from), end: isoDay(custom.to ?? custom.from) } : null
    : presetRange(preset);
  const prev = range ? previousRange(range, preset) : null;
  const filters = { tenantId, keyId, model };
  const against = preset === "month" ? "the same days last month" : preset === "last-month" ? "the month before" : "the previous period";

  const names = useMemo(() => ({
    tenantNames: new Map(tenants.map((t) => [t.id, t.name])),
    keyNames: new Map(keys.map((k) => [k.id, k.name])),
    keyPrefixes: new Map(keys.map((k) => [k.id, k.key_prefix])),
  }), [tenants, keys]);

  const days = useDaily(range, filters, "day");
  const prevDays = useDaily(prev, filters, "day");
  const splitGroup: UsageDailyGroupBy = split === "model" ? "day_model" : "day_tenant";
  const splitRows = useDaily(range, filters, splitGroup, split !== "none" && measure !== "ttft");
  const teams = useDaily(range, filters, "tenant");
  const modelRows = useDaily(range, filters, "model");
  const table = useDaily(range, filters, group);

  const cur = totals(days.data ?? []);
  const before = prevDays.data ? totals(prevDays.data) : null;
  const lookups = cur.cacheHits + cur.cacheMisses;
  const successRate = cur.requests ? (cur.succeeded / cur.requests) * 100 : 0;
  const prevSuccess = before && before.requests ? (before.succeeded / before.requests) * 100 : null;

  const labelFor = (r: UsageDailyRow) => (split === "tenant" ? names.tenantNames.get(r.tenant_id) ?? "Deleted team" : r.model || "(unknown)");
  const chart = range ? buildChart(range, measure, split, days.data ?? [], prev && prevDays.data ? { range: prev, rows: prevDays.data } : null, splitRows.data ?? [], labelFor) : { days: [], series: [] };
  const chartRows = chart.days.map((d) => ({ label: new Date(`${d.day}T00:00`).toLocaleDateString([], { month: "short", day: "numeric" }), value: d.value, previous: d.previous, ...d.parts }));
  const chartEmpty = chart.days.every((d) => !d.value);

  const teamRank = [...(teams.data ?? [])].sort((a, b) => b.cost_usd - a.cost_usd);
  const teamSpend = teamRank.reduce((n, r) => n + r.cost_usd, 0);
  const modelRank = [...(modelRows.data ?? [])].sort((a, b) => b.total_tokens - a.total_tokens);
  const q = rowFilter.trim().toLowerCase();
  const labeled = sortRows(toBreakdownRows(table.data ?? [], group, names), sort.col, sort.desc).filter((r) => !q || `${r.label} ${r.sublabel}`.toLowerCase().includes(q));
  const tableTotal = totals(labeled);
  const maxSpend = Math.max(...labeled.map((r) => r.cost_usd), 0);
  const tenantKeys = tenantId ? keys.filter((k) => k.tenant_id === tenantId) : [];
  const filterLine = [tenantId ? names.tenantNames.get(tenantId) : "every team", keyId ? `key ${names.keyNames.get(keyId) || names.keyPrefixes.get(keyId)}` : null, model || null].filter(Boolean).join(" · ");
  const groupInfo = TABLE_GROUPS.find((g) => g.value === group)!;

  const drill = (r: UsageDailyRow) => {
    if (group === "tenant") { setTenantId(r.tenant_id); setKeyId(""); setGroup("key"); }
    else if (group === "key") { setTenantId(r.tenant_id); setKeyId(r.key_id); setGroup("model"); }
    else if (group === "model") { setModel(r.model); setGroup("tenant"); }
  };
  const header = (col: SortColumn, label: string, right = true) => (
    <th className={cn("py-2.5 pr-3 font-semibold", right && "text-right")}>
      <button type="button" onClick={() => setSort((s) => ({ col, desc: s.col === col ? !s.desc : col !== "label" }))} className={cn("inline-flex items-center gap-1 uppercase hover:text-foreground", sort.col === col && "text-foreground")}>
        {label}
        {sort.col === col && (sort.desc ? <ArrowDown className="h-3 w-3" /> : <ArrowUp className="h-3 w-3" />)}
      </button>
    </th>
  );

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-5">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-[26px] font-semibold tracking-tight">Reports</h1>
          <p className="mt-1 text-[13px] text-muted-foreground">
            {range ? `${rangeLabel(range)} · ${filterLine}${prev ? ` · compared with ${rangeLabel(prev)}` : ""}` : "Pick the days to report on"}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Segmented<RangePreset> label="Date range" value={preset} onChange={(v) => { setPreset(v); if (v === "custom") setPicker(true); }} options={RANGE_PRESETS.map((r) => ({ value: r.value, label: r.value === "custom" && preset === "custom" && range ? rangeLabel(range) : r.label }))} />
          <Select aria-label="Team" value={tenantId} onValueChange={(v) => { setTenantId(v); setKeyId(""); }} searchPlaceholder="Find a team" className={cn("h-9 w-auto min-w-[9rem] text-[12.5px]", tenantId && "border-foreground")} options={[{ value: "", label: "All teams" }, ...tenants.map((t) => ({ value: t.id, label: t.name }))]} />
          {tenantId && <Select aria-label="Key" value={keyId} onValueChange={setKeyId} searchPlaceholder="Find a key" className={cn("h-9 w-auto min-w-[8rem] text-[12.5px]", keyId && "border-foreground")} options={[{ value: "", label: "All keys" }, ...tenantKeys.map((k) => ({ value: k.id, label: k.name || k.key_prefix, hint: k.key_prefix }))]} />}
          <Select aria-label="Model" value={model} onValueChange={setModel} searchPlaceholder="Find a model" className={cn("h-9 w-auto min-w-[9rem] text-[12.5px]", model && "border-foreground")} options={[{ value: "", label: "All models" }, ...models.map((m) => ({ value: m, label: m }))]} />
          <Button type="button" size="sm" className="h-9" disabled={!range} onClick={() => setExporting(true)}><Download className="h-3.5 w-3.5" />Export</Button>
        </div>
      </div>

      <Dialog open={picker} onOpenChange={setPicker}>
        <DialogContent className="max-w-fit">
          <DialogHeader><DialogTitle>Pick the days</DialogTitle></DialogHeader>
          <Calendar mode="range" numberOfMonths={2} selected={custom} onSelect={setCustom} disabled={{ after: new Date() }} />
          <div className="flex justify-end"><Button size="sm" onClick={() => setPicker(false)} disabled={!custom?.from}>Done</Button></div>
        </DialogContent>
      </Dialog>

      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-5">
        <Tile label="Requests" value={days.data ? compact(cur.requests) : "—"} detail={change(cur.requests, before?.requests, against)} />
        <Tile label="Tokens" value={days.data ? compact(cur.tokens) : "—"} detail={cur.tokens ? `${Math.round((cur.inputTokens / cur.tokens) * 100)}% in · ${change(cur.tokens, before?.tokens, "before") ?? ""}` : null} />
        <Tile label="Spend" value={days.data ? money(cur.cost) : "—"} detail={range && cur.cost ? `${money(cur.cost / daysBetween(range))} a day · ${change(cur.cost, before?.cost, "before") ?? ""}` : null} />
        <Tile
          label="Succeeded"
          value={cur.requests ? `${successRate.toFixed(1)}%` : "—"}
          detail={cur.requests ? `${compact(cur.failed)} failed${prevSuccess !== null ? ` · ${successRate - prevSuccess >= 0 ? "▲" : "▼"} ${Math.abs(successRate - prevSuccess).toFixed(1)} pts` : ""}` : null}
          emphasis={cur.requests > 0 && successRate < 95}
        />
        <Tile label="Cache hits" value={lookups ? `${((cur.cacheHits / lookups) * 100).toFixed(1)}%` : "—"} detail={lookups ? `${compact(cur.cacheHits)} of ${compact(lookups)} lookups` : "No cached lookups"} />
      </div>
      {cur.energyWh > 0 && (
        <p className="-mt-2 text-xs text-muted-foreground">
          Energy {energy(cur.energyWh)} · {co2(cur.co2G)} CO₂ · {money(cur.energyCost)} of power, for the models with energy accounting on · average first token {formatMs(cur.avgTtftMs)}
        </p>
      )}

      <Panel
        label="Usage over time"
        title={`${MEASURES.find((m) => m.value === measure)!.label} per day`}
        subtitle={chart.series.length ? `Split by ${split === "tenant" ? "team" : "model"}: the four largest, then the rest` : measure === "ttft" ? "Average per day; the dashed line is the same day in the previous period" : "Bars are this period; the dashed line is the same day in the previous period"}
        action={
          <div className="flex flex-wrap items-center gap-2">
            <Segmented<Measure> label="Measure" value={measure} onChange={setMeasure} options={MEASURES} />
            <Segmented<Split> label="Split by" value={split} onChange={setSplit} options={[{ value: "none", label: "No split" }, { value: "tenant", label: "Team" }, { value: "model", label: "Model" }]} />
          </div>
        }
      >
        <div className="px-[18px] pb-4 pt-3">
          {days.isLoading ? (
            <div className="skeleton h-[260px] rounded-lg" />
          ) : chartEmpty ? (
            <div className="flex h-[260px] items-center justify-center rounded-lg border border-dashed border-border text-sm text-muted-foreground">No usage in these days</div>
          ) : (
            <div className="h-[260px]">
              <ResponsiveContainer width="100%" height="100%">
                <ComposedChart data={chartRows} margin={{ top: 8, right: 4, left: 0, bottom: 0 }} barCategoryGap="18%">
                  <CartesianGrid {...chartGrid} vertical={false} />
                  <XAxis dataKey="label" tick={axisTick} axisLine={false} tickLine={false} minTickGap={24} height={20} />
                  <YAxis tick={axisTick} axisLine={false} tickLine={false} width={52} tickFormatter={(v: number) => (measure === "spend" ? `$${compactAxis(v)}` : measure === "ttft" ? formatMs(v) : compactAxis(v))} />
                  <Tooltip cursor={{ fill: "hsl(240 4% 16% / 0.5)" }} content={<ChartTip measure={measure} series={chart.series} />} />
                  {chart.series.length ? (
                    chart.series.map((s, i) => <Bar key={s.id} dataKey={s.id} stackId="split" fill={s.color} isAnimationActive={false} radius={i === chart.series.length - 1 ? [3, 3, 0, 0] : 0} />)
                  ) : (
                    <Bar dataKey="value" fill="hsl(240 5% 82%)" radius={[3, 3, 0, 0]} isAnimationActive={false} />
                  )}
                  {!chart.series.length && <Line type="step" dataKey="previous" stroke={PREVIOUS} strokeWidth={2} strokeDasharray="5 4" dot={false} isAnimationActive={false} connectNulls={false} />}
                </ComposedChart>
              </ResponsiveContainer>
            </div>
          )}
          <div className="flex flex-wrap gap-4 pt-3 text-xs text-secondary-foreground">
            {chart.series.length ? chart.series.map((s) => <span key={s.id} className="inline-flex items-center gap-1.5"><span className="h-2.5 w-2.5 rounded-[2px]" style={{ background: s.color }} />{s.label}</span>) : (
              <>
                <span className="inline-flex items-center gap-1.5"><span className="h-2.5 w-2.5 rounded-[2px] bg-[hsl(240_5%_82%)]" />This period</span>
                <span className="inline-flex items-center gap-1.5"><span className="w-4 border-t-2 border-dashed border-muted-foreground" />Same day, previous period</span>
              </>
            )}
          </div>
        </div>
      </Panel>

      <div className="grid gap-4 lg:grid-cols-2">
        <Panel title="Teams by spend" subtitle="Click a team to see only its usage">
          <div className="px-[18px] pb-2 pt-2">
            {teamRank.length === 0 ? <p className="py-6 text-center text-xs text-muted-foreground">{teams.isLoading ? "Loading…" : "No usage in these days."}</p> : (
              <>
                {teamRank.slice(0, 6).map((r) => (
                  <button key={r.tenant_id} type="button" onClick={() => { setTenantId(r.tenant_id); setKeyId(""); }} className="grid w-full grid-cols-[minmax(0,1fr)_120px_72px_40px] items-center gap-3 border-t border-border/70 py-2 text-left text-[13px] first:border-t-0 hover:text-foreground">
                    <span className="truncate">{names.tenantNames.get(r.tenant_id) ?? "Deleted team"}</span>
                    <Meter value={r.cost_usd} max={teamRank[0].cost_usd || 1} />
                    <span className="text-right font-mono text-[12px]">{money(r.cost_usd)}</span>
                    <span className="text-right text-[12px] text-muted-foreground">{teamSpend ? `${Math.round((r.cost_usd / teamSpend) * 100)}%` : "—"}</span>
                  </button>
                ))}
                {teamRank.length > 6 && <p className="border-t border-border/70 py-2 text-[12.5px] text-muted-foreground">{teamRank.length - 6} more teams · {money(teamRank.slice(6).reduce((n, r) => n + r.cost_usd, 0))}</p>}
              </>
            )}
          </div>
        </Panel>
        <Panel title="Models by tokens" subtitle="Spend beside each">
          <div className="px-[18px] pb-2 pt-2">
            {modelRank.length === 0 ? <p className="py-6 text-center text-xs text-muted-foreground">{modelRows.isLoading ? "Loading…" : "No usage in these days."}</p> : (
              <>
                {modelRank.slice(0, 6).map((r) => (
                  <button key={r.model} type="button" onClick={() => setModel(r.model)} className="grid w-full grid-cols-[minmax(0,1fr)_120px_64px_56px] items-center gap-3 border-t border-border/70 py-2 text-left text-[13px] first:border-t-0 hover:text-foreground">
                    <span className="truncate font-mono text-[12.5px]">{r.model || "(unknown)"}</span>
                    <Meter value={r.total_tokens} max={modelRank[0].total_tokens || 1} />
                    <span className="text-right font-mono text-[12px]">{compact(r.total_tokens)}</span>
                    <span className="text-right text-[12px] text-muted-foreground">{money(r.cost_usd)}</span>
                  </button>
                ))}
                {modelRank.length > 6 && <p className="border-t border-border/70 py-2 text-[12.5px] text-muted-foreground">{modelRank.length - 6} more models · {compact(modelRank.slice(6).reduce((n, r) => n + r.total_tokens, 0))} tokens</p>}
              </>
            )}
          </div>
        </Panel>
      </div>

      <section aria-label="Breakdown" className="overflow-hidden rounded-xl border border-border bg-card">
        <div className="flex flex-wrap items-center justify-between gap-3 px-[18px] py-3.5">
          <div className="flex flex-wrap items-center gap-3">
            <h2 className="text-sm font-semibold">Breakdown</h2>
            <Segmented<TableGroup> label="One row per" value={group} onChange={(g) => { setGroup(g); setSort(g === "day" ? { col: "label", desc: false } : { col: "cost_usd", desc: true }); }} options={TABLE_GROUPS.map((g) => ({ value: g.value, label: g.label }))} />
          </div>
          <label className="flex h-8 w-60 items-center gap-2 rounded-lg border border-border px-2.5 text-[12.5px]">
            <Search className="h-3.5 w-3.5 text-muted-foreground" aria-hidden />
            <input value={rowFilter} onChange={(e) => setRowFilter(e.target.value)} placeholder="Filter rows" aria-label="Filter rows" className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground" />
          </label>
        </div>
        <div className="overflow-x-auto">
          <table className="w-full min-w-[1080px] text-[13px]">
            <thead>
              <tr className="border-y border-border text-left text-[11px] tracking-[0.07em] text-muted-foreground">
                <th className="py-2.5 pl-[18px] pr-3 text-left font-semibold">
                  <button type="button" onClick={() => setSort((s) => ({ col: "label", desc: s.col === "label" ? !s.desc : false }))} className={cn("inline-flex items-center gap-1 uppercase hover:text-foreground", sort.col === "label" && "text-foreground")}>
                    {groupInfo.label}{sort.col === "label" && (sort.desc ? <ArrowDown className="h-3 w-3" /> : <ArrowUp className="h-3 w-3" />)}
                  </button>
                </th>
                {header("requests", "Requests")}
                {header("error_requests", "Failed")}
                {header("input_tokens", "Tokens in")}
                {header("output_tokens", "Tokens out")}
                {header("cost_usd", "Spend")}
                <th className="py-2.5 pr-3 font-semibold uppercase">Share of spend</th>
                {header("avg_ttft_ms", "First token")}
                <th className="py-2.5 pr-[18px] text-right font-semibold">
                  <button type="button" onClick={() => setSort((s) => ({ col: "energy_wh", desc: s.col === "energy_wh" ? !s.desc : true }))} className={cn("inline-flex items-center gap-1 uppercase hover:text-foreground", sort.col === "energy_wh" && "text-foreground")}>
                    Energy{sort.col === "energy_wh" && (sort.desc ? <ArrowDown className="h-3 w-3" /> : <ArrowUp className="h-3 w-3" />)}
                  </button>
                </th>
              </tr>
            </thead>
            <tbody>
              {labeled.map((r) => {
                const drillable = group === "tenant" || group === "key" || group === "model";
                return (
                  <tr key={`${r.day}|${r.tenant_id}|${r.key_id}|${r.model}`} onClick={drillable ? () => drill(r) : undefined} className={cn("border-t border-border/70", drillable && "cursor-pointer hover:bg-muted/30")}>
                    <td className="max-w-[22rem] py-2.5 pl-[18px] pr-3">
                      <span className={cn("truncate", (group === "day" || group === "model" || group === "key_model") && "font-mono text-[12.5px]")}>{r.label}</span>
                      {r.sublabel && <span className="ml-2 font-mono text-[11.5px] text-muted-foreground">{r.sublabel}</span>}
                    </td>
                    <td className="py-2.5 pr-3 text-right font-mono text-[12px]">{r.requests.toLocaleString()}</td>
                    <td className="py-2.5 pr-3 text-right font-mono text-[12px] text-muted-foreground">{r.error_requests.toLocaleString()}</td>
                    <td className="py-2.5 pr-3 text-right font-mono text-[12px]">{compact(r.input_tokens)}</td>
                    <td className="py-2.5 pr-3 text-right font-mono text-[12px]">{compact(r.output_tokens)}</td>
                    <td className="py-2.5 pr-3 text-right font-mono text-[12px]">{money(r.cost_usd)}</td>
                    <td className="py-2.5 pr-3">
                      <div className="flex items-center gap-2">
                        <Meter value={r.cost_usd} max={maxSpend || 1} className="flex-1" />
                        <span className="w-9 text-right text-[11.5px] text-muted-foreground">{tableTotal.cost ? `${Math.round((r.cost_usd / tableTotal.cost) * 100)}%` : "—"}</span>
                      </div>
                    </td>
                    <td className="py-2.5 pr-3 text-right font-mono text-[12px] text-muted-foreground">{r.avg_ttft_ms ? formatMs(r.avg_ttft_ms) : "—"}</td>
                    <td className="py-2.5 pr-[18px] text-right font-mono text-[12px] text-muted-foreground">{r.energy_wh ? energy(r.energy_wh) : "—"}</td>
                  </tr>
                );
              })}
              {labeled.length === 0 && (
                <tr><td colSpan={9} className="py-10 text-center text-muted-foreground">{table.isLoading ? "Loading…" : q ? `No ${groupInfo.plural} match “${rowFilter}”.` : "No usage in these days."}</td></tr>
              )}
            </tbody>
            {labeled.length > 0 && (
              <tfoot>
                <tr className="border-t border-border bg-muted/20 font-medium">
                  <td className="py-2.5 pl-[18px] pr-3">{q ? `${labeled.length} matching ${groupInfo.plural}` : `All ${labeled.length} ${groupInfo.plural}`}</td>
                  <td className="py-2.5 pr-3 text-right font-mono text-[12px]">{tableTotal.requests.toLocaleString()}</td>
                  <td className="py-2.5 pr-3 text-right font-mono text-[12px]">{tableTotal.failed.toLocaleString()}</td>
                  <td className="py-2.5 pr-3 text-right font-mono text-[12px]">{compact(tableTotal.inputTokens)}</td>
                  <td className="py-2.5 pr-3 text-right font-mono text-[12px]">{compact(tableTotal.outputTokens)}</td>
                  <td className="py-2.5 pr-3 text-right font-mono text-[12px]">{money(tableTotal.cost)}</td>
                  <td />
                  <td className="py-2.5 pr-3 text-right font-mono text-[12px]">{tableTotal.avgTtftMs ? formatMs(tableTotal.avgTtftMs) : "—"}</td>
                  <td className="py-2.5 pr-[18px] text-right font-mono text-[12px]">{tableTotal.energyWh ? energy(tableTotal.energyWh) : "—"}</td>
                </tr>
              </tfoot>
            )}
          </table>
        </div>
      </section>

      {range && (
        <ExportSheet
          open={exporting}
          onClose={() => setExporting(false)}
          range={range}
          filters={filters}
          filterLine={filterLine}
          names={names}
        />
      )}
    </div>
  );
}
