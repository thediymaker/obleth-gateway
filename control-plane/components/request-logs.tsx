"use client";

import { Fragment, useRef, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { ChevronLeft, ChevronRight, Hexagon, Pause, Play, RefreshCw, Search } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import { RequestDetail } from "@/components/request-detail";
import type { UsageLogEntry } from "@/lib/obleth";
import { formatDurationMs, truncateId } from "@/lib/format";
import { cn, formatCurrency, formatNumber } from "@/lib/utils";

const LIVE_POLL_MS = 15_000;
const PAGE_SIZES = [50, 100, 200] as const;
const DEFAULT_PAGE_SIZE = 50;

interface TenantOption {
  id: string;
  name: string;
}

interface TimeWindow {
  label: string;
  ms: number;
}

const DAY_MS = 24 * 60 * 60_000;

const WINDOWS: TimeWindow[] = [
  { label: "Last 15 min", ms: 15 * 60_000 },
  { label: "Last hour", ms: 60 * 60_000 },
  { label: "Last 24 hours", ms: DAY_MS },
  { label: "Last 7 days", ms: 7 * DAY_MS },
  { label: "Last 30 days", ms: 30 * DAY_MS },
  { label: "Last 90 days", ms: 90 * DAY_MS },
];

interface Filters {
  windowMs: number;
  tenantId: string;
  model: string;
  requestType: string;
  status: string;
  requestId: string;
  tracedOnly: boolean;
  includeInternal: boolean;
}

const DEFAULT_FILTERS: Filters = {
  windowMs: WINDOWS[2].ms,
  tenantId: "",
  model: "",
  requestType: "",
  status: "",
  requestId: "",
  tracedOnly: false,
  includeInternal: false,
};

const REQUEST_TYPES = [
  "chat",
  "completion",
  "responses",
  "embedding",
  "audio",
  "image",
  "rerank",
  "moderation",
  "other",
];

interface Cursor {
  beforeMs: number;
  beforeRequestId: string;
}

export function RequestLogs({ tenants, models }: { tenants: TenantOption[]; models: string[] }) {
  const [filters, setFilters] = useState<Filters>(DEFAULT_FILTERS);
  const [requestIdDraft, setRequestIdDraft] = useState("");
  const [liveTail, setLiveTail] = useState(true);
  const [pageSize, setPageSize] = useState<number>(DEFAULT_PAGE_SIZE);
  const [cursor, setCursor] = useState<Cursor | undefined>(undefined);
  const [cursorStack, setCursorStack] = useState<Cursor[]>([]);
  const [expandedRequestId, setExpandedRequestId] = useState<string | null>(null);

  const toggleExpand = (id: string) =>
    setExpandedRequestId((prev) => (prev === id ? null : id));

  // Pin the lower time bound for the lifetime of a filter set so paging back
  // through history doesn't keep sliding the window forward under us. Live tail
  // recomputes it on every poll instead, to keep surfacing fresh rows.
  const pinnedSinceRef = useRef<number>(Date.now() - filters.windowMs);

  const resetPaging = () => {
    setCursor(undefined);
    setCursorStack([]);
  };

  // Any filter change invalidates the current cursor stack and re-pins the
  // window, so paging always starts from a coherent newest-first page.
  const patchFilters = (patch: Partial<Filters>) => {
    setFilters((prev) => ({ ...prev, ...patch }));
    pinnedSinceRef.current = Date.now() - (patch.windowMs ?? filters.windowMs);
    resetPaging();
  };

  const applyRequestId = () => patchFilters({ requestId: requestIdDraft.trim() });

  const query = useQuery({
    queryKey: ["request-logs", filters, cursor, liveTail, pageSize],
    queryFn: async () => {
      // Live tail advances the lower bound on every poll; paused mode pins it so
      // paging back through history stays anchored.
      const since = liveTail ? Date.now() - filters.windowMs : pinnedSinceRef.current;
      const params = new URLSearchParams();
      params.set("limit", String(pageSize));
      params.set("since_ms", String(since));
      if (filters.tenantId) params.set("tenant_id", filters.tenantId);
      if (filters.model) params.set("model", filters.model);
      if (filters.requestType) params.set("request_type", filters.requestType);
      if (filters.status) params.set("status", filters.status);
      if (filters.requestId) params.set("request_id", filters.requestId);
      if (filters.tracedOnly) params.set("traced_only", "true");
      if (filters.includeInternal) params.set("include_internal", "true");
      if (!liveTail && cursor) {
        params.set("before_ms", String(cursor.beforeMs));
        params.set("before_request_id", cursor.beforeRequestId);
      }
      const res = await fetch(`/api/live/usage/logs?${params.toString()}`);
      if (!res.ok) throw new Error("request logs unavailable");
      return (await res.json()) as UsageLogEntry[];
    },
    refetchInterval: liveTail ? LIVE_POLL_MS : false,
    placeholderData: keepPreviousData,
  });

  const rows = query.data ?? [];
  const atFirstPage = cursorStack.length === 0;
  const hasMore = rows.length === pageSize;

  // Paging into history requires a pinned lower bound, so the first page step
  // automatically pauses live tail and anchors the window.
  const ensurePaused = () => {
    if (liveTail) {
      setLiveTail(false);
      pinnedSinceRef.current = Date.now() - filters.windowMs;
    }
  };

  const goNext = () => {
    const last = rows[rows.length - 1];
    if (!last) return;
    ensurePaused();
    setCursorStack((prev) => [...prev, cursor ?? { beforeMs: 0, beforeRequestId: "" }]);
    setCursor({ beforeMs: last.ts_ms, beforeRequestId: last.request_id });
  };

  const goPrev = () => {
    setCursorStack((prev) => {
      if (prev.length === 0) return prev;
      const next = [...prev];
      const restored = next.pop()!;
      setCursor(restored.beforeRequestId === "" ? undefined : restored);
      return next;
    });
  };

  const toggleLiveTail = () => {
    setLiveTail((on) => {
      const next = !on;
      if (next) {
        pinnedSinceRef.current = Date.now() - filters.windowMs;
        resetPaging();
      }
      return next;
    });
  };

  const resetFilters = () => {
    setRequestIdDraft("");
    patchFilters(DEFAULT_FILTERS);
  };

  const filtersActive = Boolean(
    filters.tenantId ||
      filters.model ||
      filters.requestType ||
      filters.status ||
      filters.requestId ||
      filters.windowMs !== DEFAULT_FILTERS.windowMs ||
      filters.tracedOnly ||
      filters.includeInternal,
  );

  return (
    <Card className="overflow-hidden rounded-md border-border/80">
      {/* Toolbar */}
      <div className="grid gap-2.5 border-b border-border bg-card/80 px-3 py-3">
        <div className="grid gap-2 md:grid-cols-[minmax(14rem,1fr)_9rem_7rem_auto_auto]">
          <div className="relative min-w-0">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              type="search"
              value={requestIdDraft}
              onChange={(e) => setRequestIdDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") applyRequestId();
              }}
              placeholder="Search by Request ID"
              aria-label="Search by request id"
              className="h-8 w-full pl-8 text-xs"
            />
          </div>
          <Select
            value={String(filters.windowMs)}
            onChange={(e) => patchFilters({ windowMs: Number(e.target.value) })}
            aria-label="Time window"
            className="h-8 w-full text-xs"
          >
            {WINDOWS.map((w) => (
              <option key={w.ms} value={w.ms}>
                {w.label}
              </option>
            ))}
          </Select>
          <Select
            value={String(pageSize)}
            onChange={(e) => {
              setPageSize(Number(e.target.value));
              resetPaging();
            }}
            aria-label="Rows per page"
            className="h-8 w-full text-xs"
          >
            {PAGE_SIZES.map((n) => (
              <option key={n} value={n}>
                {n} / page
              </option>
            ))}
          </Select>
          <Button
            type="button"
            variant={liveTail ? "default" : "secondary"}
            size="sm"
            onClick={toggleLiveTail}
            aria-pressed={liveTail}
            className="h-8"
          >
            {liveTail ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
            {liveTail ? "Live" : "Paused"}
          </Button>
          <Button
            type="button"
            variant="secondary"
            size="sm"
            onClick={() => query.refetch()}
            className="h-8"
          >
            <RefreshCw className={cn("h-3.5 w-3.5", query.isFetching && "animate-spin")} />
            Fetch
          </Button>
        </div>

        <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-[minmax(0,11rem)_minmax(0,11rem)_minmax(0,9rem)_minmax(0,9rem)_auto_1fr]">
          <Select
            value={filters.tenantId}
            onChange={(e) => patchFilters({ tenantId: e.target.value })}
            aria-label="Filter by team"
            className="h-8 w-full text-xs"
          >
            <option value="">All teams</option>
            {tenants.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}
              </option>
            ))}
          </Select>
          <Select
            value={filters.model}
            onChange={(e) => patchFilters({ model: e.target.value })}
            aria-label="Filter by model"
            className="h-8 w-full text-xs"
          >
            <option value="">All models</option>
            {models.map((m) => (
              <option key={m} value={m}>
                {m}
              </option>
            ))}
          </Select>
          <Select
            value={filters.requestType}
            onChange={(e) => patchFilters({ requestType: e.target.value })}
            aria-label="Filter by type"
            className="h-8 w-full text-xs"
          >
            <option value="">All types</option>
            {REQUEST_TYPES.map((t) => (
              <option key={t} value={t}>
                {t}
              </option>
            ))}
          </Select>
          <Select
            value={filters.status}
            onChange={(e) => patchFilters({ status: e.target.value })}
            aria-label="Filter by status"
            className="h-8 w-full text-xs"
          >
            <option value="">All status</option>
            <option value="success">Success</option>
            <option value="error">Error</option>
          </Select>
          <button
            type="button"
            onClick={() => patchFilters({ tracedOnly: !filters.tracedOnly })}
            className={cn(
              "inline-flex h-8 items-center justify-center gap-1.5 rounded-md border px-2.5 text-[11px] transition-colors",
              filters.tracedOnly
                ? "border-emerald-700/60 bg-emerald-950/40 text-emerald-400"
                : "border-border bg-muted/30 text-muted-foreground hover:text-foreground",
            )}
          >
            <span className="h-1.5 w-1.5 rounded-full border border-current" />
            Traced only
          </button>
          <button
            type="button"
            onClick={() => patchFilters({ includeInternal: !filters.includeInternal })}
            className={cn(
              "inline-flex h-8 items-center justify-center gap-1.5 rounded-md border px-2.5 text-[11px] transition-colors",
              filters.includeInternal
                ? "border-amber-700/60 bg-amber-950/40 text-amber-400"
                : "border-border bg-muted/30 text-muted-foreground hover:text-foreground",
            )}
          >
            <span className="h-1.5 w-1.5 rounded-full border border-current" />
            Show health/internal traffic
          </button>
          {filtersActive && (
            <Button
              type="button"
              variant="ghost"
              size="sm"
              onClick={resetFilters}
              className="h-8 justify-self-start lg:justify-self-end"
            >
              Reset
            </Button>
          )}
        </div>

        <div className="flex flex-col gap-2 border-t border-border/60 pt-2.5 sm:flex-row sm:items-center sm:justify-between">
          <span className="text-xs tabular-nums text-muted-foreground">
            {formatNumber(rows.length)} shown
          </span>
          <div className="flex items-center gap-1.5">
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={goPrev}
              disabled={atFirstPage}
            >
              <ChevronLeft className="h-3.5 w-3.5" />
              Newer
            </Button>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={goNext}
              disabled={!hasMore}
            >
              Older
              <ChevronRight className="h-3.5 w-3.5" />
            </Button>
          </div>
        </div>
      </div>

      {/* Table */}
      <div className="hidden overflow-x-auto md:block">
        <table className="w-full min-w-[680px] text-sm lg:min-w-[760px] xl:min-w-[920px] 2xl:min-w-[1120px]">
          <thead>
            <tr className="border-b border-border text-left text-xs text-muted-foreground">
              <th className="w-32 px-3 py-2 font-medium">Time</th>
              <th className="hidden px-3 py-2 font-medium lg:table-cell">Type</th>
              <th className="px-3 py-2 font-medium">Status</th>
              <th className="px-3 py-2 font-medium">Model</th>
              <th className="hidden px-3 py-2 font-medium 2xl:table-cell">Session</th>
              <th className="px-3 py-2 font-medium">Request ID</th>
              <th className="hidden px-3 py-2 text-right font-medium xl:table-cell">Cost</th>
              <th className="hidden px-3 py-2 text-right font-medium xl:table-cell">Energy</th>
              <th className="px-3 py-2 text-right font-medium">Tokens</th>
              <th className="hidden px-3 py-2 text-right font-medium xl:table-cell">TTFB</th>
              <th className="px-3 py-2 text-right font-medium">Duration</th>
              <th className="hidden px-3 py-2 font-medium 2xl:table-cell">Team</th>
              <th className="hidden px-4 py-2 font-medium 2xl:table-cell">Key</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <Fragment key={`${row.request_id}-${row.ts_ms}`}>
                <LogRow
                  row={row}
                  isExpanded={expandedRequestId === row.request_id}
                  onExpand={() => toggleExpand(row.request_id)}
                />
                {expandedRequestId === row.request_id && (
                  <tr>
                    <td colSpan={13} className="max-w-0 border-b border-emerald-900/40 p-0">
                      <div className="min-w-0 w-full max-w-full overflow-hidden">
                        <RequestDetail row={row} />
                      </div>
                    </td>
                  </tr>
                )}
              </Fragment>
            ))}
            {rows.length === 0 && (
              <tr>
                <td colSpan={13} className="px-4 py-16 text-center text-muted-foreground">
                  {query.isLoading
                    ? "Loading requests..."
                    : "No requests match the current filters and window."}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      <div className="divide-y divide-border/60 md:hidden">
        {rows.map((row) => (
          <Fragment key={`${row.request_id}-${row.ts_ms}`}>
            <LogCard
              row={row}
              isExpanded={expandedRequestId === row.request_id}
              onExpand={() => toggleExpand(row.request_id)}
            />
            {expandedRequestId === row.request_id && <RequestDetail row={row} />}
          </Fragment>
        ))}
        {rows.length === 0 && (
          <div className="px-4 py-14 text-center text-sm text-muted-foreground">
            {query.isLoading
              ? "Loading requests..."
              : "No requests match the current filters and window."}
          </div>
        )}
      </div>
    </Card>
  );
}

function LogRow({
  row,
  isExpanded,
  onExpand,
}: {
  row: UsageLogEntry;
  isExpanded: boolean;
  onExpand: () => void;
}) {
  const ok = row.status_code >= 200 && row.status_code < 400;

  return (
    <tr
      onClick={onExpand}
      className={cn(
        "cursor-pointer select-none border-b border-border/60 transition-colors hover:bg-muted/20",
        row.has_trace && "bg-emerald-950/30",
        isExpanded && "border-b-0",
      )}
    >
      <td className="px-3 py-2 tabular-nums text-muted-foreground">
        <TimeStamp ms={row.ts_ms} />
      </td>
      <td className="hidden px-3 py-1.5 lg:table-cell">
        <Badge className="capitalize">{row.request_type || "other"}</Badge>
      </td>
      <td className="px-3 py-1.5">
        <StatusBadge ok={ok} statusCode={row.status_code} />
      </td>
      <td className="px-3 py-1.5">
        <span className="block max-w-[13rem] truncate font-mono text-xs">{row.model}</span>
      </td>
      <td className="hidden px-3 py-1.5 2xl:table-cell">
        <SessionCell entry={row} />
      </td>
      <td className="px-3 py-1.5 font-mono text-xs text-muted-foreground" title={row.request_id}>
        <span className="inline-flex items-center gap-1.5">
          {row.has_trace && <TraceMarker />}
          <span>{row.request_id.slice(0, 8)}</span>
        </span>
      </td>
      <td className="hidden px-3 py-1.5 text-right tabular-nums xl:table-cell">
        {formatCurrency(row.cost_usd)}
      </td>
      <td className="hidden px-3 py-1.5 text-right tabular-nums xl:table-cell">
        {formatWh(row.energy_wh)}
      </td>
      <td className="px-3 py-1.5 text-right tabular-nums text-muted-foreground">
        {formatNumber(row.total_tokens)}
      </td>
      <td className="hidden px-3 py-1.5 text-right tabular-nums text-muted-foreground xl:table-cell">
        {formatDurationMs(row.ttft_ms)}
      </td>
      <td className="px-3 py-1.5 text-right tabular-nums text-muted-foreground">
        {formatDurationMs(row.total_ms)}
      </td>
      <td className="hidden px-3 py-1.5 2xl:table-cell">
        <span className="block max-w-[10rem] truncate">
          {row.tenant_name || row.tenant_id.slice(0, 8)}
        </span>
      </td>
      <td className="hidden px-4 py-1.5 2xl:table-cell">
        <span className="flex min-w-0 items-baseline gap-1.5">
          <span className="truncate text-xs">{row.key_name || "--"}</span>
          {row.key_prefix && (
            <span className="shrink-0 font-mono text-[10px] text-muted-foreground">
              {row.key_prefix}
            </span>
          )}
        </span>
      </td>
    </tr>
  );
}

function LogCard({
  row,
  isExpanded,
  onExpand,
}: {
  row: UsageLogEntry;
  isExpanded: boolean;
  onExpand: () => void;
}) {
  const ok = row.status_code >= 200 && row.status_code < 400;

  return (
    <div
      className={cn(
        "bg-card/40 transition-colors",
        row.has_trace && "bg-emerald-950/20",
        isExpanded && "bg-muted/20",
      )}
    >
      <button type="button" onClick={onExpand} className="block w-full px-3 py-3 text-left">
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <div className="flex min-w-0 items-center gap-2">
              <StatusBadge ok={ok} statusCode={row.status_code} />
              <Badge className="capitalize">{row.request_type || "other"}</Badge>
            </div>
            <div className="mt-2 flex min-w-0 items-center gap-1.5 font-mono text-xs text-muted-foreground">
              {row.has_trace && <TraceMarker />}
              <span className="truncate" title={row.request_id}>
                {row.request_id}
              </span>
            </div>
          </div>
          <TimeStamp ms={row.ts_ms} align="right" />
        </div>

        <div className="mt-3 grid grid-cols-2 gap-3 text-xs">
          <MobileMetric label="Model" value={row.model} mono />
          <MobileMetric label="Duration" value={formatDurationMs(row.total_ms)} mono />
          <MobileMetric label="Tokens" value={formatNumber(row.total_tokens)} mono />
          <MobileMetric label="Cost" value={formatCurrency(row.cost_usd)} mono />
          <MobileMetric label="TTFB" value={formatDurationMs(row.ttft_ms)} mono />
          <MobileMetric label="Team" value={row.tenant_name || row.tenant_id.slice(0, 8)} />
        </div>
      </button>
    </div>
  );
}

export function SessionCell({ entry }: { entry: Pick<UsageLogEntry, "session_id" | "session_id_source"> }) {
  if (!entry.session_id) {
    return <span className="text-muted-foreground/50">--</span>;
  }
  const source = entry.session_id_source;
  return (
    <span className="inline-flex items-center gap-1">
      <span className="font-mono text-[11px]" title={entry.session_id}>{truncateId(entry.session_id)}</span>
      {source === "client" && (
        <Badge className="border-primary/40 bg-primary/10 text-primary">client</Badge>
      )}
      {source === "derived" && (
        <Badge className="border-border bg-muted/60 text-muted-foreground">derived</Badge>
      )}
    </span>
  );
}

function StatusBadge({ ok, statusCode }: { ok: boolean; statusCode: number }) {
  return (
    <span
      className={cn(
        "inline-flex items-center rounded-sm px-1.5 py-0.5 text-[11px] font-medium",
        ok
          ? "bg-[hsl(158_45%_45%/0.16)] text-[hsl(158_55%_60%)]"
          : "bg-[hsl(350_65%_55%/0.16)] text-[hsl(350_70%_66%)]",
      )}
      title={`HTTP ${statusCode}`}
    >
      {ok ? "Success" : `Error ${statusCode}`}
    </span>
  );
}

function TraceMarker() {
  return (
    <Hexagon
      className="h-2.5 w-2.5 shrink-0 text-emerald-400/90"
      strokeWidth={2.4}
      aria-label="Traced request"
    />
  );
}

function TimeStamp({ ms, align = "left" }: { ms: number; align?: "left" | "right" }) {
  const { date, time } = formatTimeParts(ms);
  return (
    <span
      className={cn(
        "flex flex-col leading-tight",
        align === "right" ? "items-end text-right" : "items-start",
      )}
    >
      <span>{date}</span>
      <span>{time}</span>
    </span>
  );
}

function MobileMetric({
  label,
  value,
  mono,
}: {
  label: string;
  value: string;
  mono?: boolean;
}) {
  return (
    <div className="min-w-0">
      <div className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</div>
      <div className={cn("truncate", mono && "font-mono")}>{value || "--"}</div>
    </div>
  );
}

function formatTimeParts(ms: number): { date: string; time: string } {
  const date = new Date(ms);
  return {
    date: date.toLocaleDateString([], {
      month: "2-digit",
      day: "2-digit",
      year: "2-digit",
    }),
    time: date.toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    }),
  };
}

export function formatWh(wh: number): string {
  if (wh <= 0) return "—";
  if (wh < 0.01) return "< 0.01 Wh";
  return `${wh.toFixed(2)} Wh`;
}
