"use client";

import { useMemo, useState, useTransition } from "react";
import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { Search } from "lucide-react";
import { cancelBatchAction } from "@/app/(dashboard)/batches/batches-actions";
import { Tile } from "@/components/models/ui";
import { Pill, Segmented } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import {
  BATCH_FILTERS,
  batchName,
  batchState,
  doneOf,
  duration,
  filterCount,
  isActive,
  ownerOf,
  percentOf,
  type BatchFilter,
  type BatchTone,
} from "@/lib/batches-model";
import { tenantHref } from "@/lib/access-model";
import { logsHref } from "@/lib/log-links";
import { modelHref } from "@/lib/model-links";
import type { AdminBatch, AdminBatchList } from "@/lib/obleth";
import { compact } from "@/lib/overview-model";
import { cn, getJson } from "@/lib/utils";

const COLS = "grid-cols-[minmax(0,1.3fr)_minmax(0,1fr)_minmax(0,0.9fr)_minmax(220px,1.6fr)_minmax(0,1.3fr)_88px]";

function since(iso: string | null, now: number): string {
  if (!iso) return "";
  const s = (now - Date.parse(iso)) / 1000;
  return Number.isFinite(s) ? `${duration(s)} ago` : "";
}

/** The progress bar: succeeded solid, failed hatched, the rest empty. */
function Progress({ b, tone }: { b: AdminBatch; tone: BatchTone }) {
  const ok = b.total ? (b.completed / b.total) * 100 : 0;
  const bad = b.total ? (b.failed / b.total) * 100 : 0;
  return (
    <span
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={percentOf(b)}
      aria-label={`${percentOf(b)}% finished`}
      className="flex h-2 overflow-hidden rounded-full bg-muted"
    >
      <span className={cn("h-full transition-[width] duration-700", tone === "waiting" ? "bg-muted-foreground" : "bg-foreground")} style={{ width: `${ok}%` }} />
      <span
        className="h-full bg-[repeating-linear-gradient(135deg,hsl(var(--muted-foreground))_0_2px,transparent_2px_5px)]"
        style={{ width: `${bad}%` }}
      />
    </span>
  );
}

function StateLabel({ tone, label }: { tone: BatchTone; label: string }) {
  if (tone === "problem") return <Pill inverted className="h-5 px-2 text-[11px]">{label}</Pill>;
  return (
    <span className="inline-flex items-center gap-1.5 text-[12.5px]">
      <span
        aria-hidden
        className={cn(
          "h-[7px] w-[7px] rounded-full",
          tone === "running" && "animate-pulse bg-foreground",
          tone === "waiting" && "border border-foreground",
          tone === "done" && "bg-foreground",
          tone === "stopped" && "border border-dashed border-muted-foreground",
        )}
      />
      {label}
    </span>
  );
}

export function BatchesView({ initial, initialFilter }: { initial: AdminBatchList | null; initialFilter: BatchFilter }) {
  const router = useRouter();
  const pathname = usePathname();
  const [filter, setFilter] = useState<BatchFilter>(initialFilter);
  const [query, setQuery] = useState("");
  const [pending, startTransition] = useTransition();
  const [cancelling, setCancelling] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const status = BATCH_FILTERS[filter].status;
  const live = useQuery({
    queryKey: ["batches", filter],
    queryFn: () => getJson<AdminBatchList>(`/api/live/batches?limit=100${status ? `&status=${status}` : ""}`),
    initialData: filter === initialFilter ? initial ?? undefined : undefined,
    refetchInterval: (q) => (q.state.data?.data.some((b) => isActive(b.status)) ? 3_000 : 15_000),
  });
  const data = live.data ?? initial;
  const now = live.dataUpdatedAt || Date.now();
  const counts = data?.counts ?? {};
  const batches = useMemo(() => {
    const q = query.trim().toLowerCase();
    const all = data?.data ?? [];
    if (!q) return all;
    return all.filter((b) => [b.id, b.model, b.tenant_name, b.key_name, batchName(b)].some((v) => v?.toLowerCase().includes(q)));
  }, [data, query]);

  const active = (data?.data ?? []).filter((b) => b.status === "in_progress");
  const remaining = active.reduce((n, b) => n + (b.total - doneOf(b)), 0);
  const waiting = active.filter((b) => b.waiting);
  const models = new Set(active.map((b) => b.model));

  const choose = (f: BatchFilter) => {
    setFilter(f);
    router.replace(f === "active" ? pathname : `${pathname}?show=${f}`, { scroll: false });
  };

  const cancel = (b: AdminBatch) => {
    if (!window.confirm(`Cancel ${batchName(b) ?? b.id}? Queued requests stop; the ${b.total - doneOf(b)} not yet finished won't run.`)) return;
    setError(null);
    setCancelling(b.id);
    startTransition(async () => {
      const r = await cancelBatchAction(b.id);
      if (!r.ok) setError(r.error);
      await live.refetch();
      setCancelling(null);
    });
  };

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-[18px]">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <h1 className="text-[26px] font-semibold tracking-tight">Batches</h1>
          <p className="text-[13px] text-secondary-foreground">
            Files of requests that run in the background, only while no one is waiting for the model. Callers submit them through <span className="font-mono text-[12px]">/v1/batches</span>.
          </p>
        </div>
      </div>

      {data == null && <p className="rounded-xl border border-border bg-card px-4 py-3 text-[13px] text-secondary-foreground">The batch list couldn&apos;t be read from the gateway. It retries every few seconds.</p>}

      <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
        <Tile label="Running now" value={counts.in_progress ?? 0} detail={active.length ? `on ${models.size} model${models.size === 1 ? "" : "s"}` : "Nothing running"} />
        <Tile label="Requests to go" value={compact(remaining)} detail={active.length ? "across the running batches" : null} />
        <Tile
          label="Waiting for capacity"
          value={waiting.length}
          emphasis={waiting.length > 0}
          detail={waiting.length ? `${[...new Set(waiting.map((b) => b.model))].join(" · ")} busy` : "Every running batch is moving"}
        />
        <Tile label="Done" value={compact(counts.completed ?? 0)} detail={(counts.expired ?? 0) ? `${counts.expired} ran out of time` : null} />
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <label className="flex h-9 min-w-[240px] max-w-[380px] flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground">
          <Search className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden />
          <input value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search batches" placeholder="Batch, model, tenant or key" className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground" />
        </label>
        <Segmented
          label="Show"
          value={filter}
          onChange={choose}
          options={(Object.keys(BATCH_FILTERS) as BatchFilter[]).map((f) => ({ value: f, label: `${BATCH_FILTERS[f].label} ${filterCount(f, counts)}` }))}
        />
      </div>

      {error && <p role="alert" className="text-[13px] text-foreground">Couldn&apos;t cancel: {error}</p>}

      <section aria-label="Batches" className="overflow-hidden rounded-xl border border-border bg-card">
        <div className="overflow-x-auto">
          <div className="min-w-[1040px]">
            <div className={cn("grid items-center gap-3.5 px-[18px] py-2.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", COLS)}>
              <span>Batch</span><span>Submitted by</span><span>Model</span><span>Progress</span><span>Status</span><span />
            </div>
            {batches.length === 0 && (
              <p className="border-t border-border px-[18px] py-8 text-center text-[13px] text-muted-foreground">
                {query ? "Nothing matches this search." : filter === "active" ? "No batches are running." : "No batches here yet."}
              </p>
            )}
            {batches.map((b) => {
              const s = batchState(b, now);
              const name = batchName(b);
              return (
                <div key={b.id} className={cn("grid min-h-[60px] items-center gap-3.5 border-t border-border px-[18px] py-2.5 text-[13px]", COLS)}>
                  <span className="flex min-w-0 flex-col">
                    <span className="truncate font-medium" title={name ?? b.id}>{name ?? <span className="font-mono text-[12.5px]">{b.id}</span>}</span>
                    <span className="truncate text-[11.5px] text-muted-foreground">
                      {name && <span className="font-mono">{b.id} · </span>}
                      {since(b.created_at, now)}
                    </span>
                  </span>
                  <span className="flex min-w-0 flex-col">
                    {b.tenant_name ? <Link href={tenantHref({ name: b.tenant_name })} className="truncate hover:underline">{b.tenant_name}</Link> : <span className="truncate text-muted-foreground">a deleted tenant</span>}
                    <span className="truncate text-[11.5px] text-muted-foreground" title={ownerOf(b)}>{b.key_name ?? "key removed"}</span>
                  </span>
                  <span className="flex min-w-0 flex-col">
                    <Link href={modelHref(b.model)} className="truncate font-mono text-[12.5px] hover:underline">{b.model}</Link>
                    <span className="truncate text-[11.5px] text-muted-foreground">{b.endpoint.replace("/v1/", "")}</span>
                  </span>
                  <span className="flex min-w-0 flex-col gap-1.5">
                    <Progress b={b} tone={s.tone} />
                    <span className="flex justify-between gap-2 font-mono text-[11.5px] text-secondary-foreground">
                      <span>{doneOf(b).toLocaleString()} of {b.total.toLocaleString()}{b.failed ? ` · ${b.failed.toLocaleString()} failed` : ""}</span>
                      <span className="text-foreground">{percentOf(b)}%</span>
                    </span>
                  </span>
                  <span className="flex min-w-0 flex-col items-start gap-0.5">
                    <StateLabel tone={s.tone} label={s.label} />
                    {s.detail && <span className="line-clamp-2 max-w-full text-[11.5px] leading-snug text-muted-foreground" title={s.detail}>{s.detail}</span>}
                    {!s.detail && b.finished_at && <span className="text-[11.5px] text-muted-foreground">{since(b.finished_at, now)}</span>}
                  </span>
                  <span className="flex justify-end gap-1">
                    {(b.status === "in_progress" || b.status === "validating") ? (
                      <Button type="button" variant="outline" size="sm" className="h-7 px-2.5 text-[12px]" disabled={pending && cancelling === b.id} onClick={() => cancel(b)}>
                        {pending && cancelling === b.id ? "Cancelling" : "Cancel"}
                      </Button>
                    ) : doneOf(b) > 0 ? (
                      <Link href={logsHref({ model: b.model, team: b.tenant_name ?? undefined, window: "7d" })} className="text-[12px] text-secondary-foreground underline underline-offset-2 hover:text-foreground">Requests</Link>
                    ) : null}
                  </span>
                </div>
              );
            })}
          </div>
        </div>
        {data?.has_more && <p className="border-t border-border px-[18px] py-2.5 text-[12px] text-muted-foreground">Showing the newest 100.</p>}
      </section>
    </div>
  );
}
