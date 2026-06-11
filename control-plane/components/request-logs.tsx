"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { ChevronLeft, ChevronRight, Pause, Play, RefreshCw, Search } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import type { UsageLogEntry } from "@/lib/obleth";
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
}

const DEFAULT_FILTERS: Filters = {
  windowMs: WINDOWS[2].ms,
  tenantId: "",
  model: "",
  requestType: "",
  status: "",
  requestId: "",
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
      filters.windowMs !== DEFAULT_FILTERS.windowMs,
  );

  return (
    <Card className="overflow-hidden rounded-md">
      {/* Toolbar */}
      <div className="flex flex-col gap-2.5 border-b border-border bg-muted/20 px-3 py-2.5">
        <div className="flex flex-wrap items-center gap-2">
          <div className="relative min-w-[200px] flex-1">
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
            className="h-8 w-36 text-xs"
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
            className="h-8 w-28 text-xs"
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
          >
            {liveTail ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
            {liveTail ? "Live" : "Paused"}
          </Button>
          <Button type="button" variant="secondary" size="sm" onClick={() => query.refetch()}>
            <RefreshCw className={cn("h-3.5 w-3.5", query.isFetching && "animate-spin")} />
            Fetch
          </Button>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <Select
            value={filters.tenantId}
            onChange={(e) => patchFilters({ tenantId: e.target.value })}
            aria-label="Filter by team"
            className="h-8 w-40 text-xs"
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
            className="h-8 w-40 text-xs"
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
            className="h-8 w-32 text-xs"
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
            className="h-8 w-28 text-xs"
          >
            <option value="">All status</option>
            <option value="success">Success</option>
            <option value="error">Error</option>
          </Select>
          {filtersActive && (
            <Button type="button" variant="ghost" size="sm" onClick={resetFilters}>
              Reset
            </Button>
          )}
        </div>

        <div className="flex items-center justify-between gap-3 border-t border-border/60 pt-2.5">
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
      <div className="overflow-x-auto">
        <table className="w-full min-w-[1080px] text-sm">
          <thead>
            <tr className="border-b border-border text-left text-xs text-muted-foreground">
              <th className="px-4 py-2 font-medium">Time</th>
              <th className="px-3 py-2 font-medium">Type</th>
              <th className="px-3 py-2 font-medium">Status</th>
              <th className="px-3 py-2 font-medium">Model</th>
              <th className="px-3 py-2 font-medium">Session</th>
              <th className="px-3 py-2 font-medium">Request ID</th>
              <th className="px-3 py-2 text-right font-medium">Cost</th>
              <th className="px-3 py-2 text-right font-medium">Tokens</th>
              <th className="px-3 py-2 text-right font-medium">TTFT</th>
              <th className="px-3 py-2 text-right font-medium">Duration</th>
              <th className="px-3 py-2 font-medium">Team</th>
              <th className="px-4 py-2 font-medium">Key</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <LogRow key={`${row.request_id}-${row.ts_ms}`} row={row} />
            ))}
            {rows.length === 0 && (
              <tr>
                <td colSpan={12} className="px-4 py-16 text-center text-muted-foreground">
                  {query.isLoading
                    ? "Loading requests..."
                    : "No requests match the current filters and window."}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </Card>
  );
}

function LogRow({ row }: { row: UsageLogEntry }) {
  const ok = row.status_code >= 200 && row.status_code < 400;

  return (
    <tr className="border-b border-border/60 transition-colors hover:bg-muted/20">
      <td className="px-4 py-1.5 tabular-nums text-muted-foreground">{formatTime(row.ts_ms)}</td>
      <td className="px-3 py-1.5">
        <Badge className="capitalize">{row.request_type || "other"}</Badge>
      </td>
      <td className="px-3 py-1.5">
        <span
          className={cn(
            "inline-flex items-center rounded-sm px-1.5 py-0.5 text-[11px] font-medium",
            ok
              ? "bg-[hsl(158_45%_45%/0.16)] text-[hsl(158_55%_60%)]"
              : "bg-[hsl(350_65%_55%/0.16)] text-[hsl(350_70%_66%)]",
          )}
          title={`HTTP ${row.status_code}`}
        >
          {ok ? "Success" : `Error ${row.status_code}`}
        </span>
      </td>
      <td className="px-3 py-1.5">
        <span className="truncate font-mono text-xs">{row.model}</span>
      </td>
      <td className="px-3 py-1.5 font-mono text-xs text-muted-foreground">
        {truncateId(row.session_id) || <span className="text-muted-foreground/50">--</span>}
      </td>
      <td className="px-3 py-1.5 font-mono text-xs text-muted-foreground" title={row.request_id}>
        {row.request_id.slice(0, 8)}
      </td>
      <td className="px-3 py-1.5 text-right tabular-nums">{formatCurrency(row.cost_usd)}</td>
      <td className="px-3 py-1.5 text-right tabular-nums text-muted-foreground">
        {formatNumber(row.total_tokens)}
      </td>
      <td className="px-3 py-1.5 text-right tabular-nums text-muted-foreground">
        {formatSeconds(row.ttft_ms)}
      </td>
      <td className="px-3 py-1.5 text-right tabular-nums text-muted-foreground">
        {formatSeconds(row.total_ms)}
      </td>
      <td className="px-3 py-1.5">
        <span className="truncate">{row.tenant_name || row.tenant_id.slice(0, 8)}</span>
      </td>
      <td className="px-4 py-1.5">
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

function formatTime(ms: number): string {
  return new Date(ms).toLocaleString([], {
    month: "2-digit",
    day: "2-digit",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

function formatSeconds(ms: number): string {
  if (!ms || ms <= 0) return "--";
  return `${(ms / 1000).toFixed(2)}s`;
}

function truncateId(id: string): string {
  if (!id) return "";
  return id.length > 12 ? `${id.slice(0, 12)}...` : id;
}
