"use client";

import { useCallback, useEffect, useMemo, useState, useTransition } from "react";
import { usePathname, useRouter } from "next/navigation";
import { Plus, Search } from "lucide-react";
import { deleteKeysAction, setKeysDisabledAction } from "@/app/actions";
import { BudgetDialog, MoveDialog, NewKeySheet } from "@/components/access/key-dialogs";
import { KeyPanel } from "@/components/access/key-panel";
import { BudgetBar, SecretBanner, StateDot } from "@/components/access/ui";
import { Notice, Tile } from "@/components/models/ui";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { Select } from "@/components/ui/select";
import {
  buildKeyRows,
  EMPTY_KEY_FILTERS,
  filterKeys,
  lastUsed,
  NEAR_BUDGET,
  sortKeys,
  type KeyFilters,
  type KeyRow,
  type KeySort,
} from "@/lib/access-model";
import { logsHref } from "@/lib/log-links";
import { money } from "@/lib/models-model";
import type { ApiKey, BudgetUsage, KeyUsageSummary, Tenant } from "@/lib/obleth";
import { compact } from "@/lib/overview-model";
import { cn } from "@/lib/utils";

const DAY_MS = 86_400_000;

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

export function KeysList({
  tenants,
  keys,
  usage,
  budgets,
  initial,
}: {
  tenants: Tenant[];
  keys: ApiKey[];
  /** The last 30 days, per key. */
  usage: KeyUsageSummary[];
  budgets: BudgetUsage[];
  initial: { tenant?: string; key?: string; newFor?: string };
}) {
  const router = useRouter();
  const pathname = usePathname();
  const { confirm, confirmElement } = useConfirm();
  const [pending, start] = useTransition();
  const [filters, setFilters] = useState<KeyFilters>({ ...EMPTY_KEY_FILTERS, tenantId: initial.tenant ?? "" });
  const [sort, setSort] = useState<KeySort>("last-used");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [open, setOpen] = useState<string | null>(initial.key ?? null);
  const [adding, setAdding] = useState(initial.newFor !== undefined);
  const [moving, setMoving] = useState<ApiKey[] | null>(null);
  const [budgeting, setBudgeting] = useState(false);
  const [secret, setSecret] = useState<{ secret: string; name: string; detail: string } | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [now] = useState(() => Date.now());

  const rows = useMemo(() => buildKeyRows(keys, tenants, usage, budgets), [keys, tenants, usage, budgets]);
  const shown = useMemo(() => sortKeys(filterKeys(rows, filters, now), sort), [rows, filters, sort, now]);
  const patch = (p: Partial<KeyFilters>) => setFilters((f) => ({ ...f, ...p }));
  const openRow = rows.find((r) => r.key.id === open) ?? null;
  const chosen = rows.filter((r) => selected.has(r.key.id));

  // The address follows the open key and the tenant filter, so a link or a reload lands in the same place.
  useEffect(() => {
    const p = new URLSearchParams();
    if (filters.tenantId) p.set("tenant", filters.tenantId);
    if (open) p.set("key", open);
    const q = p.toString();
    window.history.replaceState(null, "", q ? `${pathname}?${q}` : pathname);
  }, [filters.tenantId, open, pathname]);

  const refresh = useCallback(() => router.refresh(), [router]);

  const on = keys.filter((k) => !k.disabled).length;
  const identity = keys.filter((k) => k.kind === "identity").length;
  const usedToday = rows.filter((r) => r.lastUsedMs > now - DAY_MS).length;
  const total30 = rows.reduce((n, r) => n + (r.usage?.requests ?? 0), 0);
  const busiest = [...rows].sort((a, b) => (b.usage?.requests ?? 0) - (a.usage?.requests ?? 0))[0];
  const near = rows.filter((r) => (r.budget?.share ?? 0) >= NEAR_BUDGET);
  const idle = rows.filter((r) => r.lastUsedMs <= now - 30 * DAY_MS);
  const idleOn = idle.filter((r) => !r.key.disabled).length;
  const tenantCount = new Set(keys.map((k) => k.tenant_id)).size;

  const allShown = shown.length > 0 && shown.every((r) => selected.has(r.key.id));
  const toggleAll = () => setSelected(allShown ? new Set() : new Set(shown.map((r) => r.key.id)));
  const toggle = (id: string) => setSelected((s) => { const n = new Set(s); if (n.has(id)) n.delete(id); else n.add(id); return n; });

  function bulkDisabled(disabled: boolean) {
    start(async () => {
      if (disabled) {
        const ok = await confirm({ title: `Turn off ${chosen.length} key${chosen.length === 1 ? "" : "s"}?`, description: "Requests with them are refused straight away. You can turn them back on.", confirmLabel: "Turn off" });
        if (!ok) return;
      }
      const res = await setKeysDisabledAction([...selected], disabled);
      setNotice(`Turned ${disabled ? "off" : "on"} ${res.done} key${res.done === 1 ? "" : "s"}.${res.failed.length ? ` ${res.failed.length} failed: ${res.failed.map((f) => f.name).join(", ")}` : ""}`);
      refresh();
    });
  }

  function bulkDelete() {
    start(async () => {
      const ok = await confirm({
        title: `Delete ${chosen.length} key${chosen.length === 1 ? "" : "s"}?`,
        description: `${chosen.slice(0, 5).map((r) => r.key.name).join(", ")}${chosen.length > 5 ? ` and ${chosen.length - 5} more` : ""}. Requests with them fail at once. This cannot be undone.`,
        confirmLabel: "Delete",
      });
      if (!ok) return;
      const res = await deleteKeysAction([...selected]);
      setNotice(`Deleted ${res.deleted} key${res.deleted === 1 ? "" : "s"}.${res.failed ? ` ${res.failed} failed.` : ""}`);
      setSelected(new Set());
      refresh();
    });
  }

  function exportChosen() {
    const blob = new Blob([csv(chosen.length ? chosen : shown)], { type: "text/csv" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = `keys_${new Date().toISOString().slice(0, 10)}.csv`;
    a.click();
    URL.revokeObjectURL(a.href);
  }

  const line = [`${keys.length} key${keys.length === 1 ? "" : "s"} across ${tenantCount} tenant${tenantCount === 1 ? "" : "s"}`, `${usedToday} used today`, `${keys.length - on} off`, identity ? `${identity} identity key${identity === 1 ? "" : "s"}` : null].filter(Boolean).join(" · ");
  const tenantOptions = [{ value: "", label: "Any tenant" }, ...tenants.map((t) => ({ value: t.id, label: t.name }))];

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
        <Tile label="Keys" value={keys.length} detail={`${on} on · ${keys.length - on} off`} />
        <Tile label="Used · 24h" value={usedToday} detail={busiest?.usage?.requests && total30 ? `${busiest.key.name} sent ${Math.round((busiest.usage.requests / total30) * 100)}% of 30-day requests` : null} />
        <Tile
          label="Near a budget"
          value={near.length}
          emphasis={near.length > 0}
          onClick={near.length ? () => patch({ budget: filters.budget === "near" ? "all" : "near" }) : undefined}
          pressed={filters.budget === "near"}
          detail={near.length ? near.map((r) => `${r.key.name} ${Math.round((r.budget?.share ?? 0) * 100)}%`).join(" · ") : "None at 80% or more"}
        />
        <Tile
          label="Not used in 30 days"
          value={idle.length}
          onClick={idle.length ? () => patch({ unused: !filters.unused }) : undefined}
          pressed={filters.unused}
          detail={idle.length ? `still on: ${idleOn} · ${filters.unused ? "showing them" : "show them"}` : null}
        />
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <label className="flex h-9 min-w-[240px] max-w-[380px] flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground">
          <Search className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden />
          <input value={filters.query} onChange={(e) => patch({ query: e.target.value })} aria-label="Search keys" placeholder="Name, prefix, tenant or identity" className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground" />
        </label>
        <Select aria-label="Tenant" value={filters.tenantId} onValueChange={(v) => patch({ tenantId: v })} searchPlaceholder="Find a tenant" className={cn("h-9 w-auto min-w-[9rem] text-[12.5px]", filters.tenantId && "border-foreground")} options={tenantOptions} />
        <Select aria-label="Status" value={filters.status} onValueChange={(v) => patch({ status: v as KeyFilters["status"] })} className={cn("h-9 w-auto min-w-[7rem] text-[12.5px]", filters.status !== "all" && "border-foreground")} options={[{ value: "all", label: "Any status" }, { value: "on", label: "On" }, { value: "off", label: "Off" }]} />
        {identity > 0 && <Select aria-label="Kind" value={filters.kind} onValueChange={(v) => patch({ kind: v as KeyFilters["kind"] })} className={cn("h-9 w-auto min-w-[8rem] text-[12.5px]", filters.kind !== "all" && "border-foreground")} options={[{ value: "all", label: "Secret or identity" }, { value: "secret", label: "Secret keys" }, { value: "identity", label: "Identity keys" }]} />}
        <Select aria-label="Budget" value={filters.budget} onValueChange={(v) => patch({ budget: v as KeyFilters["budget"] })} className={cn("h-9 w-auto min-w-[8rem] text-[12.5px]", filters.budget !== "all" && "border-foreground")} options={[{ value: "all", label: "Any budget" }, { value: "capped", label: "Has a budget" }, { value: "near", label: "Near its budget" }, { value: "none", label: "No budget" }]} />
        <button type="button" aria-pressed={filters.unused} onClick={() => patch({ unused: !filters.unused })} className={cn("inline-flex h-9 items-center rounded-lg border px-3 text-[12.5px]", filters.unused ? "border-foreground text-foreground" : "border-border text-secondary-foreground hover:text-foreground")}>Unused 30 days</button>
        <Select aria-label="Sort" value={sort} onValueChange={(v) => setSort(v as KeySort)} className="h-9 w-auto min-w-[9rem] text-[12.5px]" options={SORTS} />
      </div>

      {selected.size > 0 && (
        <div role="region" aria-label="Selected keys" className="sticky top-2 z-20 flex flex-wrap items-center justify-between gap-3 rounded-xl border border-muted-foreground/60 bg-card px-4 py-2.5 shadow-xl">
          <span className="text-[13px]"><b className="font-semibold">{selected.size}</b> selected</span>
          <div className="flex flex-wrap gap-2">
            <Button type="button" size="sm" variant="outline" disabled={pending} onClick={() => bulkDisabled(true)}>Turn off</Button>
            <Button type="button" size="sm" variant="outline" disabled={pending} onClick={() => bulkDisabled(false)}>Turn on</Button>
            <Button type="button" size="sm" variant="outline" disabled={pending} onClick={() => setBudgeting(true)}>Set a budget…</Button>
            <Button type="button" size="sm" variant="outline" disabled={pending} onClick={() => setMoving(chosen.map((r) => r.key))}>Move…</Button>
            <Button type="button" size="sm" variant="outline" onClick={exportChosen}>Export</Button>
            <Button type="button" size="sm" variant="outline" disabled={pending} onClick={bulkDelete}>Delete…</Button>
            <Button type="button" size="sm" variant="ghost" onClick={() => setSelected(new Set())}>Clear</Button>
          </div>
        </div>
      )}

      <section aria-label="Keys" className="overflow-hidden rounded-xl border border-border bg-card">
        <div className="overflow-x-auto">
          <div className="min-w-[1040px]">
            <div className={cn("grid items-center gap-3.5 px-[18px] py-2.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", COLS)}>
              <input type="checkbox" aria-label="Select every key shown" checked={allShown} onChange={toggleAll} className="accent-foreground" />
              <span>Key</span><span>Tenant</span><span>Status</span><span>Last used</span><span className="text-right">Req · 30d</span><span className="text-right">Spend</span><span>Budget this period</span><span className="text-right">Weight</span>
            </div>
            {shown.length === 0 && <p className="border-t border-border px-[18px] py-8 text-center text-[13px] text-muted-foreground">{keys.length ? "No key matches these filters." : "No keys yet."}</p>}
            {shown.map((r) => {
              const k = r.key;
              return (
                <div
                  key={k.id}
                  onClick={(e) => {
                    if (e.target instanceof Element && e.target.closest("a, button, input, label")) return;
                    setOpen(k.id);
                  }}
                  className={cn("grid min-h-[52px] cursor-pointer items-center gap-3.5 border-t border-border px-[18px] py-2 text-[13px] hover:bg-muted/30", COLS, k.disabled && "text-muted-foreground", open === k.id && "bg-muted/40")}
                >
                  <input type="checkbox" aria-label={`Select ${k.name}`} checked={selected.has(k.id)} onChange={() => toggle(k.id)} className="accent-foreground" />
                  <button type="button" aria-label={`Open ${k.name}`} onClick={() => setOpen(k.id)} className="flex min-w-0 flex-col text-left">
                    <span className={cn("truncate font-medium", !k.disabled && "text-foreground")}>{k.name || k.key_prefix}</span>
                    <span className="truncate font-mono text-[11.5px] text-muted-foreground">{k.kind === "identity" ? `identity · ${k.identity_subject ?? ""}` : `${k.key_prefix}…`}</span>
                  </button>
                  <span className="truncate">{r.tenantName}</span>
                  <span className="inline-flex items-center gap-2"><StateDot state={k.disabled ? "off" : "on"} />{k.disabled ? "Off" : "On"}</span>
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
            })}
          </div>
        </div>
        <div className="flex flex-wrap justify-between gap-2 border-t border-border px-[18px] py-3 text-[12.5px] text-muted-foreground">
          <span>{shown.length === keys.length ? `${keys.length} keys` : `${shown.length} of ${keys.length} keys`} · click one to see and change it</span>
          <span>Use is the last 30 days; budgets count their own period</span>
        </div>
      </section>

      <KeyPanel
        key={open ?? "none"}
        row={openRow}
        tenants={tenants}
        siblings={openRow ? keys.filter((k) => k.tenant_id === openRow.key.tenant_id && k.id !== openRow.key.id) : []}
        onClose={() => setOpen(null)}
        onMove={(k) => setMoving([k])}
        onReplaced={(created) => { setSecret(created); setOpen(null); }}
        onNotice={setNotice}
        onChanged={refresh}
      />
      {adding && (
        <NewKeySheet
          open
          onClose={() => setAdding(false)}
          tenants={tenants}
          tenantId={initial.newFor ?? filters.tenantId}
          onCreated={(c) => { setSecret({ secret: c.secret, name: c.name, detail: `for ${c.tenantName} · ${c.detail}` }); setAdding(false); refresh(); }}
        />
      )}
      {moving && (
        <MoveDialog
          key={moving.map((k) => k.id).join(",")}
          open
          onClose={() => setMoving(null)}
          keys={moving}
          tenants={tenants}
          onDone={(text) => { setNotice(text); setSelected(new Set()); refresh(); }}
        />
      )}
      {budgeting && (
        <BudgetDialog open onClose={() => setBudgeting(false)} keys={chosen.map((r) => r.key)} onDone={(text) => { setNotice(text); refresh(); }} />
      )}
    </div>
  );
}
