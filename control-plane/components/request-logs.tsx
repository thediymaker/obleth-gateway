"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { Pause, Play, RefreshCw, Search } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import type { UsageLogEntry } from "@/lib/obleth";
import { cn, formatCurrency, formatNumber } from "@/lib/utils";

const LIVE_POLL_MS = 15_000;
const PAGE_SIZE = 50;

interface TenantOption {
  id: string;
  name: string;
}

interface TimeWindow {
  label: string;
  ms: number;
}

const WINDOWS: TimeWindow[] = [
  { label: "Last 15 min", ms: 15 * 60_000 },
  { label: "Last hour", ms: 60 * 60_000 },
  { label: "Last 24 hours", ms: 24 * 60 * 60_000 },
  { label: "Last 7 days", ms: 7 * 24 * 60 * 60_000 },
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
    queryKey: ["request-logs", filters, cursor, liveTail],
    queryFn: async () => {
      // Live tail advances the lower bound on every poll; paused mode pins it so
      // paging back through history stays anchored.
      const since = liveTail ? Date.now() - filters.windowMs : pinnedSinceRef.current;
      const params = new URLSearchParams();
      params.set("limit", String(PAGE_SIZE));
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
  const hasMore = rows.length === PAGE_SIZE;

  const goNext = () => {
    const last = rows[rows.length - 1];
    if (!last) return;
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

  return (
    <div className="space-y-4">
      <LiveBar
        liveTail={liveTail}
        onToggle={toggleLiveTail}
        onRefresh={() => query.refetch()}
        isFetching={query.isFetching}
        count={rows.length}
        atFirstPage={atFirstPage}
        hasMore={hasMore}
        onPrev={goPrev}
        onNext={goNext}
        windowMs={filters.windowMs}
        onWindowChange={(ms) => patchFilters({ windowMs: ms })}
        requestIdDraft={requestIdDraft}
        onRequestIdDraft={setRequestIdDraft}
        onApplyRequestId={applyRequestId}
      />

      <FilterBar
        filters={filters}
        tenants={tenants}
        models={models}
        onChange={patchFilters}
        onReset={() => {
          setRequestIdDraft("");
          patchFilters(DEFAULT_FILTERS);
        }}
      />

      <Card className="rounded-md">
        <CardContent className="p-0">
          <div className="overflow-x-auto">
            <table className="w-full min-w-[1080px] text-sm">
              <thead>
                <tr className="border-b border-border text-left text-xs text-muted-foreground">
                  <th className="px-4 py-3 font-medium">Time</th>
                  <th className="px-3 py-3 font-medium">Type</th>
                  <th className="px-3 py-3 font-medium">Status</th>
                  <th className="px-3 py-3 font-medium">Model</th>
                  <th className="px-3 py-3 font-medium">Session</th>
                  <th className="px-3 py-3 font-medium">Request ID</th>
                  <th className="px-3 py-3 text-right font-medium">Cost</th>
                  <th className="px-3 py-3 text-right font-medium">Tokens</th>
                  <th className="px-3 py-3 text-right font-medium">TTFT</th>
                  <th className="px-3 py-3 text-right font-medium">Duration</th>
                  <th className="px-3 py-3 font-medium">Team</th>
                  <th className="px-4 py-3 font-medium">Key</th>
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
        </CardContent>
      </Card>
    </div>
  );
}

function LiveBar({
  liveTail,
  onToggle,
  onRefresh,
  isFetching,
  count,
  atFirstPage,
  hasMore,
  onPrev,
  onNext,
  windowMs,
  onWindowChange,
  requestIdDraft,
  onRequestIdDraft,
  onApplyRequestId,
}: {
  liveTail: boolean;
  onToggle: () => void;
  onRefresh: () => void;
  isFetching: boolean;
  count: number;
  atFirstPage: boolean;
  hasMore: boolean;
  onPrev: () => void;
  onNext: () => void;
  windowMs: number;
  onWindowChange: (ms: number) => void;
  requestIdDraft: string;
  onRequestIdDraft: (v: string) => void;
  onApplyRequestId: () => void;
}) {
  return (
    <div className="rounded-md border border-border bg-card px-4 py-3">
      <div className="flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
        <div className="flex flex-wrap items-center gap-2">
          <div className="relative">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              type="search"
              value={requestIdDraft}
              onChange={(e) => onRequestIdDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") onApplyRequestId();
              }}
              placeholder="Search by Request ID"
              aria-label="Search by request id"
              className="h-8 w-64 pl-8 text-xs"
            />
          </div>
          <Select
            value={String(windowMs)}
            onChange={(e) => onWindowChange(Number(e.target.value))}
            aria-label="Time window"
            className="h-8 w-40 text-xs"
          >
            {WINDOWS.map((w) => (
              <option key={w.ms} value={w.ms}>
                {w.label}
              </option>
            ))}
          </Select>
          <Button
            type="button"
            variant={liveTail ? "default" : "secondary"}
            size="sm"
            onClick={onToggle}
            aria-pressed={liveTail}
          >
            {liveTail ? <Pause className="h-3.5 w-3.5" /> : <Play className="h-3.5 w-3.5" />}
            {liveTail ? "Live Tail on" : "Live Tail off"}
          </Button>
          <Button type="button" variant="secondary" size="sm" onClick={onRefresh}>
            <RefreshCw className={cn("h-3.5 w-3.5", isFetching && "animate-spin")} />
            Fetch
          </Button>
        </div>

        <div className="flex flex-wrap items-center gap-3">
          {liveTail ? (
            <Badge className="gap-1.5">
              <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-[hsl(160_14%_58%)]" />
              Auto-refreshing every {LIVE_POLL_MS / 1000}s
            </Badge>
          ) : (
            <span className="text-xs text-muted-foreground">Paused</span>
          )}
          <span className="text-xs tabular-nums text-muted-foreground">
            {formatNumber(count)} shown
          </span>
          <div className="flex items-center gap-1.5">
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={onPrev}
              disabled={liveTail || atFirstPage}
            >
              Previous
            </Button>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={onNext}
              disabled={liveTail || !hasMore}
            >
              Next
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}

function FilterBar({
  filters,
  tenants,
  models,
  onChange,
  onReset,
}: {
  filters: Filters;
  tenants: TenantOption[];
  models: string[];
  onChange: (patch: Partial<Filters>) => void;
  onReset: () => void;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      <Select
        value={filters.tenantId}
        onChange={(e) => onChange({ tenantId: e.target.value })}
        aria-label="Filter by team"
        className="h-8 w-44 text-xs"
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
        onChange={(e) => onChange({ model: e.target.value })}
        aria-label="Filter by model"
        className="h-8 w-44 text-xs"
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
        onChange={(e) => onChange({ requestType: e.target.value })}
        aria-label="Filter by type"
        className="h-8 w-36 text-xs"
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
        onChange={(e) => onChange({ status: e.target.value })}
        aria-label="Filter by status"
        className="h-8 w-32 text-xs"
      >
        <option value="">All status</option>
        <option value="success">Success</option>
        <option value="error">Error</option>
      </Select>
      <Button type="button" variant="ghost" size="sm" onClick={onReset}>
        Reset Filters
      </Button>
    </div>
  );
}

function LogRow({ row }: { row: UsageLogEntry }) {
  const ok = row.status_code >= 200 && row.status_code < 400;

  return (
    <tr className="border-b border-border/60 transition-colors hover:bg-muted/20">
      <td className="px-4 py-2.5 tabular-nums text-muted-foreground">{formatTime(row.ts_ms)}</td>
      <td className="px-3 py-2.5">
        <Badge className="capitalize">{row.request_type || "other"}</Badge>
      </td>
      <td className="px-3 py-2.5">
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
      <td className="px-3 py-2.5">
        <span className="truncate font-mono text-xs">{row.model}</span>
      </td>
      <td className="px-3 py-2.5 font-mono text-xs text-muted-foreground">
        {truncateId(row.session_id) || <span className="text-muted-foreground/50">--</span>}
      </td>
      <td className="px-3 py-2.5 font-mono text-xs text-muted-foreground" title={row.request_id}>
        {row.request_id.slice(0, 8)}
      </td>
      <td className="px-3 py-2.5 text-right tabular-nums">{formatCurrency(row.cost_usd)}</td>
      <td className="px-3 py-2.5 text-right tabular-nums text-muted-foreground">
        {formatNumber(row.total_tokens)}
      </td>
      <td className="px-3 py-2.5 text-right tabular-nums text-muted-foreground">
        {formatSeconds(row.ttft_ms)}
      </td>
      <td className="px-3 py-2.5 text-right tabular-nums text-muted-foreground">
        {formatSeconds(row.total_ms)}
      </td>
      <td className="px-3 py-2.5">
        <span className="truncate">{row.tenant_name || row.tenant_id.slice(0, 8)}</span>
      </td>
      <td className="px-4 py-2.5">
        <div className="min-w-0">
          <p className="truncate text-xs">{row.key_name || "--"}</p>
          {row.key_prefix && (
            <p className="truncate font-mono text-[10px] text-muted-foreground">{row.key_prefix}</p>
          )}
        </div>
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
