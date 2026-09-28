"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { Bookmark, Search, X } from "lucide-react";
import { Facets } from "@/components/logs/facets";
import { RequestPanel } from "@/components/logs/request-panel";
import { Segmented } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Select } from "@/components/ui/select";
import {
  bucketFor,
  bucketLabel,
  cost,
  DEFAULT_LOG_FILTERS,
  duration,
  fillHistogram,
  filtersActive,
  isFailure,
  LOG_WINDOWS,
  loadViews,
  logParams,
  parseSearch,
  REQUEST_TYPES,
  storeViews,
  timeSplit,
  tokens,
  when,
  windowRange,
  type HistogramBucket,
  type KeyOption,
  type LogFilters,
  type LogWindowId,
  type Named,
  type SavedView,
} from "@/lib/logs-model";
import type { UsageLogEntry, UsageLogFacets, UsageLogHistogram } from "@/lib/obleth";
import { cn, getJson } from "@/lib/utils";

const PAGE = 50;
const LIVE_MS = 5_000;

function Chip({ active, children, onClear }: { active: boolean; children: React.ReactNode; onClear?: () => void }) {
  return (
    <span className={cn("inline-flex h-8 items-center gap-1.5 rounded-lg border px-2.5 text-[12.5px]", active ? "border-foreground text-foreground" : "border-border text-secondary-foreground")}>
      {children}
      {active && onClear && <button type="button" onClick={onClear} aria-label="Remove filter" className="text-muted-foreground hover:text-foreground"><X className="h-3 w-3" /></button>}
    </span>
  );
}

function Toggle({ on, onClick, children }: { on: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button type="button" aria-pressed={on} onClick={onClick} className={cn("inline-flex h-8 items-center gap-1.5 rounded-lg border px-2.5 text-[12.5px] transition-colors", on ? "border-foreground text-foreground" : "border-border text-secondary-foreground hover:text-foreground")}>
      <span className={cn("h-1.5 w-1.5 rounded-full border border-current", on && "bg-current")} />
      {children}
    </button>
  );
}

/** Requests per bucket, failures bright. Drag across it to zoom the window to that stretch. */
function Strip({ buckets, bucketMs, loading, onZoom }: { buckets: HistogramBucket[]; bucketMs: number; loading: boolean; onZoom: (since: number, until: number) => void }) {
  const box = useRef<HTMLDivElement>(null);
  const [drag, setDrag] = useState<{ a: number; b: number } | null>(null);
  const max = Math.max(1, ...buckets.map((b) => b.requests));
  const indexAt = (x: number) => {
    const r = box.current!.getBoundingClientRect();
    return Math.min(buckets.length - 1, Math.max(0, Math.floor(((x - r.left) / r.width) * buckets.length)));
  };
  const lo = drag ? Math.min(drag.a, drag.b) : -1;
  const hi = drag ? Math.max(drag.a, drag.b) : -1;
  const label = (t: number) => new Date(t).toLocaleString([], bucketMs >= 86_400_000 ? { month: "short", day: "numeric" } : { hour: "2-digit", minute: "2-digit" });
  const ticks = buckets.length > 4 ? [0, 0.25, 0.5, 0.75].map((f) => buckets[Math.floor(f * (buckets.length - 1))]) : [];
  return (
    <section aria-label="Requests over time" className="rounded-xl border border-border bg-card px-[18px] pb-2.5 pt-3">
      <div className="flex items-center justify-between pb-2 text-[12.5px]">
        <span className="text-secondary-foreground">Requests per {bucketLabel(bucketMs)} · <span className="text-foreground">failed shown bright</span></span>
        <span className="text-[11.5px] text-muted-foreground">Drag across the chart to zoom into a moment</span>
      </div>
      <div
        ref={box}
        role="img"
        aria-label={`${buckets.reduce((n, b) => n + b.requests, 0)} requests, ${buckets.reduce((n, b) => n + b.errors, 0)} failed`}
        className="relative flex h-16 cursor-crosshair select-none items-end gap-px"
        onPointerDown={(e) => { if (buckets.length) { (e.target as HTMLElement).setPointerCapture?.(e.pointerId); const i = indexAt(e.clientX); setDrag({ a: i, b: i }); } }}
        onPointerMove={(e) => { if (drag) setDrag({ ...drag, b: indexAt(e.clientX) }); }}
        onPointerUp={() => {
          if (drag && hi > lo) onZoom(buckets[lo].bucket_ms, buckets[hi].bucket_ms + bucketMs);
          setDrag(null);
        }}
      >
        {loading && buckets.length === 0 && <div className="skeleton absolute inset-0 rounded-md" />}
        {buckets.map((b, i) => (
          <div key={b.bucket_ms} title={`${new Date(b.bucket_ms).toLocaleString()} · ${b.requests} requests${b.errors ? `, ${b.errors} failed` : ""}`} className={cn("flex h-full flex-1 flex-col justify-end", i >= lo && i <= hi && "bg-muted/60")}>
            <span className="block rounded-t-[1px] bg-foreground" style={{ height: `${(b.errors / max) * 100}%` }} />
            <span className="block bg-muted-foreground/60" style={{ height: `${((b.requests - b.errors) / max) * 100}%` }} />
          </div>
        ))}
      </div>
      <div className="flex justify-between pt-1.5 font-mono text-[10.5px] text-muted-foreground">
        {ticks.map((b) => <span key={b.bucket_ms}>{label(b.bucket_ms)}</span>)}
        {ticks.length > 0 && <span>{buckets.length && Date.now() - buckets[buckets.length - 1].bucket_ms < bucketMs * 2 ? "now" : label(buckets[buckets.length - 1].bucket_ms)}</span>}
      </div>
    </section>
  );
}

function Row({ row, selected, onOpen, now }: { row: UsageLogEntry; selected: boolean; onOpen: () => void; now: number }) {
  const failed = isFailure(row);
  const t = timeSplit(row);
  const pct = (ms: number) => `${t.total ? (ms / t.total) * 100 : 0}%`;
  return (
    <tr onClick={onOpen} className={cn("cursor-pointer border-t border-border/70 transition-colors hover:bg-muted/30", selected && "bg-secondary")}>
      <td className="whitespace-nowrap py-2 pl-[18px] pr-3 font-mono text-[12px] text-muted-foreground" title={new Date(row.ts_ms).toLocaleString()}>{when(row.ts_ms, now)}</td>
      <td className="py-2 pr-3">
        <span className={cn("inline-flex h-5 items-center rounded-full border px-1.5 font-mono text-[11px]", failed ? "border-foreground bg-foreground font-semibold text-background" : "border-border text-secondary-foreground")}>{row.status_code}</span>
      </td>
      <td className="max-w-[16rem] py-2 pr-3">
        <span className="block truncate font-mono text-[12.5px]">{row.model}</span>
        <span className="block truncate text-[11.5px] text-muted-foreground">{row.request_type || "other"}{row.cache_status === "hit" ? " · cached" : ""}</span>
      </td>
      <td className="max-w-[16rem] py-2 pr-3">
        <span className="block truncate">{row.tenant_name || row.tenant_id.slice(0, 8)}</span>
        <span className="block truncate font-mono text-[11.5px] text-muted-foreground">{row.key_name || "—"}{row.key_prefix ? ` · ${row.key_prefix}` : ""}</span>
      </td>
      <td className="whitespace-nowrap py-2 pr-3 text-right font-mono text-[12px]">{tokens(row.input_tokens)} → {tokens(row.output_tokens)}</td>
      <td className="py-2 pr-3">
        <div className="flex items-center gap-2.5" title={`waited ${duration(t.wait)} · first token after ${duration(t.wait + t.first)} · ${duration(t.total)} in all`}>
          <span className="flex h-1.5 min-w-[6rem] flex-1 overflow-hidden rounded-full bg-muted/50">
            <span className="bg-muted-foreground/40" style={{ width: pct(t.wait) }} />
            <span className="bg-muted-foreground" style={{ width: pct(t.first) }} />
            <span className={failed ? "bg-foreground" : "bg-secondary-foreground"} style={{ width: pct(t.rest) }} />
          </span>
          <span className="w-14 text-right font-mono text-[12px]">{duration(row.total_ms)}</span>
        </div>
      </td>
      <td className="whitespace-nowrap py-2 pr-3 text-right font-mono text-[12px] text-secondary-foreground">{cost(row.cost_usd)}</td>
      <td className="w-8 py-2 pr-[18px] text-center text-muted-foreground" title={row.has_trace ? "Traced" : undefined}>{row.has_trace ? "◇" : ""}</td>
    </tr>
  );
}

export function RequestLogs({
  tenants,
  models,
  keys,
  initial,
  initialRequestId,
}: {
  tenants: Named[];
  models: string[];
  keys: KeyOption[];
  initial?: Partial<LogFilters>;
  initialRequestId?: string;
}) {
  const [filters, setFilters] = useState<LogFilters>({ ...DEFAULT_LOG_FILTERS, ...initial, ...(initialRequestId ? { requestId: initialRequestId, includeInternal: true, window: "30d" as const } : {}) });
  const [live, setLive] = useState(!initialRequestId && initial?.window !== "custom");
  const [pinnedAt, setPinnedAt] = useState(() => Date.now());
  const [older, setOlder] = useState<UsageLogEntry[]>([]);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [openId, setOpenId] = useState<string | null>(initialRequestId ?? null);
  const [draft, setDraft] = useState("");
  const [searchNote, setSearchNote] = useState<string | null>(null);
  const [views, setViews] = useState<SavedView[]>([]);
  const [naming, setNaming] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const search = useRef<HTMLInputElement>(null);

  useEffect(() => setViews(loadViews()), []);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 5_000);
    return () => clearInterval(t);
  }, []);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "/" || (e.target instanceof Element && e.target.closest("input, textarea, select, [contenteditable=true]"))) return;
      e.preventDefault();
      search.current?.focus();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const patch = useCallback((p: Partial<LogFilters>) => {
    setFilters((f) => ({ ...f, ...p }));
    setOlder([]);
    setPinnedAt(Date.now());
  }, []);

  // Live, the window slides with every poll; paused, it stays where it was
  // when paused, so paging back through history stays put.
  const rangeAt = live ? undefined : pinnedAt;
  const head = useQuery({
    queryKey: ["request-logs", filters, rangeAt],
    queryFn: () => getJson<UsageLogEntry[]>(`/api/live/usage/logs?${logParams(filters, windowRange(filters, rangeAt ?? Date.now()), { limit: PAGE })}`),
    refetchInterval: live ? LIVE_MS : false,
    placeholderData: keepPreviousData,
  });
  const bucketMs = bucketFor(filters, rangeAt ?? now);
  const histogram = useQuery({
    queryKey: ["request-logs-histogram", filters, rangeAt, bucketMs],
    queryFn: () => getJson<UsageLogHistogram>(`/api/live/usage/logs/histogram?${logParams(filters, windowRange(filters, rangeAt ?? Date.now()), { bucket_ms: bucketMs })}`),
    refetchInterval: live ? 15_000 : false,
    placeholderData: keepPreviousData,
  });
  const facets = useQuery({
    queryKey: ["request-logs-facets", filters, rangeAt],
    queryFn: () => getJson<UsageLogFacets>(`/api/live/usage/logs/facets?${logParams(filters, windowRange(filters, rangeAt ?? Date.now()))}`),
    refetchInterval: live ? 30_000 : false,
    placeholderData: keepPreviousData,
  });
  const top = head.data?.[0];
  // Paused: count what has arrived since, without moving the list.
  const newer = useQuery({
    queryKey: ["request-logs-newer", filters, top?.ts_ms],
    enabled: !live && !!top && filters.window !== "custom",
    queryFn: () => getJson<UsageLogEntry[]>(`/api/live/usage/logs?${logParams(filters, { since: top!.ts_ms + 1, until: Date.now() }, { limit: 100 })}`),
    refetchInterval: 15_000,
  });

  const rows = useMemo(() => {
    const seen = new Set<string>();
    return [...(head.data ?? []), ...older].filter((r) => (seen.has(r.request_id) ? false : (seen.add(r.request_id), true)));
  }, [head.data, older]);
  const range = windowRange(filters, rangeAt ?? now);
  const buckets = fillHistogram(histogram.data?.buckets ?? [], range, histogram.data?.bucket_ms ?? bucketMs);
  const total = buckets.reduce((n, b) => n + b.requests, 0);
  const failedCount = buckets.reduce((n, b) => n + b.errors, 0);
  const lastPage = older.length ? older.length % PAGE !== 0 : (head.data?.length ?? 0) < PAGE;

  async function loadOlder() {
    const last = rows[rows.length - 1];
    if (!last) return;
    // Paging back pauses the list and pins the window where it is now.
    const at = live ? Date.now() : pinnedAt;
    if (live) {
      setLive(false);
      setPinnedAt(at);
    }
    setLoadingOlder(true);
    try {
      const page = await getJson<UsageLogEntry[]>(`/api/live/usage/logs?${logParams(filters, windowRange(filters, at), { limit: PAGE, before_ms: last.ts_ms, before_request_id: last.request_id })}`);
      setOlder((o) => [...o, ...page]);
    } finally {
      setLoadingOlder(false);
    }
  }

  function runSearch() {
    const { patch: p, unknown } = parseSearch(draft, { tenants, keys, models });
    if (Object.keys(p).length) patch(p);
    setSearchNote(unknown.length ? `Didn't recognise ${unknown.join(", ")}. Try model:, team:, key:, status:, type: or session:.` : null);
    if (!unknown.length) setDraft("");
  }

  const openIndex = rows.findIndex((r) => r.request_id === openId);
  const open = openIndex >= 0 ? rows[openIndex] : null;
  const step = useCallback((dir: -1 | 1) => {
    setOpenId((id) => {
      const i = rows.findIndex((r) => r.request_id === id);
      const next = rows[i + dir];
      return next ? next.request_id : id;
    });
  }, [rows]);

  const tenantName = new Map(tenants.map((t) => [t.id, t.name]));
  const teamKeys = filters.tenantId ? keys.filter((k) => k.tenantId === filters.tenantId) : keys;
  const at = (t: number) => new Date(t).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
  const sameDay = new Date(range.since).toDateString() === new Date(range.until).toDateString();
  const winLabel = filters.window === "custom" ? `between ${at(range.since)} and ${sameDay ? new Date(range.until).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : at(range.until)}` : `in the last ${{ "15m": "15 minutes", "1h": "hour", "24h": "24 hours", "7d": "7 days", "30d": "30 days" }[filters.window]}`;
  const saveView = () => {
    const name = naming?.trim();
    if (!name) return;
    const next = [...views.filter((v) => v.name !== name), { name, filters }];
    setViews(next);
    storeViews(next);
    setNaming(null);
  };

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-4">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-[26px] font-semibold tracking-tight">Request logs</h1>
          <p className="mt-1 text-[13px] text-muted-foreground">
            {histogram.isLoading ? "Counting…" : `${total.toLocaleString()} requests ${winLabel}${total ? ` · ${((failedCount / total) * 100).toFixed(1)}% failed` : ""}`}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Button
            type="button"
            variant="outline"
            size="sm"
            aria-pressed={live}
            onClick={() => { setLive((l) => !l); setPinnedAt(Date.now()); if (!live) setOlder([]); }}
            disabled={filters.window === "custom"}
            className={cn("h-9 gap-2", live && "border-foreground")}
          >
            <span className={cn("h-2 w-2 rounded-full", live ? "bg-foreground" : "border-[1.5px] border-muted-foreground")} />
            {live ? "Live" : "Paused"}
          </Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button type="button" variant="outline" size="sm" className="h-9"><Bookmark className="h-3.5 w-3.5" />Views</Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-64">
              <DropdownMenuLabel className="text-[11px] uppercase tracking-[0.07em] text-muted-foreground">Saved in this browser</DropdownMenuLabel>
              {views.length === 0 && <p className="px-2 py-1.5 text-xs text-muted-foreground">None yet.</p>}
              {views.map((v) => (
                <DropdownMenuItem key={v.name} onSelect={() => { patch(v.filters); setLive(v.filters.window !== "custom"); }} className="justify-between">
                  <span className="truncate">{v.name}</span>
                  <button type="button" aria-label={`Delete ${v.name}`} onClick={(e) => { e.stopPropagation(); const next = views.filter((x) => x.name !== v.name); setViews(next); storeViews(next); }} className="text-muted-foreground hover:text-foreground"><X className="h-3 w-3" /></button>
                </DropdownMenuItem>
              ))}
              <DropdownMenuSeparator />
              <DropdownMenuItem disabled={!filtersActive(filters)} onSelect={() => setNaming("")}>Save these filters…</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      {naming !== null && (
        <form onSubmit={(e) => { e.preventDefault(); saveView(); }} className="flex items-center gap-2 rounded-lg border border-border px-3 py-2 text-[13px]">
          <span className="text-muted-foreground">Name this view</span>
          <input autoFocus value={naming} onChange={(e) => setNaming(e.target.value)} placeholder="Failed chat on glm-5-3" aria-label="View name" className="min-w-0 flex-1 bg-transparent outline-none" />
          <Button type="submit" size="sm" disabled={!naming.trim()}>Save</Button>
          <Button type="button" size="sm" variant="ghost" onClick={() => setNaming(null)}>Cancel</Button>
        </form>
      )}

      <div className="flex flex-col gap-2.5">
        <div className="flex flex-wrap items-center gap-2">
          <label className="flex h-9 min-w-[18rem] flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground">
            <Search className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden />
            <input
              ref={search}
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") runSearch(); if (e.key === "Escape") setDraft(""); }}
              placeholder="Request ID, or model:, team:, key:, status: (error, ok or a code), type:, session:"
              aria-label="Search requests"
              className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground"
            />
            <kbd className="rounded border border-border px-1.5 font-mono text-[11px] text-muted-foreground">/</kbd>
          </label>
          <Segmented<LogWindowId | "custom">
            label="Time window"
            value={filters.window}
            onChange={(w) => { if (w !== "custom") { patch({ window: w, sinceMs: undefined, untilMs: undefined }); } }}
            options={[...LOG_WINDOWS.map((w) => ({ value: w.id, label: w.label })), ...(filters.window === "custom" ? [{ value: "custom" as const, label: "Custom" }] : [])]}
          />
        </div>
        {searchNote && <p className="text-xs text-foreground">{searchNote}</p>}
        <div className="flex flex-wrap items-center gap-2">
          <Select aria-label="Status" value={filters.status} onValueChange={(v) => patch({ status: v as LogFilters["status"] })} className={cn("h-8 w-auto min-w-[8.5rem] text-[12.5px]", filters.status && "border-foreground")} options={[{ value: "", label: "Any status" }, { value: "success", label: "Succeeded" }, { value: "error", label: "Failed" }]} />
          <Select aria-label="Model" value={filters.model} onValueChange={(v) => patch({ model: v })} searchPlaceholder="Find a model" className={cn("h-8 w-auto min-w-[9rem] text-[12.5px]", filters.model && "border-foreground")} options={[{ value: "", label: "Any model" }, ...models.map((m) => ({ value: m, label: m }))]} />
          <Select aria-label="Team" value={filters.tenantId} onValueChange={(v) => patch({ tenantId: v, keyId: "" })} searchPlaceholder="Find a team" className={cn("h-8 w-auto min-w-[9rem] text-[12.5px]", filters.tenantId && "border-foreground")} options={[{ value: "", label: "Any team" }, ...tenants.map((t) => ({ value: t.id, label: t.name }))]} />
          <Select aria-label="Key" value={filters.keyId} onValueChange={(v) => { const k = keys.find((x) => x.id === v); patch({ keyId: v, ...(k ? { tenantId: k.tenantId } : {}) }); }} searchPlaceholder="Find a key" className={cn("h-8 w-auto min-w-[8rem] text-[12.5px]", filters.keyId && "border-foreground")} options={[{ value: "", label: "Any key" }, ...teamKeys.slice(0, 500).map((k) => ({ value: k.id, label: `${k.name || k.prefix}`, hint: filters.tenantId ? k.prefix : `${k.prefix} · ${tenantName.get(k.tenantId) ?? ""}` }))]} />
          <Select aria-label="Type" value={filters.requestType} onValueChange={(v) => patch({ requestType: v })} className={cn("h-8 w-auto min-w-[7.5rem] text-[12.5px]", filters.requestType && "border-foreground")} options={[{ value: "", label: "Any type" }, ...REQUEST_TYPES.map((t) => ({ value: t, label: t }))]} />
          <Toggle on={filters.tracedOnly} onClick={() => patch({ tracedOnly: !filters.tracedOnly })}>Traced only</Toggle>
          <Toggle on={filters.includeInternal} onClick={() => patch({ includeInternal: !filters.includeInternal })}>Include health checks</Toggle>
          {filters.statusCode && <Chip active onClear={() => patch({ statusCode: "" })}>HTTP <span className="font-mono">{filters.statusCode}</span></Chip>}
          {filters.sessionId && <Chip active onClear={() => patch({ sessionId: "" })}>Session <span className="font-mono">{filters.sessionId.slice(0, 12)}{filters.sessionId.length > 12 ? "…" : ""}</span></Chip>}
          {filters.requestId && <Chip active onClear={() => patch({ requestId: "" })}>ID starts <span className="font-mono">{filters.requestId}</span></Chip>}
          {filtersActive(filters) && (
            <button type="button" onClick={() => { patch({ ...DEFAULT_LOG_FILTERS }); setLive(true); }} className="ml-auto text-[12.5px] text-muted-foreground hover:text-foreground">Clear filters</button>
          )}
        </div>
      </div>

      <Strip
        buckets={buckets}
        bucketMs={histogram.data?.bucket_ms ?? bucketMs}
        loading={histogram.isLoading}
        onZoom={(since, until) => { setLive(false); patch({ window: "custom", sinceMs: since, untilMs: Math.min(until, Date.now()) }); }}
      />

      <Facets facets={facets.data} filters={filters} onPatch={patch} loading={facets.isLoading} />

      <section aria-label="Requests" className="overflow-hidden rounded-xl border border-border bg-card">
        {!live && (newer.data?.length ?? 0) > 0 && (
          <div className="flex justify-center border-b border-border py-2">
            <button type="button" onClick={() => { setLive(true); setOlder([]); }} className="inline-flex h-7 items-center rounded-full border border-border px-3 text-xs hover:border-muted-foreground">
              ↑ {newer.data!.length >= 100 ? "100+" : newer.data!.length} newer · show
            </button>
          </div>
        )}
        <div className="overflow-x-auto">
          <table className="w-full min-w-[960px] text-[13px]">
            <thead>
              <tr className="text-left text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">
                <th className="py-2.5 pl-[18px] pr-3 font-semibold">When</th>
                <th className="py-2.5 pr-3 font-semibold">Status</th>
                <th className="py-2.5 pr-3 font-semibold">Model</th>
                <th className="py-2.5 pr-3 font-semibold">Team · key</th>
                <th className="py-2.5 pr-3 text-right font-semibold">Tokens in → out</th>
                <th className="py-2.5 pr-3 font-semibold">Wait · first token · rest</th>
                <th className="py-2.5 pr-3 text-right font-semibold">Cost</th>
                <th className="py-2.5 pr-[18px]"><span className="sr-only">Traced</span></th>
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => <Row key={r.request_id} row={r} selected={r.request_id === openId} onOpen={() => setOpenId(r.request_id)} now={now} />)}
            </tbody>
          </table>
        </div>
        {rows.length === 0 && (
          <div className="flex flex-col items-center gap-3 px-6 py-14 text-center text-[13px] text-muted-foreground">
            {head.isLoading ? <div className="skeleton h-4 w-48 rounded" /> : head.isError ? "The request log could not be read." : <>No requests match{filtersActive(filters) ? " these filters" : ""} {winLabel}.{filtersActive(filters) && <Button type="button" size="sm" variant="outline" onClick={() => patch({ ...DEFAULT_LOG_FILTERS })}>Clear filters</Button>}</>}
          </div>
        )}
        {rows.length > 0 && (
          <div className="flex items-center justify-between border-t border-border px-[18px] py-3 text-[12.5px] text-muted-foreground">
            <span>{rows.length.toLocaleString()} shown of {total.toLocaleString()} {winLabel} · ◇ traced</span>
            <Button type="button" variant="outline" size="sm" disabled={lastPage || loadingOlder} onClick={loadOlder}>{loadingOlder ? "Loading…" : lastPage ? "That's all" : "Load older"}</Button>
          </div>
        )}
      </section>

      {open && (
        <RequestPanel
          row={open}
          onClose={() => setOpenId(null)}
          onStep={step}
          canStep={{ newer: openIndex > 0, older: openIndex < rows.length - 1 }}
          onFilter={(p) => { setOpenId(null); patch(p); }}
        />
      )}
    </div>
  );
}
