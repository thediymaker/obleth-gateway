"use client";

import { memo, useCallback, useDeferredValue, useMemo, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Plus, Search } from "lucide-react";
import { BudgetBar, TENANT_STATUS_LABEL, tenantGlyph } from "@/components/access/ui";
import { AddTenantSheet } from "@/components/access/add-tenant";
import { useFairshareLive } from "@/components/fairshare/hooks";
import { Tile } from "@/components/models/ui";
import { Segmented } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { ListFooter, useRowLimit } from "@/components/ui/list-footer";
import { Select } from "@/components/ui/select";
import {
  accessLabel,
  tenantHref,
  buildTenantRows,
  filterTenants,
  limitsLabel,
  NEAR_BUDGET,
  sortTenants,
  type KeyCounts,
  type TenantHas,
  type TenantRow,
  type TenantSort,
  type TenantStatusFilter,
} from "@/lib/access-model";
import type { AdminUser } from "@/lib/auth/users";
import { logsHref } from "@/lib/log-links";
import type { BudgetUsage, Tenant, UsageAgg } from "@/lib/obleth";
import { compact, money } from "@/lib/format";
import { cn } from "@/lib/utils";
import { StatusDot } from "@/components/ui/status";

const HAS_OPTIONS: { value: TenantHas; label: string }[] = [
  { value: "any", label: "Has anything" },
  { value: "budget", label: "Has a budget" },
  { value: "hours", label: "Has access hours" },
  { value: "allowlist", label: "Limited to some models" },
  { value: "guardrails", label: "Has guardrails" },
  { value: "near-limit", label: "Near a limit" },
];

const SORT_OPTIONS: { value: TenantSort; label: string }[] = [
  { value: "requests", label: "Sort: most requests" },
  { value: "budget", label: "Sort: most of budget used" },
  { value: "name", label: "Sort: name" },
  { value: "newest", label: "Sort: newest" },
];

const COLS = "grid-cols-[minmax(0,1.5fr)_100px_120px_56px_56px_84px_minmax(150px,1fr)_minmax(0,1fr)_minmax(0,1fr)]";

function nearReason(r: TenantRow): string | null {
  if ((r.budget?.share ?? 0) >= NEAR_BUDGET) return `${r.tenant.name} ${Math.round((r.budget?.share ?? 0) * 100)}% of budget`;
  if (r.waiting > 0) return `${r.tenant.name} has ${r.waiting} waiting`;
  return null;
}

/** One tenant's row. Memoised: typing in the search leaves it alone. */
const TenantListRow = memo(function TenantListRow({ row: r, monthSpend, onOpen }: {
  row: TenantRow;
  /** This calendar month's spend. */
  monthSpend: number;
  onOpen: (href: string) => void;
}) {
  const t = r.tenant;
  const href = tenantHref(t);
  return (
    <div
      onClick={(e) => {
        if (e.target instanceof Element && e.target.closest("a, button")) return;
        onOpen(href);
      }}
      className={cn("grid min-h-[52px] cursor-pointer items-center gap-3.5 border-t border-border px-[18px] py-2 text-[13px] hover:bg-muted/30", COLS, t.status !== "active" && "text-muted-foreground")}
    >
      <span className="flex min-w-0 flex-col">
        <Link href={href} className="truncate font-medium text-foreground hover:underline">{t.name}</Link>
        <span className="truncate text-[11.5px] text-muted-foreground">{t.organization || t.description || t.contact_email || " "}</span>
      </span>
      <span className="inline-flex items-center gap-2"><StatusDot glyph={tenantGlyph(t.status)} />{TENANT_STATUS_LABEL[t.status] ?? t.status}</span>
      <span className="truncate font-mono text-[12px]">{t.fairshare_group} · {t.weight}</span>
      <Link href={`/keys?tenant=${t.id}`} className="text-right font-mono text-[12px] hover:underline" title={r.keysOff ? `${r.keysOff} off` : undefined}>{r.keys}</Link>
      <span className="text-right font-mono text-[12px]">{r.users}</span>
      {r.requests24h ? (
        <Link href={logsHref({ team: t.id, window: "24h" })} title="See these requests" className="text-right font-mono text-[12px] underline decoration-muted-foreground/40 underline-offset-[3px] hover:decoration-foreground">{compact(r.requests24h)}</Link>
      ) : (
        <span className="text-right font-mono text-[12px] text-muted-foreground">0</span>
      )}
      <span className="flex min-w-0 flex-col gap-1">
        <span className="truncate font-mono text-[12px]">{r.budget ? r.budget.label : `${money(monthSpend)} · no budget`}</span>
        <BudgetBar share={r.budget ? r.budget.share : null} />
      </span>
      <span className="truncate text-[12px] text-secondary-foreground" title={limitsLabel(t)}>
        {limitsLabel(t)}
        {r.waiting > 0 && <span className="text-foreground"> · {r.waiting} waiting</span>}
      </span>
      <span className="truncate text-[12.5px] text-secondary-foreground" title={accessLabel(t)}>{accessLabel(t)}</span>
    </div>
  );
});

export function TenantsList({
  tenants,
  keyCounts,
  users,
  usage24h,
  budgets,
  monthSpend,
}: {
  tenants: Tenant[];
  /** Keys per tenant, counted on the server. */
  keyCounts: KeyCounts;
  users: AdminUser[];
  usage24h: UsageAgg[];
  budgets: BudgetUsage[];
  /** Spend this calendar month, by tenant id. */
  monthSpend: Record<string, number>;
}) {
  const router = useRouter();
  const fairshare = useFairshareLive();
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<TenantStatusFilter>("all");
  const [group, setGroup] = useState("");
  const [has, setHas] = useState<TenantHas>("any");
  const [sort, setSort] = useState<TenantSort>("requests");
  const [adding, setAdding] = useState(false);

  const rows = useMemo(() => buildTenantRows(tenants, keyCounts, users, usage24h, budgets, fairshare.data), [tenants, keyCounts, users, usage24h, budgets, fairshare.data]);
  // The search box stays responsive; the rows catch up when typing pauses.
  const applied = useDeferredValue(query);
  const shown = useMemo(() => sortTenants(filterTenants(rows, { query: applied, status, group, has }), sort), [rows, applied, status, group, has, sort]);
  const { visible, more, showMore } = useRowLimit(shown);
  const open = useCallback((href: string) => router.push(href), [router]);
  const groups = [...new Set(tenants.map((t) => t.fairshare_group))].sort();
  const count = (s: string) => tenants.filter((t) => t.status === s).length;

  const totalRequests = rows.reduce((n, r) => n + r.requests24h, 0);
  const busiest = [...rows].sort((a, b) => b.requests24h - a.requests24h)[0];
  const spend = Object.values(monthSpend).reduce((n, v) => n + v, 0);
  const dayOfMonth = new Date().getDate();
  const near = rows.filter((r) => nearReason(r));
  const nearBudget = rows.filter((r) => (r.budget?.share ?? 0) >= NEAR_BUDGET).length;
  const closed = rows.filter((r) => r.access === "closed" || r.access === "ended" || r.access === "not-yet").length;
  const waiting = rows.filter((r) => r.waiting > 0).length;
  const keyTotals = Object.values(keyCounts).reduce((n, c) => ({ keys: n.keys + c.keys, off: n.off + c.off }), { keys: 0, off: 0 });
  const pendingUsers = users.filter((u) => u.status !== "active").length;
  const line = [
    `${count("active")} active`,
    count("suspended") ? `${count("suspended")} suspended` : null,
    nearBudget ? `${nearBudget} near ${nearBudget === 1 ? "its" : "their"} budget` : null,
    closed ? `${closed} outside ${closed === 1 ? "its" : "their"} access hours` : null,
    waiting ? `${waiting} waiting for slots now` : null,
  ].filter(Boolean).join(" · ");
  const policies = {
    guardrails: tenants.filter((t) => t.guardrails_policy).length,
    compression: tenants.filter((t) => t.compression_policy).length,
    tracing: tenants.filter((t) => t.tracing_enabled).length,
  };

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-[18px]">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <h1 className="text-[26px] font-semibold tracking-tight">Tenants</h1>
          <p className="text-[13px] text-secondary-foreground">{line || "No tenants yet"}</p>
        </div>
        <Button type="button" size="sm" className="h-9" onClick={() => setAdding(true)}><Plus className="h-4 w-4" />Add tenant</Button>
      </div>

      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-5">
        <Tile label="Tenants" value={tenants.length} detail={`${count("active")} active · ${count("suspended")} suspended${count("archived") ? ` · ${count("archived")} archived` : ""}`} />
        <Tile
          label="Requests · 24h"
          value={compact(totalRequests)}
          href={totalRequests ? logsHref({ window: "24h" }) : undefined}
          detail={busiest && totalRequests ? `${busiest.tenant.name} ${Math.round((busiest.requests24h / totalRequests) * 100)}%` : null}
        />
        <Tile label="Spend · this month" value={money(spend)} detail={spend ? `${money(spend / dayOfMonth)} a day` : null} />
        <Tile
          label="Near a limit"
          value={near.length}
          emphasis={near.length > 0}
          onClick={near.length ? () => setHas(has === "near-limit" ? "any" : "near-limit") : undefined}
          pressed={has === "near-limit"}
          detail={near.length ? near.map(nearReason).join(" · ") : "None near a budget or waiting"}
        />
        <Tile label="Keys · users" value={`${keyTotals.keys} · ${users.length}`} detail={`${keyTotals.off} keys off · ${pendingUsers} users pending`} />
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <label className="flex h-9 min-w-[240px] max-w-[420px] flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground">
          <Search className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden />
          <input value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search tenants" placeholder="Search name, organization or contact" className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground" />
        </label>
        <Segmented
          label="Status"
          value={status}
          onChange={setStatus}
          options={[
            { value: "all", label: `All ${tenants.length}` },
            { value: "active", label: `Active ${count("active")}` },
            { value: "suspended", label: `Suspended ${count("suspended")}` },
            { value: "archived", label: `Archived ${count("archived")}` },
          ]}
        />
        {groups.length > 1 && (
          <Select aria-label="Group" value={group} onValueChange={setGroup} className={cn("h-9 w-auto min-w-[8rem] text-[12.5px]", group && "border-foreground")} options={[{ value: "", label: "Any group" }, ...groups.map((g) => ({ value: g, label: `Group ${g}` }))]} />
        )}
        <Select aria-label="Has" value={has} onValueChange={(v) => setHas(v as TenantHas)} className={cn("h-9 w-auto min-w-[9rem] text-[12.5px]", has !== "any" && "border-foreground")} options={HAS_OPTIONS} />
        <Select aria-label="Sort" value={sort} onValueChange={(v) => setSort(v as TenantSort)} className="h-9 w-auto min-w-[10rem] text-[12.5px]" options={SORT_OPTIONS} />
      </div>

      <section aria-label="Tenants" className="overflow-hidden rounded-xl border border-border bg-card">
        <div className="overflow-x-auto">
          <div className="min-w-[1080px]">
            <div className={cn("grid items-center gap-3.5 px-[18px] py-2.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", COLS)}>
              <span>Tenant</span><span>Status</span><span>Group · weight</span><span className="text-right">Keys</span><span className="text-right">Users</span><span className="text-right">Req · 24h</span><span>Spend vs budget</span><span>Limits</span><span>Access</span>
            </div>
            {shown.length === 0 && <p className="border-t border-border px-[18px] py-8 text-center text-[13px] text-muted-foreground">{tenants.length ? "No tenant matches these filters." : "No tenants yet. Add one to hand out keys."}</p>}
            {visible.map((r) => <TenantListRow key={r.tenant.id} row={r} monthSpend={monthSpend[r.tenant.id] ?? 0} onOpen={open} />)}
          </div>
        </div>
        <ListFooter shown={visible.length} total={shown.length} noun="tenants" more={more} onMore={showMore} action="Show more" />
        <div className="flex flex-wrap justify-between gap-2 border-t border-border px-[18px] py-3 text-[12.5px] text-muted-foreground">
          <span>{shown.length === tenants.length ? `${tenants.length} tenants` : `${shown.length} of ${tenants.length} tenants`} · click one for its page</span>
          <span>Policies set: guardrails {policies.guardrails} · compression {policies.compression} · tracing {policies.tracing}</span>
        </div>
      </section>

      <AddTenantSheet open={adding} onClose={() => setAdding(false)} tenants={tenants} />
    </div>
  );
}
