"use client";

import { useMemo, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Plus, Search } from "lucide-react";
import { BudgetBar, StateDot, TENANT_STATUS_LABEL, tenantState } from "@/components/access/ui";
import { CreateTenant } from "@/components/create-tenant";
import { useFairshareLive } from "@/components/fairshare/hooks";
import { Tile } from "@/components/models/ui";
import { Segmented } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Select } from "@/components/ui/select";
import {
  accessLabel,
  tenantHref,
  buildTenantRows,
  filterTenants,
  limitsLabel,
  NEAR_BUDGET,
  sortTenants,
  type TenantHas,
  type TenantRow,
  type TenantSort,
  type TenantStatusFilter,
} from "@/lib/access-model";
import type { AdminUser } from "@/lib/auth/users";
import { logsHref } from "@/lib/log-links";
import { money } from "@/lib/models-model";
import type { ApiKey, BudgetUsage, Tenant, UsageAgg } from "@/lib/obleth";
import { compact } from "@/lib/overview-model";
import { cn } from "@/lib/utils";

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

export function TenantsList({
  tenants,
  keys,
  users,
  usage24h,
  budgets,
  monthSpend,
  models,
}: {
  tenants: Tenant[];
  keys: ApiKey[];
  users: AdminUser[];
  usage24h: UsageAgg[];
  budgets: BudgetUsage[];
  /** Spend this calendar month, by tenant id. */
  monthSpend: Record<string, number>;
  models: string[];
}) {
  const router = useRouter();
  const fairshare = useFairshareLive();
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState<TenantStatusFilter>("all");
  const [group, setGroup] = useState("");
  const [has, setHas] = useState<TenantHas>("any");
  const [sort, setSort] = useState<TenantSort>("requests");
  const [adding, setAdding] = useState(false);

  const rows = useMemo(() => buildTenantRows(tenants, keys, users, usage24h, budgets, fairshare.data), [tenants, keys, users, usage24h, budgets, fairshare.data]);
  const shown = useMemo(() => sortTenants(filterTenants(rows, { query, status, group, has }), sort), [rows, query, status, group, has, sort]);
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
  const keysOff = keys.filter((k) => k.disabled).length;
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
        <Tile label="Keys · users" value={`${keys.length} · ${users.length}`} detail={`${keysOff} keys off · ${pendingUsers} users pending`} />
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
            {shown.map((r) => {
              const t = r.tenant;
              const href = tenantHref(t);
              return (
                <div
                  key={t.id}
                  onClick={(e) => {
                    if (e.target instanceof Element && e.target.closest("a, button")) return;
                    router.push(href);
                  }}
                  className={cn("grid min-h-[52px] cursor-pointer items-center gap-3.5 border-t border-border px-[18px] py-2 text-[13px] hover:bg-muted/30", COLS, t.status !== "active" && "text-muted-foreground")}
                >
                  <span className="flex min-w-0 flex-col">
                    <Link href={href} className="truncate font-medium text-foreground hover:underline">{t.name}</Link>
                    <span className="truncate text-[11.5px] text-muted-foreground">{t.organization || t.description || t.contact_email || " "}</span>
                  </span>
                  <span className="inline-flex items-center gap-2"><StateDot state={tenantState(t.status)} />{TENANT_STATUS_LABEL[t.status] ?? t.status}</span>
                  <span className="truncate font-mono text-[12px]">{t.fairshare_group} · {t.weight}</span>
                  <Link href={`/keys?tenant=${t.id}`} className="text-right font-mono text-[12px] hover:underline" title={r.keysOff ? `${r.keysOff} off` : undefined}>{r.keys}</Link>
                  <span className="text-right font-mono text-[12px]">{r.users}</span>
                  {r.requests24h ? (
                    <Link href={logsHref({ team: t.id, window: "24h" })} title="See these requests" className="text-right font-mono text-[12px] underline decoration-muted-foreground/40 underline-offset-[3px] hover:decoration-foreground">{compact(r.requests24h)}</Link>
                  ) : (
                    <span className="text-right font-mono text-[12px] text-muted-foreground">0</span>
                  )}
                  <span className="flex min-w-0 flex-col gap-1">
                    <span className="truncate font-mono text-[12px]">{r.budget ? r.budget.label : `${money(monthSpend[t.id] ?? 0)} · no budget`}</span>
                    <BudgetBar share={r.budget ? r.budget.share : null} />
                  </span>
                  <span className="truncate text-[12px] text-secondary-foreground" title={limitsLabel(t)}>
                    {limitsLabel(t)}
                    {r.waiting > 0 && <span className="text-foreground"> · {r.waiting} waiting</span>}
                  </span>
                  <span className="truncate text-[12.5px] text-secondary-foreground" title={accessLabel(t)}>{accessLabel(t)}</span>
                </div>
              );
            })}
          </div>
        </div>
        <div className="flex flex-wrap justify-between gap-2 border-t border-border px-[18px] py-3 text-[12.5px] text-muted-foreground">
          <span>{shown.length === tenants.length ? `${tenants.length} tenants` : `${shown.length} of ${tenants.length} tenants`} · click one for its page</span>
          <span>Policies set: guardrails {policies.guardrails} · compression {policies.compression} · tracing {policies.tracing}</span>
        </div>
      </section>

      <Dialog open={adding} onOpenChange={setAdding}>
        <DialogContent className="grid h-[min(760px,85vh)] max-h-[85vh] max-w-4xl grid-rows-[auto_minmax(0,1fr)] overflow-hidden">
          <DialogHeader>
            <DialogTitle>Add a tenant</DialogTitle>
            <DialogDescription>A team or project that gets its own keys, share of the gateway, and optional limits.</DialogDescription>
          </DialogHeader>
          <CreateTenant models={models} tenantWeights={tenants.map((t) => t.weight)} onCreated={() => { setAdding(false); router.refresh(); }} />
        </DialogContent>
      </Dialog>
    </div>
  );
}
