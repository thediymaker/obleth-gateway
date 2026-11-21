"use client";

import { memo, useCallback, useEffect, useMemo, useRef, useState, useTransition } from "react";
import { usePathname, useRouter } from "next/navigation";
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { Plus, Search } from "lucide-react";
import { deleteKeysAction, setKeysDisabledAction, type KeyTarget } from "@/app/actions";
import { BudgetDialog, bulkFailures, MoveDialog, NewKeySheet, type KeysByFilter } from "@/components/access/key-dialogs";
import { KeyPanel } from "@/components/access/key-panel";
import { BudgetBar, SecretBanner } from "@/components/access/ui";
import { Notice, Tile } from "@/components/models/ui";
import { Pill } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { ListFooter } from "@/components/ui/list-footer";
import { Select } from "@/components/ui/select";
import {
  buildKeyRows,
  keyFiltersWords,
  keyListParams,
  keyListQuery,
  lastUsed,
  type KeyFilters,
  type KeyRow,
  type KeySort,
  type KeyWindow,
} from "@/lib/access-model";
import type { KeyPageData } from "@/lib/key-page-data";
import { logsHref } from "@/lib/log-links";
import type { ApiKey, KeyStats, NamedNetwork, Tenant } from "@/lib/obleth";
import { compact, formatNumber, money } from "@/lib/format";
import { cn, getJson } from "@/lib/utils";
import { StatusDot } from "@/components/ui/status";

const SORTS: { value: KeySort; label: string }[] = [
  { value: "last-used", label: "Sort: last used" },
  { value: "requests", label: "Sort: most requests" },
  { value: "spend", label: "Sort: most spend" },
  { value: "budget", label: "Sort: most of budget used" },
  { value: "name", label: "Sort: name" },
  { value: "newest", label: "Sort: newest" },
];

const COLS = "grid-cols-[28px_minmax(0,1.4fr)_minmax(0,1fr)_64px_minmax(0,1fr)_76px_72px_minmax(140px,1fr)_60px]";

function csv(rows: KeyRow[]): string {
  const esc = (v: string | number) => (/[",\n]/.test(String(v)) ? `"${String(v).replace(/"/g, '""')}"` : String(v));
  const head = ["name", "prefix", "kind", "tenant", "status", "last_used", "requests_30d", "tokens_30d", "spend_30d_usd", "budget", "weight", "max_in_flight", "id"];
  const lines = rows.map((r) => [
    r.key.name, r.key.key_prefix, r.key.kind, r.tenantName, r.key.disabled ? "off" : "on",
    r.lastUsedMs ? new Date(r.lastUsedMs).toISOString() : "", r.usage?.requests ?? 0, r.usage?.total_tokens ?? 0, (r.usage?.cost_usd ?? 0).toFixed(4),
    r.budget?.label ?? "", r.key.weight, r.key.max_in_flight ?? "", r.key.id,
  ].map(esc).join(","));
  return [head.join(","), ...lines].join("\n");
}

/** One key's row. Memoised: a keystroke or a checkbox elsewhere leaves it alone. */
const KeyListRow = memo(function KeyListRow({ row: r, now, picked, isOpen, onOpen, onToggle }: {
  row: KeyRow;
  now: number;
  picked: boolean;
  isOpen: boolean;
  onOpen: (id: string) => void;
  onToggle: (id: string) => void;
}) {
  const k = r.key;
  return (
    <div
      onClick={(e) => {
        if (e.target instanceof Element && e.target.closest("a, button, input, label")) return;
        onOpen(k.id);
      }}
      className={cn("grid min-h-[52px] cursor-pointer items-center gap-3.5 border-t border-border px-[18px] py-2 text-[13px] hover:bg-muted/30", COLS, k.disabled && "text-muted-foreground", isOpen && "bg-muted/40")}
    >
      <input type="checkbox" aria-label={`Select ${k.name}`} checked={picked} onChange={() => onToggle(k.id)} className="accent-foreground" />
      <button type="button" aria-label={`Open ${k.name}`} onClick={() => onOpen(k.id)} className="flex min-w-0 flex-col text-left">
        <span className="flex min-w-0 items-center gap-1.5">
          <span className={cn("truncate font-medium", !k.disabled && "text-foreground")}>{k.name || k.key_prefix}</span>
          {k.background && <Pill className="h-[18px] px-1.5 text-[10.5px]">Background</Pill>}
          {r.networks && <Pill className="h-[18px] px-1.5 text-[10.5px]">{r.networks}</Pill>}
        </span>
        <span className="truncate font-mono text-[11.5px] text-muted-foreground">{k.kind === "identity" ? `identity · ${k.identity_subject ?? ""}` : `${k.key_prefix}…`}</span>
      </button>
      <span className="truncate">{r.tenantName}</span>
      <span className="inline-flex items-center gap-2"><StatusDot glyph={k.disabled ? "off" : "on"} />{k.disabled ? "Off" : "On"}</span>
      <span className="flex min-w-0 flex-col">
        <span className="font-mono text-[12px]">{lastUsed(r.lastUsedMs, now)}</span>
        {r.usage?.last_model && <span className="truncate text-[11.5px] text-muted-foreground">{r.usage.last_model}</span>}
      </span>
      {r.usage?.requests ? (
        <a href={logsHref({ key: k.id, window: "30d" })} title="See these requests" className="text-right font-mono text-[12px] underline decoration-muted-foreground/40 underline-offset-[3px] hover:decoration-foreground">{compact(r.usage.requests)}</a>
      ) : (
        <span className="text-right font-mono text-[12px] text-muted-foreground">—</span>
      )}
      <span className="text-right font-mono text-[12px]">{r.usage?.cost_usd ? money(r.usage.cost_usd) : "—"}</span>
      <span className="flex min-w-0 flex-col gap-1">
        <span className="truncate text-[12px]">{r.budget ? r.budget.label : "No budget"}</span>
        <BudgetBar share={r.budget ? r.budget.share : null} />
      </span>
      <span className="text-right font-mono text-[12px]">{k.weight}</span>
    </div>
  );
});

/** How long the search box waits for typing to pause before it asks the gateway. */
const SEARCH_PAUSE_MS = 250;

/** The Keys list's first page, as the server rendered it, and what it was for. */
export interface KeysFirstPage {
  filters: KeyFilters;
  sort: KeySort;
  page: KeyPageData;
}

export function KeysList({
  tenants,
  first,
  usageWindow,
  stats,
  opened = null,
  initial,
  named = [],
}: {
  tenants: Tenant[];
  /** Settings › Networks, offered by name in a key's allowed networks. */
  named?: NamedNetwork[];
  first: KeysFirstPage;
  /** The use window every page reads, fixed for this visit so pages hold still. */
  usageWindow: KeyWindow;
  /** Every key's counts for the tiles; `null` when they could not be read. */
  stats: KeyStats | null;
  /** The key the address opened, when it is not on the first page. */
  opened?: KeyPageData | null;
  initial: { tenant?: string; key?: string; newFor?: string };
}) {
  const router = useRouter();
  const pathname = usePathname();
  const { confirm, confirmElement } = useConfirm();
  const [pending, start] = useTransition();
  const [filters, setFilters] = useState<KeyFilters>(first.filters);
  const [applied, setApplied] = useState<KeyFilters>(first.filters);
  const [sort, setSort] = useState<KeySort>(first.sort);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [allMatching, setAllMatching] = useState<KeysByFilter | null>(null);
  const [counting, setCounting] = useState(false);
  const [open, setOpen] = useState<string | null>(initial.key ?? null);
  const [adding, setAdding] = useState(initial.newFor !== undefined);
  const [moving, setMoving] = useState<{ keys: ApiKey[]; byFilter?: KeysByFilter } | null>(null);
  const [budgeting, setBudgeting] = useState(false);
  const [secret, setSecret] = useState<{ secret: string; name: string; detail: string } | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [now] = useState(() => usageWindow.untilMs);

  // Filters apply at once; the search waits for typing to pause.
  useEffect(() => {
    if (filters.query === applied.query) {
      if (filters !== applied) setApplied(filters);
      return;
    }
    const t = setTimeout(() => setApplied(filters), SEARCH_PAUSE_MS);
    return () => clearTimeout(t);
  }, [filters, applied]);

  // The gateway filters and sorts every key; a new search, filter or sort
  // starts again from the first page.
  const query = useMemo(() => keyListQuery(applied, sort, usageWindow), [applied, sort, usageWindow]);
  const params = useMemo(() => keyListParams(query).toString(), [query]);
  const firstParams = useMemo(() => keyListParams(keyListQuery(first.filters, first.sort, usageWindow)).toString(), [first, usageWindow]);
  const live = useInfiniteQuery({
    queryKey: ["keys", params],
    queryFn: ({ pageParam }) => getJson<KeyPageData>(`/api/live/keys?${params}${pageParam ? `&cursor=${encodeURIComponent(pageParam)}` : ""}`),
    initialPageParam: "",
    getNextPageParam: (last) => last.next ?? undefined,
    initialData: params === firstParams ? { pages: [first.page], pageParams: [""] } : undefined,
    staleTime: 30_000,
  });
  const pages = useMemo(() => live.data?.pages ?? [], [live.data]);
  const rows = useMemo(
    () => buildKeyRows(pages.flatMap((p) => p.keys), tenants, pages.flatMap((p) => p.usage), pages.flatMap((p) => p.budgets), named),
    [pages, tenants, named],
  );
  const openedRows = useMemo(() => (opened ? buildKeyRows(opened.keys, tenants, opened.usage, opened.budgets, named) : []), [opened, tenants, named]);
  const patch = (p: Partial<KeyFilters>) => setFilters((f) => ({ ...f, ...p }));
  const openRow = rows.find((r) => r.key.id === open) ?? openedRows.find((r) => r.key.id === open) ?? null;
  const chosen = rows.filter((r) => selected.has(r.key.id));
  const loading = live.isFetching && !live.isFetchingNextPage;

  // A selection belongs to the list it was made in.
  const lastParams = useRef(params);
  useEffect(() => {
    if (lastParams.current === params) return;
    lastParams.current = params;
    setSelected(new Set());
    setAllMatching(null);
  }, [params]);

  // What a key's weight means against the others in its tenant.
  const siblings = useQuery({
    queryKey: ["key-weights", openRow?.key.tenant_id],
    queryFn: () => getJson<Pick<ApiKey, "id" | "weight" | "disabled">[]>(`/api/live/keys/weights?tenant_id=${encodeURIComponent(openRow!.key.tenant_id)}`),
    enabled: !!openRow,
  });

  // The address follows the open key and the tenant filter, so a link or a reload lands in the same place.
  useEffect(() => {
    const p = new URLSearchParams();
    if (filters.tenantId) p.set("tenant", filters.tenantId);
    if (open) p.set("key", open);
    const q = p.toString();
    window.history.replaceState(null, "", q ? `${pathname}?${q}` : pathname);
  }, [filters.tenantId, open, pathname]);

  const refresh = useCallback(() => {
    void live.refetch();
    router.refresh();
  }, [live, router]);

  const total = stats?.keys ?? null;
  const busiest = stats?.busiest;
  const tenantName = tenants.find((t) => t.id === applied.tenantId)?.name;

  // "Select all" picks the rows shown; when more match, the bar offers every
  // match, counted by the gateway.
  const allShown = rows.length > 0 && rows.every((r) => selected.has(r.key.id));
  const toggleAll = () => {
    setAllMatching(null);
    setSelected(allShown || allMatching ? new Set() : new Set(rows.map((r) => r.key.id)));
  };
  const toggle = useCallback((id: string) => {
    setAllMatching(null);
    setSelected((s) => { const n = new Set(s); if (n.has(id)) n.delete(id); else n.add(id); return n; });
  }, []);
  const clearSelection = () => { setSelected(new Set()); setAllMatching(null); };

  /** How many keys the filters match now, from the gateway. */
  const countMatching = () => getJson<{ keys: number }>(`/api/live/keys/count?${params}`).then((c) => c.keys);

  function selectAllMatching() {
    setCounting(true);
    countMatching()
      .then((count) => setAllMatching({ filter: query, count }))
      .catch((e) => setNotice(e instanceof Error ? e.message : "Could not count the matching keys."))
      .finally(() => setCounting(false));
  }

  /**
   * What a bulk action acts on, and how to say it: the picked rows, or every
   * match counted again just before the confirm, saying how many are not on
   * the page.
   */
  async function bulkTarget(): Promise<{ target: KeyTarget; count: number; words: string } | null> {
    if (!allMatching) {
      return { target: [...selected], count: selected.size, words: `${chosen.slice(0, 5).map((r) => r.key.name).join(", ")}${chosen.length > 5 ? ` and ${chosen.length - 5} more` : ""}.` };
    }
    const count = await countMatching().catch(() => null);
    if (count == null) {
      setNotice("Could not count the matching keys. Nothing was changed.");
      return null;
    }
    const hidden = Math.max(0, count - rows.length);
    return {
      target: { filter: allMatching.filter, expected: count },
      count,
      words: `Every key ${keyFiltersWords(applied, tenantName)}${hidden ? `: ${formatNumber(hidden)} of them are not loaded on this page` : ""}.`,
    };
  }

  const plural = (n: number) => `${formatNumber(n)} key${n === 1 ? "" : "s"}`;

  function bulkDisabled(disabled: boolean) {
    start(async () => {
      const t = await bulkTarget();
      if (!t) return;
      if (disabled || allMatching) {
        const ok = await confirm({
          title: `Turn ${disabled ? "off" : "on"} ${plural(t.count)}?`,
          description: `${allMatching ? `${t.words} ` : ""}${disabled ? "Requests with them are refused straight away. You can turn them back on." : "Requests with them are accepted again."}`,
          confirmLabel: `Turn ${disabled ? "off" : "on"}`,
        });
        if (!ok) return;
      }
      const res = await setKeysDisabledAction(t.target, disabled);
      setNotice(`Turned ${disabled ? "off" : "on"} ${plural(res.done)}.${bulkFailures(res)}`);
      refresh();
    });
  }

  function bulkDelete() {
    start(async () => {
      const t = await bulkTarget();
      if (!t) return;
      const ok = await confirm({
        title: `Delete ${plural(t.count)}?`,
        description: `${t.words} Requests with them fail at once. This cannot be undone.`,
        confirmLabel: "Delete",
      });
      if (!ok) return;
      const res = await deleteKeysAction(t.target);
      setNotice(`Deleted ${plural(res.done)}.${bulkFailures(res)}`);
      clearSelection();
      refresh();
    });
  }

  function exportChosen() {
    const blob = new Blob([csv(chosen.length ? chosen : rows)], { type: "text/csv" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = `keys_${new Date().toISOString().slice(0, 10)}.csv`;
    a.click();
    URL.revokeObjectURL(a.href);
  }

  const line = stats
    ? [`${plural(stats.keys)} across ${stats.tenants} tenant${stats.tenants === 1 ? "" : "s"}`, `${stats.used_recent} used today`, `${stats.disabled} off`, stats.identity ? `${stats.identity} identity key${stats.identity === 1 ? "" : "s"}` : null].filter(Boolean).join(" · ")
    : "Counts are not available right now";
  const tenantOptions = [{ value: "", label: "Any tenant" }, ...tenants.map((t) => ({ value: t.id, label: t.name }))];
  const selectedCount = allMatching ? allMatching.count : selected.size;

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-[18px]">
      {confirmElement}
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <h1 className="text-[26px] font-semibold tracking-tight">API keys</h1>
          <p className="text-[13px] text-secondary-foreground">{line}</p>
        </div>
        <Button type="button" size="sm" className="h-9" onClick={() => setAdding(true)}><Plus className="h-4 w-4" />New key</Button>
      </div>

      {secret && (
        <SecretBanner
          title={<>{secret.name} is ready. Copy its secret now: it is shown only this once.</>}
          secret={secret.secret}
          detail={secret.detail}
          onDone={() => setSecret(null)}
        />
      )}
      {notice && <Notice onDismiss={() => setNotice(null)}>{notice}</Notice>}

      <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
        <Tile label="Keys" value={total ?? "—"} detail={stats ? `${stats.keys - stats.disabled} on · ${stats.disabled} off` : null} />
        <Tile label="Used · 24h" value={stats?.used_recent ?? "—"} detail={busiest && stats?.requests ? `${busiest.name} sent ${Math.round((busiest.requests / stats.requests) * 100)}% of 30-day requests` : null} />
        <Tile
          label="Near a budget"
          value={stats?.near_budget ?? "—"}
          emphasis={(stats?.near_budget ?? 0) > 0}
          onClick={stats?.near_budget ? () => patch({ budget: filters.budget === "near" ? "all" : "near" }) : undefined}
          pressed={filters.budget === "near"}
          detail={stats?.near_budget ? `${stats.near_budget_keys.map((k) => `${k.name} ${Math.round(k.share * 100)}%`).join(" · ")}${stats.near_budget > stats.near_budget_keys.length ? " · …" : ""}` : "None at 80% or more"}
        />
        <Tile
          label="Not used in 30 days"
          value={stats?.unused ?? "—"}
          onClick={stats?.unused ? () => patch({ unused: !filters.unused }) : undefined}
          pressed={filters.unused}
          detail={stats?.unused ? `still on: ${stats.unused_on} · ${filters.unused ? "showing them" : "show them"}` : null}
        />
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <label className="flex h-9 min-w-[240px] max-w-[380px] flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground">
          <Search className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden />
          <input value={filters.query} onChange={(e) => patch({ query: e.target.value })} aria-label="Search keys" placeholder="Name, prefix, tenant or identity" className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground" />
        </label>
        <Select aria-label="Tenant" value={filters.tenantId} onValueChange={(v) => patch({ tenantId: v })} searchPlaceholder="Find a tenant" className={cn("h-9 w-auto min-w-[9rem] text-[12.5px]", filters.tenantId && "border-foreground")} options={tenantOptions} />
        <Select aria-label="Status" value={filters.status} onValueChange={(v) => patch({ status: v as KeyFilters["status"] })} className={cn("h-9 w-auto min-w-[7rem] text-[12.5px]", filters.status !== "all" && "border-foreground")} options={[{ value: "all", label: "Any status" }, { value: "on", label: "On" }, { value: "off", label: "Off" }]} />
        {(stats?.identity ?? 0) > 0 && <Select aria-label="Kind" value={filters.kind} onValueChange={(v) => patch({ kind: v as KeyFilters["kind"] })} className={cn("h-9 w-auto min-w-[8rem] text-[12.5px]", filters.kind !== "all" && "border-foreground")} options={[{ value: "all", label: "Secret or identity" }, { value: "secret", label: "Secret keys" }, { value: "identity", label: "Identity keys" }]} />}
        <Select aria-label="Budget" value={filters.budget} onValueChange={(v) => patch({ budget: v as KeyFilters["budget"] })} className={cn("h-9 w-auto min-w-[8rem] text-[12.5px]", filters.budget !== "all" && "border-foreground")} options={[{ value: "all", label: "Any budget" }, { value: "capped", label: "Has a budget" }, { value: "near", label: "Near its budget" }, { value: "none", label: "No budget" }]} />
        <button type="button" aria-pressed={filters.unused} onClick={() => patch({ unused: !filters.unused })} className={cn("inline-flex h-9 items-center rounded-lg border px-3 text-[12.5px]", filters.unused ? "border-foreground text-foreground" : "border-border text-secondary-foreground hover:text-foreground")}>Unused 30 days</button>
        <Select aria-label="Sort" value={sort} onValueChange={(v) => setSort(v as KeySort)} className="h-9 w-auto min-w-[9rem] text-[12.5px]" options={SORTS} />
      </div>

      {selectedCount > 0 && (
        <div role="region" aria-label="Selected keys" className="sticky top-2 z-20 flex flex-wrap items-center justify-between gap-3 rounded-xl border border-muted-foreground/60 bg-card px-4 py-2.5 shadow-xl">
          <span className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[13px]">
            {allMatching ? (
              <span>All <b className="font-semibold">{formatNumber(allMatching.count)}</b> keys {keyFiltersWords(applied, tenantName) === "every key" ? "" : `${keyFiltersWords(applied, tenantName)} `}selected</span>
            ) : (
              <span><b className="font-semibold">{selected.size}</b> selected</span>
            )}
            {!allMatching && allShown && live.hasNextPage && (
              <button type="button" disabled={counting} onClick={selectAllMatching} className="underline decoration-muted-foreground/50 underline-offset-[3px] hover:decoration-foreground">
                {counting ? "Counting…" : "Select every matching key"}
              </button>
            )}
          </span>
          <div className="flex flex-wrap gap-2">
            <Button type="button" size="sm" variant="outline" disabled={pending} onClick={() => bulkDisabled(true)}>Turn off</Button>
            <Button type="button" size="sm" variant="outline" disabled={pending} onClick={() => bulkDisabled(false)}>Turn on</Button>
            <Button type="button" size="sm" variant="outline" disabled={pending} onClick={() => setBudgeting(true)}>Set a budget…</Button>
            <Button type="button" size="sm" variant="outline" disabled={pending} onClick={() => setMoving(allMatching ? { keys: [], byFilter: allMatching } : { keys: chosen.map((r) => r.key) })}>Move…</Button>
            <Button type="button" size="sm" variant="outline" onClick={exportChosen} disabled={!!allMatching} title={allMatching ? "Export takes the rows on the page: pick them, or load more first" : undefined}>Export</Button>
            <Button type="button" size="sm" variant="outline" disabled={pending} onClick={bulkDelete}>Delete…</Button>
            <Button type="button" size="sm" variant="ghost" onClick={clearSelection}>Clear</Button>
          </div>
        </div>
      )}

      <section aria-label="Keys" aria-busy={loading} className="overflow-hidden rounded-xl border border-border bg-card">
        <div className="overflow-x-auto">
          <div className="min-w-[1040px]">
            <div className={cn("grid items-center gap-3.5 px-[18px] py-2.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", COLS)}>
              <input type="checkbox" aria-label={`Select the ${rows.length} keys shown`} checked={allShown || !!allMatching} onChange={toggleAll} className="accent-foreground" />
              <span>Key</span><span>Tenant</span><span>Status</span><span>Last used</span><span className="text-right">Req · 30d</span><span className="text-right">Spend</span><span>Budget this period</span><span className="text-right">Weight</span>
            </div>
            {live.isError && <p role="alert" className="border-t border-border px-[18px] py-8 text-center text-[13px] text-muted-foreground">{live.error instanceof Error ? live.error.message : "The keys could not be read."}</p>}
            {!live.isError && rows.length === 0 && <p className="border-t border-border px-[18px] py-8 text-center text-[13px] text-muted-foreground">{loading ? "Loading…" : total ? "No key matches these filters." : "No keys yet."}</p>}
            {rows.map((r) => (
              <KeyListRow key={r.key.id} row={r} now={now} picked={allMatching ? true : selected.has(r.key.id)} isOpen={open === r.key.id} onOpen={setOpen} onToggle={toggle} />
            ))}
          </div>
        </div>
        <ListFooter shown={rows.length} noun="keys" more={live.hasNextPage} loading={live.isFetchingNextPage} onMore={() => live.fetchNextPage()} action="Load more" />
        <div className="flex flex-wrap justify-between gap-2 border-t border-border px-[18px] py-3 text-[12.5px] text-muted-foreground">
          <span>{plural(rows.length)}{live.hasNextPage ? " so far" : ""} · click one to see and change it</span>
          <span>Use is the last 30 days; budgets count their own period</span>
        </div>
      </section>

      <KeyPanel
        key={open ?? "none"}
        row={openRow}
        tenants={tenants}
        siblings={openRow ? (siblings.data ?? []).filter((k) => k.id !== openRow.key.id) : []}
        onClose={() => setOpen(null)}
        onMove={(k) => setMoving({ keys: [k] })}
        onReplaced={(created) => { setSecret(created); setOpen(null); }}
        onNotice={setNotice}
        onChanged={refresh}
        named={named}
      />
      {adding && (
        <NewKeySheet
          open
          onClose={() => setAdding(false)}
          tenants={tenants}
          tenantId={initial.newFor ?? filters.tenantId}
          named={named}
          onCreated={(c) => { setSecret({ secret: c.secret, name: c.name, detail: `for ${c.tenantName} · ${c.detail}` }); setAdding(false); refresh(); }}
        />
      )}
      {moving && (
        <MoveDialog
          key={moving.byFilter ? "filter" : moving.keys.map((k) => k.id).join(",")}
          open
          onClose={() => setMoving(null)}
          keys={moving.keys}
          byFilter={moving.byFilter}
          tenants={tenants}
          onDone={(text) => { setNotice(text); clearSelection(); refresh(); }}
        />
      )}
      {budgeting && (
        <BudgetDialog open onClose={() => setBudgeting(false)} keys={chosen.map((r) => r.key)} byFilter={allMatching ?? undefined} onDone={(text) => { setNotice(text); refresh(); }} />
      )}
    </div>
  );
}

