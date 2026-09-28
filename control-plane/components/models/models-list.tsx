"use client";

import { useCallback, useEffect, useMemo, useRef, useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { Check, ChevronDown, Download, Plus, Search, Upload, X } from "lucide-react";
import { checkModelsHealthAction, deleteModelsAction, setModelsEnabledAction, type BulkModelResult } from "@/app/actions";
import { useCapacityDiscovery, useFairshareLive } from "@/components/fairshare/hooks";
import { AddModelSheet, type AddMode } from "@/components/models/add-model";
import { Notice, ProviderMark, StatusMark, Tile } from "@/components/models/ui";
import { Meter, Segmented } from "@/components/overview/ui";
import type { RecipeCard } from "@/components/recipes/recipe-card";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import {
  buildModelRows,
  contextLabel,
  EMPTY_FILTERS,
  filterRows,
  groupCounts,
  MODEL_TYPE_NAMES,
  modelHref,
  RUNS_LABELS,
  SORT_LABELS,
  sortRows,
  statusLine,
  tagLabels,
  TYPE_GROUPS,
  type ModelFilters,
  type ModelRow,
  type ModelSort,
  type RunsOn,
  type StatusFilter,
  type TypeGroup,
} from "@/lib/models-model";
import type { CacheStats, ModelHealthSummary, ModelRoute, UsageModelAgg } from "@/lib/obleth";
import { compact, formatMs, isBenchmarkRoute } from "@/lib/overview-model";
import { logsHref } from "@/lib/log-links";
import { cn, getJson } from "@/lib/utils";

const DAY_MS = 86_400_000;

const STATUS_FILTERS: { value: StatusFilter; label: string }[] = [
  { value: "all", label: "Any status" },
  { value: "attention", label: "Needs attention" },
  { value: "off", label: "Switched off" },
];

export function ModelsList({
  models,
  health: initialHealth,
  managed,
  cacheStats,
  slurmEnabled,
  recipeCards,
  initialAdd,
}: {
  models: ModelRoute[];
  health: ModelHealthSummary[];
  managed: Record<string, boolean>;
  cacheStats?: CacheStats;
  slurmEnabled: boolean;
  recipeCards: RecipeCard[];
  initialAdd?: AddMode | null;
}) {
  const router = useRouter();
  const { confirm, confirmElement } = useConfirm();
  const [filters, setFilters] = useState<ModelFilters>(EMPTY_FILTERS);
  const [sort, setSort] = useState<ModelSort>("busiest");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [adding, setAdding] = useState<AddMode | null>(initialAdd ?? null);
  const [notice, setNotice] = useState<{ text: string; failed: BulkModelResult["failed"] } | null>(null);
  const [pending, start] = useTransition();
  const search = useRef<HTMLInputElement>(null);

  const healthQuery = useQuery({
    queryKey: ["models-health"],
    queryFn: () => getJson<ModelHealthSummary[]>("/api/live/models/health"),
    initialData: initialHealth,
    refetchInterval: 20_000,
  });
  const usageQuery = useQuery({
    queryKey: ["usage-models", "24h"],
    queryFn: () => getJson<UsageModelAgg[]>(`/api/live/usage/models?since_ms=${Date.now() - DAY_MS}`),
    refetchInterval: 60_000,
  });
  const fairshare = useFairshareLive();
  const discovery = useCapacityDiscovery();

  const rows = useMemo(
    () => buildModelRows(models, healthQuery.data ?? [], managed, usageQuery.data ?? [], fairshare.data, discovery.data),
    [models, healthQuery.data, managed, usageQuery.data, fairshare.data, discovery.data],
  );
  const real = useMemo(() => rows.filter((r) => !isBenchmarkRoute(r.model)), [rows]);
  const benchmarkCount = rows.length - real.length;
  const shown = useMemo(() => sortRows(filterRows(rows, filters), sort), [rows, filters, sort]);
  const counts = groupCounts(filters.benchmarks ? rows : real);
  const filtered = filters.query !== "" || filters.group !== "all" || filters.status !== "all" || filters.runs !== "all";

  // Selection only ever holds rows that still exist.
  useEffect(() => {
    setSelected((prev) => {
      const ids = new Set(models.map((m) => m.id));
      const next = new Set([...prev].filter((id) => ids.has(id)));
      return next.size === prev.size ? prev : next;
    });
  }, [models]);

  // "/" jumps to the search box, as it does in most lists.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "/" || (e.target instanceof Element && e.target.closest("input, textarea, select, [contenteditable=true]"))) return;
      e.preventDefault();
      search.current?.focus();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const set = <K extends keyof ModelFilters>(key: K, value: ModelFilters[K]) => setFilters((f) => ({ ...f, [key]: value }));
  const down = real.filter((r) => r.status === "down");
  const pressured = real.filter((r) => r.queued > 0 || (r.cap > 0 && r.inFlight / r.cap >= 0.9));
  const hits = cacheStats?.hits ?? 0;
  const lookups = hits + (cacheStats?.misses ?? 0);
  const selectedRows = rows.filter((r) => selected.has(r.id));
  const allOn = selectedRows.length > 0 && selectedRows.every((r) => r.model.enabled);
  const visibleIds = shown.map((r) => r.id);
  const allVisibleSelected = visibleIds.length > 0 && visibleIds.every((id) => selected.has(id));

  const toggle = (id: string) => setSelected((prev) => { const next = new Set(prev); if (next.has(id)) next.delete(id); else next.add(id); return next; });
  const toggleAll = () => setSelected((prev) => (allVisibleSelected ? new Set([...prev].filter((id) => !visibleIds.includes(id))) : new Set([...prev, ...visibleIds])));

  function report(verb: string, result: BulkModelResult) {
    const n = result.done;
    setNotice({ text: `${verb} ${n} model${n === 1 ? "" : "s"}${result.failed.length ? `; ${result.failed.length} failed` : ""}.`, failed: result.failed });
  }

  function bulk(kind: "check" | "on" | "off" | "delete") {
    const ids = [...selected];
    start(async () => {
      if (kind === "check") report("Checked", await checkModelsHealthAction(ids));
      if (kind === "on") report("Turned on", await setModelsEnabledAction(ids, true));
      if (kind === "off") {
        const ok = await confirm({
          title: `Turn off ${ids.length} model${ids.length === 1 ? "" : "s"}?`,
          description: "Requests naming them start failing straight away. Their settings are kept, and you can turn them back on.",
          confirmLabel: "Turn off",
        });
        if (ok) report("Turned off", await setModelsEnabledAction(ids, false));
      }
      if (kind === "delete") {
        const names = selectedRows.map((r) => r.name);
        const ok = await confirm({
          title: `Delete ${ids.length} model${ids.length === 1 ? "" : "s"}?`,
          description: `${names.slice(0, 5).join(", ")}${names.length > 5 ? ` and ${names.length - 5} more` : ""}. Requests naming them start failing immediately. This cannot be undone.`,
          confirmLabel: "Delete",
        });
        if (ok) {
          const result = await deleteModelsAction(ids);
          report("Deleted", result);
          setSelected(new Set());
        }
      }
      router.refresh();
    });
  }

  const exportSelected = () => {
    const names = selectedRows.map((r) => r.name);
    window.location.href = `/api/live/models/export?names=${encodeURIComponent(names.join(","))}`;
  };

  const closeAdd = useCallback(() => setAdding(null), []);

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-5">
      {confirmElement}
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-[26px] font-semibold tracking-tight">Models</h1>
          <p className="mt-1 text-[13px] text-muted-foreground">{real.length === 0 ? "No models yet" : statusLine(real)}</p>
        </div>
        <div className="flex items-center gap-2">
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button type="button" variant="outline" size="sm" className="h-9">
                Import or export
                <ChevronDown className="h-3.5 w-3.5" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => setAdding("file")}><Upload className="mr-2 h-3.5 w-3.5" />Import a file…</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => setAdding("provider")}><Download className="mr-2 h-3.5 w-3.5" />Import from a provider…</DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem disabled={models.length === 0} onSelect={() => { window.location.href = "/api/live/models/export"; }}>
                <Download className="mr-2 h-3.5 w-3.5" />Export every model
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
          <Button type="button" size="sm" className="h-9" onClick={() => setAdding("connect")}>
            <Plus className="h-3.5 w-3.5" />
            Add model
          </Button>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
        <Tile label="Models" value={real.length} detail={`${real.filter((r) => r.model.enabled).length} on · ${real.filter((r) => !r.model.enabled).length} off`} />
        <Tile
          label="Failing health checks"
          value={down.length}
          detail={down.length ? down.map((r) => r.name).slice(0, 2).join(", ") + (down.length > 2 ? ` +${down.length - 2}` : "") : "Every check passing"}
          emphasis={down.length > 0}
          onClick={down.length ? () => set("status", filters.status === "attention" ? "all" : "attention") : undefined}
          pressed={filters.status === "attention"}
        />
        <Tile
          label="Pools under pressure"
          value={pressured.length}
          detail={pressured.length ? pressured.map((r) => r.name).slice(0, 2).join(", ") + (pressured.length > 2 ? ` +${pressured.length - 2}` : "") : "Room in every pool"}
          emphasis={pressured.some((r) => r.queued > 0)}
        />
        <Tile label="Response cache" value={lookups > 0 ? `${((hits / lookups) * 100).toFixed(1)}%` : "—"} detail={lookups > 0 ? `hit rate · ${compact(cacheStats?.tokens_saved ?? 0)} tokens saved` : "No cached lookups yet"} />
      </div>

      <div className="flex flex-col gap-3">
        <div className="flex flex-wrap items-center gap-2">
          <label className="flex h-9 min-w-[16rem] flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground lg:max-w-md">
            <Search className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden />
            <input
              ref={search}
              value={filters.query}
              onChange={(e) => set("query", e.target.value)}
              placeholder="Search name, alias, upstream or tag"
              aria-label="Search models"
              className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground"
            />
            {filters.query ? (
              <button type="button" onClick={() => set("query", "")} aria-label="Clear search" className="text-muted-foreground hover:text-foreground"><X className="h-3.5 w-3.5" /></button>
            ) : (
              <kbd className="rounded border border-border px-1.5 font-mono text-[11px] text-muted-foreground">/</kbd>
            )}
          </label>
          <Segmented<TypeGroup | "all">
            label="Model type"
            value={filters.group}
            onChange={(v) => set("group", v)}
            options={[
              { value: "all", label: <>All <span className="ml-1 tabular-nums text-muted-foreground">{counts.all}</span></> },
              ...TYPE_GROUPS.filter((g) => counts[g.value] > 0).map((g) => ({ value: g.value, label: <>{g.label} <span className="ml-1 tabular-nums text-muted-foreground">{counts[g.value]}</span></> })),
            ]}
          />
          <Choice label="Status" value={filters.status} options={STATUS_FILTERS} onChange={(v) => set("status", v)} />
          <Choice
            label="Runs on"
            value={filters.runs}
            options={[{ value: "all", label: "Runs anywhere" }, ...(Object.keys(RUNS_LABELS) as RunsOn[]).map((r) => ({ value: r, label: RUNS_LABELS[r] }))]}
            onChange={(v) => set("runs", v as RunsOn | "all")}
          />
          <Choice label="Sort" value={sort} prefix="Sort: " options={(Object.keys(SORT_LABELS) as ModelSort[]).map((s) => ({ value: s, label: SORT_LABELS[s] }))} onChange={(v) => setSort(v as ModelSort)} />
          {benchmarkCount > 0 && (
            <button type="button" onClick={() => set("benchmarks", !filters.benchmarks)} className="text-[12.5px] text-muted-foreground underline-offset-2 hover:text-foreground hover:underline">
              {filters.benchmarks ? "Hide" : "Show"} {benchmarkCount} benchmark route{benchmarkCount === 1 ? "" : "s"}
            </button>
          )}
        </div>
        {notice && (
          <Notice onDismiss={() => setNotice(null)} strong={notice.failed.length > 0}>
            <p>{notice.text}</p>
            {notice.failed.length > 0 && (
              <ul className="mt-1 space-y-0.5 text-xs text-muted-foreground">
                {notice.failed.map((f) => <li key={f.name}><span className="font-mono text-foreground">{f.name}</span>: {f.error}</li>)}
              </ul>
            )}
          </Notice>
        )}
      </div>

      <section aria-label="Models" className="overflow-hidden rounded-xl border border-border bg-card">
        <div className="overflow-x-auto">
          <table className="w-full min-w-[980px] text-[13px]">
            <thead>
              <tr className="border-b border-border text-left text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">
                <th className="w-10 py-2.5 pl-4">
                  <input type="checkbox" aria-label="Select every model shown" checked={allVisibleSelected} onChange={toggleAll} className="h-3.5 w-3.5 accent-foreground" />
                </th>
                <th className="py-2.5 pr-3 font-semibold">Model</th>
                <th className="py-2.5 pr-3 font-semibold">Type</th>
                <th className="py-2.5 pr-3 font-semibold">Health</th>
                <th className="w-40 py-2.5 pr-3 font-semibold">Load now</th>
                <th className="py-2.5 pr-3 text-right font-semibold">Req · 24h</th>
                <th className="py-2.5 pr-3 text-right font-semibold">First token</th>
                <th className="py-2.5 pr-3 font-semibold">Price</th>
                <th className="py-2.5 pr-3 text-right font-semibold">Context</th>
                <th className="py-2.5 pr-4 font-semibold">Router tags</th>
              </tr>
            </thead>
            <tbody>
              {shown.map((row) => (
                <Row key={row.id} row={row} selected={selected.has(row.id)} onToggle={() => toggle(row.id)} onOpen={() => router.push(modelHref(row.name))} />
              ))}
            </tbody>
          </table>
        </div>
        {shown.length === 0 && (
          <div className="flex flex-col items-center gap-3 px-6 py-14 text-center text-[13px] text-muted-foreground">
            {models.length === 0 ? (
              <>
                <p>No models yet. Point obleth at an endpoint that already serves one.</p>
                <Button type="button" size="sm" onClick={() => setAdding("connect")}><Plus className="h-3.5 w-3.5" />Add model</Button>
              </>
            ) : (
              <>
                <p>No model matches{filters.query ? ` “${filters.query}”` : ""}{filtered ? " with these filters" : ""}.</p>
                <Button type="button" size="sm" variant="outline" onClick={() => setFilters((f) => ({ ...EMPTY_FILTERS, benchmarks: f.benchmarks }))}>Clear filters</Button>
              </>
            )}
          </div>
        )}
        {shown.length > 0 && (
          <div className="border-t border-border px-4 py-2.5 text-xs text-muted-foreground">
            {shown.length === rows.length ? `${shown.length} models` : `${shown.length} of ${filters.benchmarks ? rows.length : real.length} models`}
          </div>
        )}
      </section>

      {selected.size > 0 && (
        <div role="region" aria-label="Selected models" className="sticky bottom-5 z-30 mx-auto flex flex-wrap items-center gap-2 rounded-xl border border-muted-foreground/50 bg-card px-3 py-2 shadow-2xl">
          <span className="inline-flex h-[22px] items-center rounded-full bg-foreground px-2 text-[11.5px] font-semibold text-background">{selected.size} selected</span>
          <Button type="button" size="sm" variant="ghost" disabled={pending} onClick={() => bulk("check")}>Check health</Button>
          <Button type="button" size="sm" variant="ghost" disabled={pending} onClick={() => bulk(allOn ? "off" : "on")}>{allOn ? "Turn off" : "Turn on"}</Button>
          <Button type="button" size="sm" variant="ghost" disabled={pending} onClick={exportSelected}>Export</Button>
          <Button type="button" size="sm" variant="ghost" disabled={pending} onClick={() => bulk("delete")}>Delete…</Button>
          <button type="button" onClick={() => setSelected(new Set())} aria-label="Clear the selection" className="ml-1 inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-secondary hover:text-foreground"><X className="h-4 w-4" /></button>
        </div>
      )}

      <AddModelSheet
        mode={adding}
        onModeChange={setAdding}
        onClose={closeAdd}
        slurmEnabled={slurmEnabled}
        recipeCards={recipeCards}
        models={models}
      />
    </div>
  );
}

function Row({ row, selected, onToggle, onOpen }: { row: ModelRow; selected: boolean; onToggle: () => void; onOpen: () => void }) {
  const m = row.model;
  const off = row.status === "off";
  const tags = tagLabels(m.tags);
  const textual = m.model_type === "chat" || m.model_type === "embedding";
  return (
    <tr
      onClick={onOpen}
      className={cn("cursor-pointer border-b border-border/70 transition-colors last:border-b-0 hover:bg-muted/30", selected && "bg-muted/40", off && "text-muted-foreground")}
    >
      <td className="py-2.5 pl-4" onClick={(e) => e.stopPropagation()}>
        <input type="checkbox" aria-label={`Select ${row.name}`} checked={selected} onChange={onToggle} className="h-3.5 w-3.5 accent-foreground" />
      </td>
      <td className="max-w-[24rem] py-2.5 pr-3">
        <div className="flex min-w-0 items-center gap-2.5">
          <ProviderMark name={row.name} upstream={m.upstream_model} className={cn(off && "opacity-50")} />
          <div className="min-w-0">
            <Link href={modelHref(row.name)} onClick={(e) => e.stopPropagation()} className={cn("block truncate font-medium hover:underline", off ? "text-secondary-foreground" : "text-foreground")}>
              {row.name}
            </Link>
            <span className="block truncate font-mono text-[11.5px] text-muted-foreground" title={m.upstream_model}>
              {(m.aliases?.length ?? 0) > 0 ? `also ${m.aliases.join(", ")} · ` : ""}{m.upstream_model}
            </span>
          </div>
        </div>
      </td>
      <td className="whitespace-nowrap py-2.5 pr-3 text-secondary-foreground">{MODEL_TYPE_NAMES[m.model_type] ?? m.model_type}</td>
      <td className="whitespace-nowrap py-2.5 pr-3"><StatusMark status={row.status} /></td>
      <td className="py-2.5 pr-3">
        {off ? (
          <span className="text-muted-foreground">—</span>
        ) : row.cap > 0 ? (
          <div className="flex items-center gap-2">
            <Meter value={row.inFlight} max={row.cap} strong={row.queued > 0} className="w-16" />
            <span className="whitespace-nowrap font-mono text-[12px] tabular-nums">{row.inFlight}/{row.cap}</span>
            {row.queued > 0 && <span className="whitespace-nowrap rounded-full bg-foreground px-1.5 text-[10.5px] font-semibold text-background">{row.queued} waiting</span>}
          </div>
        ) : (
          <span className="font-mono text-[12px] tabular-nums text-muted-foreground">{row.inFlight > 0 ? `${row.inFlight} in flight` : "no cap"}</span>
        )}
      </td>
      <td className="py-2.5 pr-3 text-right font-mono text-[12px] tabular-nums">
        {row.requests ? (
          <Link href={logsHref({ model: row.name, window: "24h" })} onClick={(e) => e.stopPropagation()} title="See these requests" className="underline-offset-2 hover:underline">{compact(row.requests)}</Link>
        ) : "—"}
      </td>
      <td className="py-2.5 pr-3 text-right font-mono text-[12px] tabular-nums">{row.p50TtftMs ? formatMs(row.p50TtftMs) : "—"}</td>
      <td className="whitespace-nowrap py-2.5 pr-3 font-mono text-[12px]">{row.price ?? <span className="text-muted-foreground">not set</span>}</td>
      <td className="py-2.5 pr-3 text-right font-mono text-[12px] tabular-nums">{textual ? contextLabel(m.context_window) : "—"}</td>
      <td className="max-w-[16rem] truncate py-2.5 pr-4 text-[12.5px] text-secondary-foreground" title={tags.join(" · ")}>{tags.length ? tags.join(" · ") : <span className="text-muted-foreground">—</span>}</td>
    </tr>
  );
}

function Choice<T extends string>({ label, value, options, onChange, prefix = "" }: {
  label: string; value: T; options: { value: T; label: string }[]; onChange: (value: T) => void; prefix?: string;
}) {
  const current = options.find((o) => o.value === value) ?? options[0];
  const active = value !== options[0].value && !prefix;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          aria-label={`${label}: ${current.label}`}
          className={cn(
            "inline-flex h-9 items-center gap-1.5 rounded-lg border px-3 text-[12.5px] transition-colors hover:text-foreground",
            active ? "border-foreground text-foreground" : "border-border text-secondary-foreground",
          )}
        >
          {prefix}{current.label}
          <ChevronDown className="h-3.5 w-3.5 text-muted-foreground" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start">
        <DropdownMenuLabel className="text-[11px] uppercase tracking-[0.07em] text-muted-foreground">{label}</DropdownMenuLabel>
        {options.map((o) => (
          <DropdownMenuItem key={o.value} onSelect={() => onChange(o.value)}>
            <Check className={cn("mr-2 h-3.5 w-3.5", o.value !== value && "invisible")} />
            {o.label}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
