"use client";

import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { format } from "date-fns";
import type { DateRange } from "react-day-picker";
import { CalendarDays, Download, Loader2 } from "lucide-react";
import {
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  ComposedChart,
  Legend,
  Line,
  Pie,
  PieChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import {
  axisTick,
  chartGrid,
  ChartShell,
  compactAxis,
  tip,
  timeCursor,
} from "@/components/chart-tooltip";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Calendar } from "@/components/ui/calendar";
import { formatNumber } from "@/lib/utils";
import { Select } from "@/components/ui/select";
import type { ApiKey, Tenant, UsageDailyGroupBy, UsageDailyRow } from "@/lib/obleth";
import {
  formatUsd,
  toBreakdownRows,
  type BreakdownGroup,
} from "@/lib/usage-breakdown";

const REQUESTS_COLOR = "hsl(38 75% 60%)";
const TOKENS_COLOR = "hsl(205 55% 58%)";
const INPUT_COLOR = "hsl(205 55% 58%)";
const OUTPUT_COLOR = "hsl(158 42% 48%)";
const TTFT_COLOR = "hsl(38 75% 60%)";
const TOTAL_COLOR = "hsl(278 42% 62%)";
const SUCCESS_COLOR = "hsl(158 42% 48%)";
const ERROR_COLOR = "hsl(350 55% 58%)";

// Shared palette for categorical charts (top models). Mirrors the overview.
const PALETTE = [
  "hsl(158 42% 48%)",
  "hsl(205 55% 58%)",
  "hsl(38 60% 56%)",
  "hsl(278 42% 62%)",
  "hsl(350 50% 60%)",
  "hsl(185 42% 52%)",
];
const OTHERS_COLOR = "hsl(240 6% 42%)";

const TOP_MODELS = 8;

const DAY = 86_400_000;

// Every column the CSV export can emit, with a human label. Order mirrors the
// server's ALL_COLUMNS allowlist. `def` marks the default-checked set.
const EXPORT_COLUMNS: { key: string; label: string; def: boolean }[] = [
  { key: "start_day", label: "Start date", def: true },
  { key: "end_day", label: "End date", def: true },
  { key: "tenant_id", label: "Tenant ID", def: false },
  { key: "tenant_name", label: "Tenant name", def: true },
  { key: "key_id", label: "Key ID", def: false },
  { key: "key_name", label: "Key name", def: true },
  { key: "key_prefix", label: "Key prefix", def: false },
  { key: "model", label: "Model", def: true },
  { key: "requests", label: "Requests", def: true },
  { key: "success_requests", label: "Successful", def: false },
  { key: "error_requests", label: "Errors", def: false },
  { key: "input_tokens", label: "Input tokens", def: false },
  { key: "output_tokens", label: "Output tokens", def: false },
  { key: "total_tokens", label: "Total tokens", def: true },
  { key: "estimated_tokens", label: "Estimated tokens", def: false },
  { key: "cache_hits", label: "Cache hits", def: false },
  { key: "cache_misses", label: "Cache misses", def: false },
  { key: "avg_ttft_ms", label: "Avg TTFB (ms)", def: false },
  { key: "avg_total_ms", label: "Avg total (ms)", def: false },
  { key: "cost_usd", label: "Spend (USD)", def: true },
  { key: "energy_kwh", label: "Energy (kWh)", def: false },
  { key: "co2_g", label: "CO₂ (g)", def: false },
  { key: "energy_cost_usd", label: "Energy cost (USD)", def: false },
];

// Header label for the breakdown table's first column, per grouping.
const GROUP_LABELS: Record<string, string> = {
  day: "Day",
  tenant: "Team",
  key: "Key",
  model: "Model",
};

function isoDay(d: Date): string {
  return format(d, "yyyy-MM-dd");
}

function formatEnergyKwh(wh: number): string {
  return `${(wh / 1000).toFixed(2)} kWh`;
}

function formatCo2(g: number): string {
  if (g >= 1000) return `${(g / 1000).toFixed(2)} kg`;
  return `${g.toFixed(2)} g`;
}

function defaultRange(): DateRange {
  const to = new Date();
  const from = new Date(Date.now() - 6 * DAY);
  return { from, to };
}

export function ReportsDashboard({ tenants, keys }: { tenants: Tenant[]; keys: ApiKey[] }) {
  const [range, setRange] = useState<DateRange | undefined>(defaultRange);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [exportOpen, setExportOpen] = useState(false);
  const [exportGroup, setExportGroup] = useState<UsageDailyGroupBy>("key_model");
  const [columns, setColumns] = useState<Set<string>>(
    () => new Set(EXPORT_COLUMNS.filter((c) => c.def).map((c) => c.key)),
  );

  const [tenantId, setTenantId] = useState("");
  const [keyId, setKeyId] = useState("");
  const [tableGroup, setTableGroup] = useState<BreakdownGroup>("day");

  // Keys shown in the key filter — only the selected tenant's.
  const tenantKeys = useMemo(
    () => (tenantId ? keys.filter((k) => k.tenant_id === tenantId) : []),
    [keys, tenantId],
  );

  const tenantNames = useMemo(() => new Map(tenants.map((t) => [t.id, t.name])), [tenants]);
  const keyNames = useMemo(() => new Map(keys.map((k) => [k.id, k.name])), [keys]);
  const keyPrefixes = useMemo(() => new Map(keys.map((k) => [k.id, k.key_prefix])), [keys]);

  function selectTenant(id: string) {
    setTenantId(id);
    setKeyId(""); // a key belongs to one tenant; changing tenant invalidates it
  }

  const startDay = range?.from ? isoDay(range.from) : undefined;
  const endDay = range?.to ? isoDay(range.to) : startDay;

  function dailyUrl(group: string): string {
    const params = new URLSearchParams({
      start_day: startDay ?? "",
      end_day: endDay ?? "",
      group_by: group,
    });
    if (tenantId) params.set("tenant_id", tenantId);
    if (keyId) params.set("key_id", keyId);
    return `/api/live/usage/daily?${params.toString()}`;
  }

  const query = useQuery({
    queryKey: ["usage-daily", startDay, endDay, tenantId, keyId, "day"],
    enabled: Boolean(startDay && endDay),
    queryFn: async (): Promise<UsageDailyRow[]> => {
      const res = await fetch(dailyUrl("day"));
      if (!res.ok) throw new Error(`Failed to load usage (${res.status})`);
      return res.json();
    },
  });

  // Separate model-grouped read powers the "top models" chart without forcing
  // the day series to carry per-model rows.
  const modelQuery = useQuery({
    queryKey: ["usage-daily", startDay, endDay, tenantId, keyId, "model"],
    enabled: Boolean(startDay && endDay),
    queryFn: async (): Promise<UsageDailyRow[]> => {
      const res = await fetch(dailyUrl("model"));
      if (!res.ok) throw new Error(`Failed to load models (${res.status})`);
      return res.json();
    },
  });

  // Shares the ["usage-daily", ...] key family, so the "day" grouping dedupes
  // against the chart query instead of refetching.
  const tableQuery = useQuery({
    queryKey: ["usage-daily", startDay, endDay, tenantId, keyId, tableGroup],
    enabled: Boolean(startDay && endDay),
    queryFn: async (): Promise<UsageDailyRow[]> => {
      const res = await fetch(dailyUrl(tableGroup));
      if (!res.ok) throw new Error(`Failed to load breakdown (${res.status})`);
      return res.json();
    },
  });

  const breakdownRows = useMemo(
    () =>
      toBreakdownRows(tableQuery.data ?? [], tableGroup, {
        tenantNames,
        keyNames,
        keyPrefixes,
      }),
    [tableQuery.data, tableGroup, tenantNames, keyNames, keyPrefixes],
  );

  const rows = useMemo(() => query.data ?? [], [query.data]);

  const totals = useMemo(() => {
    return rows.reduce(
      (acc, r) => {
        acc.requests += r.requests;
        acc.tokens += r.total_tokens;
        acc.inputTokens += r.input_tokens;
        acc.outputTokens += r.output_tokens;
        acc.errors += r.error_requests;
        acc.cacheHits += r.cache_hits;
        acc.cacheMisses += r.cache_misses;
        acc.energyWh += r.energy_wh;
        acc.co2G += r.co2_g;
        acc.costUsd += r.cost_usd;
        return acc;
      },
      {
        requests: 0,
        tokens: 0,
        inputTokens: 0,
        outputTokens: 0,
        errors: 0,
        cacheHits: 0,
        cacheMisses: 0,
        energyWh: 0,
        co2G: 0,
        costUsd: 0,
      },
    );
  }, [rows]);

  const successRate =
    totals.requests > 0 ? ((totals.requests - totals.errors) / totals.requests) * 100 : 0;
  const cacheLookups = totals.cacheHits + totals.cacheMisses;
  const cacheHitRate = cacheLookups > 0 ? (totals.cacheHits / cacheLookups) * 100 : 0;

  // Wider bars when the range is short so a 7-day window doesn't look bare.
  const barSize = rows.length <= 10 ? 64 : rows.length <= 20 ? 40 : 24;

  const outcomes = useMemo(
    () => [
      { name: "Successful", value: totals.requests - totals.errors, fill: SUCCESS_COLOR },
      { name: "Errors", value: totals.errors, fill: ERROR_COLOR },
    ],
    [totals.requests, totals.errors],
  );

  const topModels = useMemo(() => {
    const data = modelQuery.data ?? [];
    const sorted = [...data].filter((r) => r.total_tokens > 0).sort((a, b) => b.total_tokens - a.total_tokens);
    const top = sorted.slice(0, TOP_MODELS).map((r, i) => ({
      name: r.model || "(unknown)",
      total_tokens: r.total_tokens,
      requests: r.requests,
      fill: PALETTE[i % PALETTE.length],
    }));
    const rest = sorted.slice(TOP_MODELS);
    if (rest.length > 0) {
      top.push({
        name: `others (${rest.length})`,
        total_tokens: rest.reduce((s, r) => s + r.total_tokens, 0),
        requests: rest.reduce((s, r) => s + r.requests, 0),
        fill: OTHERS_COLOR,
      });
    }
    return top;
  }, [modelQuery.data]);

  const rangeLabel =
    range?.from && range?.to
      ? `${format(range.from, "MMM d, yyyy")} \u2013 ${format(range.to, "MMM d, yyyy")}`
      : range?.from
        ? format(range.from, "MMM d, yyyy")
        : "Select a range";

  function toggleColumn(key: string) {
    setColumns((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }

  function exportCsv() {
    if (!startDay || !endDay) return;
    const ordered = EXPORT_COLUMNS.filter((c) => columns.has(c.key)).map((c) => c.key);
    if (ordered.length === 0) return;
    const params = new URLSearchParams({
      start_day: startDay,
      end_day: endDay,
      group_by: exportGroup,
      columns: ordered.join(","),
    });
    if (tenantId) params.set("tenant_id", tenantId);
    if (keyId) params.set("key_id", keyId);
    window.location.href = `/api/live/usage/export?${params.toString()}`;
    setExportOpen(false);
  }

  return (
    <div className="space-y-6">
      {/* Controls */}
      <div className="flex flex-wrap items-center gap-3">
        <Dialog open={pickerOpen} onOpenChange={setPickerOpen}>
          <DialogTrigger asChild>
            <Button variant="outline" className="gap-2">
              <CalendarDays className="h-4 w-4" />
              {rangeLabel}
            </Button>
          </DialogTrigger>
          <DialogContent className="max-w-fit">
            <DialogHeader>
              <DialogTitle>Select date range</DialogTitle>
            </DialogHeader>
            <Calendar
              mode="range"
              numberOfMonths={2}
              selected={range}
              onSelect={setRange}
              disabled={{ after: new Date() }}
            />
            <div className="flex justify-end">
              <Button size="sm" onClick={() => setPickerOpen(false)} disabled={!range?.from}>
                Done
              </Button>
            </div>
          </DialogContent>
        </Dialog>

        <Select
          value={tenantId}
          onChange={(e) => selectTenant(e.target.value)}
          aria-label="Filter by team"
          className="h-9 w-44 text-sm"
        >
          <option value="">All teams</option>
          {tenants.map((t) => (
            <option key={t.id} value={t.id}>
              {t.name}
            </option>
          ))}
        </Select>
        {tenantId && (
          <Select
            value={keyId}
            onChange={(e) => setKeyId(e.target.value)}
            aria-label="Filter by key"
            className="h-9 w-44 text-sm"
          >
            <option value="">All keys</option>
            {tenantKeys.map((k) => (
              <option key={k.id} value={k.id}>
                {k.name || k.key_prefix}
              </option>
            ))}
          </Select>
        )}

        {query.isFetching && (
          <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
            Loading
          </span>
        )}

        {/* Export lives at the top now; the column picker opens in a modal. */}
        <Dialog open={exportOpen} onOpenChange={setExportOpen}>
          <DialogTrigger asChild>
            <Button className="ml-auto gap-2" disabled={!startDay}>
              <Download className="h-4 w-4" />
              Export CSV
            </Button>
          </DialogTrigger>
          <DialogContent className="sm:max-w-lg">
            <DialogHeader>
              <DialogTitle className="flex items-center gap-2">
                <Download className="h-4 w-4" />
                Export CSV
              </DialogTitle>
            </DialogHeader>
            <p className="text-sm text-muted-foreground">
              Rows for {rangeLabel}
              {tenantId ? " (current team/key filter applies)" : ""}. Choose the row grouping
              and the columns to include.
            </p>
            <Select
              value={exportGroup}
              onChange={(e) => setExportGroup(e.target.value as UsageDailyGroupBy)}
              aria-label="Export row grouping"
              className="h-8 w-44 text-xs"
            >
              <option value="key_model">Per key + model</option>
              <option value="day">Per day</option>
              <option value="tenant">Per team</option>
              <option value="key">Per key</option>
              <option value="model">Per model</option>
            </Select>
            <div className="grid grid-cols-2 gap-x-6 gap-y-2 sm:grid-cols-3">
              {EXPORT_COLUMNS.map((c) => (
                <label key={c.key} className="flex items-center gap-2 text-sm">
                  <input
                    type="checkbox"
                    className="h-4 w-4 rounded border-border accent-[hsl(var(--primary))]"
                    checked={columns.has(c.key)}
                    onChange={() => toggleColumn(c.key)}
                  />
                  <span className="truncate">{c.label}</span>
                </label>
              ))}
            </div>
            <div className="flex items-center justify-between gap-3">
              <span className="text-xs text-muted-foreground">{columns.size} columns selected</span>
              <div className="flex gap-2">
                <Button variant="outline" size="sm" onClick={() => setExportOpen(false)}>
                  Cancel
                </Button>
                <Button
                  size="sm"
                  onClick={exportCsv}
                  disabled={columns.size === 0 || !startDay}
                  className="gap-2"
                >
                  <Download className="h-4 w-4" />
                  Download
                </Button>
              </div>
            </div>
          </DialogContent>
        </Dialog>
      </div>

      {/* KPI strip */}
      <div className="grid gap-4 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-8">
        <Kpi label="Requests" value={formatNumber(totals.requests)} />
        <Kpi label="Total tokens" value={formatNumber(totals.tokens)} />
        <Kpi label="Spend" value={formatUsd(totals.costUsd)} />
        <Kpi label="Success rate" value={`${successRate.toFixed(1)}%`} />
        <Kpi label="Errors" value={formatNumber(totals.errors)} />
        <Kpi
          label="Cache hit rate"
          value={cacheLookups > 0 ? `${cacheHitRate.toFixed(1)}%` : "\u2014"}
        />
        <Kpi
          label="Energy"
          value={totals.energyWh > 0 ? formatEnergyKwh(totals.energyWh) : "\u2014"}
        />
        <Kpi
          label="CO₂"
          value={totals.co2G > 0 ? formatCo2(totals.co2G) : "\u2014"}
        />
      </div>

      {/* Volume chart */}
      <Card>
        <CardHeader>
          <CardTitle>Daily volume</CardTitle>
          <CardDescription>Requests and total tokens per day.</CardDescription>
        </CardHeader>
        <CardContent>
          {rows.length === 0 ? (
            <p className="py-12 text-center text-sm text-muted-foreground">
              {query.isLoading ? "Loading\u2026" : "No usage in this range."}
            </p>
          ) : (
            <ChartShell heightClass="h-72">
              <ResponsiveContainer width="100%" height="100%">
                <ComposedChart data={rows} margin={{ top: 8, right: 8, bottom: 0, left: 0 }}>
                  <CartesianGrid {...chartGrid} vertical={false} />
                  <XAxis dataKey="day" tick={axisTick} tickLine={false} axisLine={false} />
                  <YAxis
                    yAxisId="left"
                    tick={axisTick}
                    tickLine={false}
                    axisLine={false}
                    tickFormatter={compactAxis}
                  />
                  <YAxis
                    yAxisId="right"
                    orientation="right"
                    tick={axisTick}
                    tickLine={false}
                    axisLine={false}
                    tickFormatter={compactAxis}
                  />
                  <Tooltip cursor={timeCursor} content={tip()} />
                  <Legend wrapperStyle={{ fontSize: 12 }} />
                  <Bar
                    yAxisId="left"
                    dataKey="requests"
                    name="Requests"
                    fill={REQUESTS_COLOR}
                    radius={[3, 3, 0, 0]}
                    barSize={barSize}
                  />
                  <Line
                    yAxisId="right"
                    type="monotone"
                    dataKey="total_tokens"
                    name="Total tokens"
                    stroke={TOKENS_COLOR}
                    strokeWidth={2}
                    dot={false}
                  />
                </ComposedChart>
              </ResponsiveContainer>
            </ChartShell>
          )}
        </CardContent>
      </Card>

      {/* Secondary charts */}
      <div className="grid gap-6 lg:grid-cols-2">
        {/* Token mix: input vs output per day */}
        <Card>
          <CardHeader>
            <CardTitle>Token mix</CardTitle>
            <CardDescription>Input vs. output tokens per day.</CardDescription>
          </CardHeader>
          <CardContent>
            {rows.length === 0 ? (
              <EmptyChart loading={query.isLoading} />
            ) : (
              <ChartShell heightClass="h-64">
                <ResponsiveContainer width="100%" height="100%">
                  <BarChart data={rows} margin={{ top: 8, right: 8, bottom: 0, left: 0 }}>
                    <CartesianGrid {...chartGrid} vertical={false} />
                    <XAxis dataKey="day" tick={axisTick} tickLine={false} axisLine={false} />
                    <YAxis
                      tick={axisTick}
                      tickLine={false}
                      axisLine={false}
                      tickFormatter={compactAxis}
                    />
                    <Tooltip cursor={timeCursor} content={tip()} />
                    <Legend wrapperStyle={{ fontSize: 12 }} />
                    <Bar
                      stackId="tok"
                      dataKey="input_tokens"
                      name="Input"
                      fill={INPUT_COLOR}
                      barSize={barSize}
                    />
                    <Bar
                      stackId="tok"
                      dataKey="output_tokens"
                      name="Output"
                      fill={OUTPUT_COLOR}
                      radius={[3, 3, 0, 0]}
                      barSize={barSize}
                    />
                  </BarChart>
                </ResponsiveContainer>
              </ChartShell>
            )}
          </CardContent>
        </Card>

        {/* Latency trend */}
        <Card>
          <CardHeader>
            <CardTitle>Latency trend</CardTitle>
            <CardDescription>Average time-to-first-byte and total response time per day.</CardDescription>
          </CardHeader>
          <CardContent>
            {rows.length === 0 ? (
              <EmptyChart loading={query.isLoading} />
            ) : (
              <ChartShell heightClass="h-64">
                <ResponsiveContainer width="100%" height="100%">
                  <ComposedChart data={rows} margin={{ top: 8, right: 8, bottom: 0, left: 0 }}>
                    <CartesianGrid {...chartGrid} vertical={false} />
                    <XAxis dataKey="day" tick={axisTick} tickLine={false} axisLine={false} />
                    <YAxis
                      tick={axisTick}
                      tickLine={false}
                      axisLine={false}
                      tickFormatter={(v) => `${compactAxis(v)}ms`}
                    />
                    <Tooltip
                      cursor={timeCursor}
                      content={tip({ valueFormatter: (v) => `${Math.round(v)} ms` })}
                    />
                    <Legend wrapperStyle={{ fontSize: 12 }} />
                    <Line
                      type="monotone"
                      dataKey="avg_ttft_ms"
                      name="Avg TTFB"
                      stroke={TTFT_COLOR}
                      strokeWidth={2}
                      dot={false}
                    />
                    <Line
                      type="monotone"
                      dataKey="avg_total_ms"
                      name="Avg total"
                      stroke={TOTAL_COLOR}
                      strokeWidth={2}
                      dot={false}
                    />
                  </ComposedChart>
                </ResponsiveContainer>
              </ChartShell>
            )}
          </CardContent>
        </Card>

        {/* Top models by tokens */}
        <Card>
          <CardHeader>
            <CardTitle>Top models</CardTitle>
            <CardDescription>By total tokens over the selected range.</CardDescription>
          </CardHeader>
          <CardContent>
            {topModels.length === 0 ? (
              <EmptyChart loading={modelQuery.isLoading} />
            ) : (
              <ChartShell heightClass="h-64">
                <ResponsiveContainer width="100%" height="100%">
                  <BarChart
                    data={topModels}
                    layout="vertical"
                    margin={{ left: 4, right: 16, top: 4, bottom: 4 }}
                  >
                    <CartesianGrid {...chartGrid} horizontal={false} />
                    <XAxis
                      type="number"
                      tick={axisTick}
                      axisLine={false}
                      tickLine={false}
                      tickFormatter={compactAxis}
                    />
                    <YAxis
                      type="category"
                      dataKey="name"
                      width={120}
                      tick={axisTick}
                      axisLine={false}
                      tickLine={false}
                      interval={0}
                    />
                    <Tooltip
                      cursor={false}
                      content={tip({
                        labelFormatter: (l, p) => {
                          const row = p[0]?.payload as { requests?: number } | undefined;
                          return row ? `${l} \u00b7 ${formatNumber(row.requests ?? 0)} requests` : String(l);
                        },
                      })}
                    />
                    <Bar dataKey="total_tokens" name="Total tokens" radius={[0, 4, 4, 0]} barSize={14}>
                      {topModels.map((d) => (
                        <Cell key={d.name} fill={d.fill} />
                      ))}
                    </Bar>
                  </BarChart>
                </ResponsiveContainer>
              </ChartShell>
            )}
          </CardContent>
        </Card>

        {/* Request outcomes */}
        <Card>
          <CardHeader>
            <CardTitle>Request outcomes</CardTitle>
            <CardDescription>Successful vs. failed requests.</CardDescription>
          </CardHeader>
          <CardContent>
            {totals.requests === 0 ? (
              <EmptyChart loading={query.isLoading} />
            ) : (
              <ChartShell heightClass="h-64">
                <ResponsiveContainer width="100%" height="100%">
                  <PieChart>
                    <Pie
                      data={outcomes}
                      dataKey="value"
                      nameKey="name"
                      innerRadius="58%"
                      outerRadius="80%"
                      paddingAngle={2}
                      strokeWidth={0}
                    >
                      {outcomes.map((o) => (
                        <Cell key={o.name} fill={o.fill} />
                      ))}
                    </Pie>
                    <Tooltip content={tip({ hideLabel: true })} />
                    <Legend wrapperStyle={{ fontSize: 12 }} />
                  </PieChart>
                </ResponsiveContainer>
              </ChartShell>
            )}
          </CardContent>
        </Card>
      </div>


      {/* Breakdown table */}
      <Card>
        <CardHeader className="flex-row items-center justify-between space-y-0">
          <CardTitle>Breakdown</CardTitle>
          <Select
            value={tableGroup}
            onChange={(e) => setTableGroup(e.target.value as BreakdownGroup)}
            aria-label="Group breakdown by"
            className="h-8 w-32 text-xs"
          >
            <option value="day">By day</option>
            <option value="tenant">By team</option>
            <option value="key">By key</option>
            <option value="model">By model</option>
          </Select>
        </CardHeader>
        <CardContent className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-border text-left text-xs text-muted-foreground">
                <th className="py-2 pr-4 font-medium">{GROUP_LABELS[tableGroup]}</th>
                <th className="py-2 pr-4 text-right font-medium">Requests</th>
                <th className="py-2 pr-4 text-right font-medium">Errors</th>
                <th className="py-2 pr-4 text-right font-medium">Input tok</th>
                <th className="py-2 pr-4 text-right font-medium">Output tok</th>
                <th className="py-2 pr-4 text-right font-medium">Total tok</th>
                <th className="py-2 pr-4 text-right font-medium">Spend</th>
                <th className="py-2 pr-4 text-right font-medium">Energy</th>
                <th className="py-2 pr-4 text-right font-medium">CO₂</th>
                <th className="py-2 pr-4 text-right font-medium">Avg TTFB</th>
                <th className="py-2 text-right font-medium">Avg total</th>
              </tr>
            </thead>
            <tbody>
              {breakdownRows.length === 0 ? (
                <tr>
                  <td colSpan={11} className="py-8 text-center text-muted-foreground">
                    No data
                  </td>
                </tr>
              ) : (
                breakdownRows.map((r) => (
                  <tr
                    key={`${r.day}|${r.tenant_id}|${r.key_id}|${r.model}`}
                    className="border-b border-border/50"
                  >
                    <td className="py-2 pr-4">
                      <span className={tableGroup === "day" ? "font-mono text-xs" : ""}>
                        {r.label}
                      </span>
                      {r.sublabel && (
                        <span className="ml-2 font-mono text-xs text-muted-foreground">
                          {r.sublabel}
                        </span>
                      )}
                    </td>
                    <td className="py-2 pr-4 text-right tabular-nums">
                      {formatNumber(r.requests)}
                    </td>
                    <td className="py-2 pr-4 text-right tabular-nums">
                      {formatNumber(r.error_requests)}
                    </td>
                    <td className="py-2 pr-4 text-right tabular-nums">
                      {formatNumber(r.input_tokens)}
                    </td>
                    <td className="py-2 pr-4 text-right tabular-nums">
                      {formatNumber(r.output_tokens)}
                    </td>
                    <td className="py-2 pr-4 text-right tabular-nums">
                      {formatNumber(r.total_tokens)}
                    </td>
                    <td className="py-2 pr-4 text-right tabular-nums">{formatUsd(r.cost_usd)}</td>
                    <td className="py-2 pr-4 text-right tabular-nums">
                      {r.energy_wh > 0 ? formatEnergyKwh(r.energy_wh) : "—"}
                    </td>
                    <td className="py-2 pr-4 text-right tabular-nums">
                      {r.co2_g > 0 ? formatCo2(r.co2_g) : "—"}
                    </td>
                    <td className="py-2 pr-4 text-right tabular-nums">
                      {Math.round(r.avg_ttft_ms)} ms
                    </td>
                    <td className="py-2 text-right tabular-nums">{Math.round(r.avg_total_ms)} ms</td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </CardContent>
      </Card>
    </div>
  );
}

function Kpi({ label, value }: { label: string; value: string }) {
  return (
    <Card>
      <CardContent className="pt-6">
        <p className="text-xs uppercase tracking-wide text-muted-foreground">{label}</p>
        <p className="mt-1 text-2xl font-semibold tabular-nums">{value}</p>
      </CardContent>
    </Card>
  );
}

function EmptyChart({ loading }: { loading: boolean }) {
  return (
    <div className="flex h-64 items-center justify-center text-sm text-muted-foreground">
      {loading ? "Loading\u2026" : "No usage in this range."}
    </div>
  );
}
