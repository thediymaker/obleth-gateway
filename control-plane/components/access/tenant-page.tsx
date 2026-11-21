"use client";

import { useCallback, useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { ChevronDown, MoreHorizontal } from "lucide-react";
import { deleteTenantAction, setTenantStatusAction } from "@/app/actions";
import { TenantSettings } from "@/components/access/tenant-settings";
import { BudgetBar, TENANT_STATUS_LABEL, tenantGlyph } from "@/components/access/ui";
import { useFairshareLive } from "@/components/fairshare/hooks";
import { Meter, Panel } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import {
  accessLabel,
  tenantHref,
  budgetView,
  changeVs,
  groupShare,
  lastUsed,
  windowsSummary,
  type TenantDay,
  type TenantOverviewData,
} from "@/lib/access-model";
import type { AdminUser } from "@/lib/auth/users";
import { logsHref } from "@/lib/log-links";
import type { ApiKey, BudgetUsage, Tenant } from "@/lib/obleth";
import { compact, dateLabel, dateTime, formatDate, money } from "@/lib/format";
import { cn, getJson } from "@/lib/utils";
import { StatusDot } from "@/components/ui/status";
import { Pill } from "@/components/ui/pill";
import { Segmented } from "@/components/ui/segmented";
import { Notice } from "@/components/ui/notice";
import { Tile } from "@/components/ui/tile";
import { jumpTo, SettingsNav, type FindItem, type NavItem } from "@/components/ui/settings-nav";
import { SectionLabel } from "@/components/ui/section-label";

type NavId = "overview" | "profile" | "limits" | "hours" | "budget" | "models" | "guardrails" | "compression" | "advanced" | "keys" | "people" | "activity";

const SETTINGS: { id: string; label: string; card: NavId; words: string }[] = [
  { id: "set-name", label: "Name", card: "profile", words: "rename title" },
  { id: "set-organization", label: "Organization", card: "profile", words: "department" },
  { id: "set-description", label: "Description", card: "profile", words: "about notes" },
  { id: "set-contact", label: "Contact", card: "profile", words: "email owner" },
  { id: "set-group", label: "Fairshare group", card: "limits", words: "pool share" },
  { id: "set-weight", label: "Weight", card: "limits", words: "priority share fairshare" },
  { id: "set-rate", label: "Token rate", card: "limits", words: "tpm tokens per minute quota rate limit" },
  { id: "set-in-flight", label: "In flight per model", card: "limits", words: "concurrency parallel slots max" },
  { id: "set-timezone", label: "Time zone", card: "hours", words: "tz zone" },
  { id: "set-hours", label: "Weekly hours", card: "hours", words: "schedule window open closed" },
  { id: "set-dates", label: "Open from · until", card: "hours", words: "term trial start end expire date" },
  { id: "set-budget", label: "Spend cap", card: "budget", words: "cost dollars usd money budget" },
  { id: "set-token-cap", label: "Token cap", card: "budget", words: "tokens budget" },
  { id: "set-models", label: "Models it can use", card: "models", words: "allowlist allowed restrict" },
  { id: "set-guardrails", label: "Guardrails", card: "guardrails", words: "pii ferpa redact block injection scanner safety" },
  { id: "set-compression", label: "Compression", card: "compression", words: "dedup compact lossy" },
  { id: "set-tracing", label: "Trace requests", card: "advanced", words: "tracing spans timeline" },
  { id: "set-synthetic", label: "Synthetic traffic", card: "advanced", words: "benchmark test" },
];

const CARD_LABEL: Record<NavId, string> = {
  overview: "Overview",
  profile: "Profile",
  limits: "Limits",
  hours: "Access hours",
  budget: "Budget",
  models: "Models",
  guardrails: "Guardrails",
  compression: "Compression",
  advanced: "Advanced",
  keys: "Keys",
  people: "People",
  activity: "Activity",
};

const FIND: FindItem[] = SETTINGS.map((s) => ({ id: s.id, label: s.label, words: s.words, hint: CARD_LABEL[s.card] }));

type Measure = "requests" | "spend" | "tokens";

function DayChart({ days, tenantId }: { days: TenantDay[]; tenantId: string }) {
  const [measure, setMeasure] = useState<Measure>("requests");
  const [hover, setHover] = useState<number | null>(null);
  const value = (d: TenantDay) => (measure === "requests" ? d.requests : measure === "spend" ? d.cost : d.tokens);
  const max = Math.max(1, ...days.map(value));
  const fmt = (v: number) => (measure === "spend" ? money(v) : compact(v));
  const total = days.reduce((n, d) => n + value(d), 0);
  const h = hover !== null ? days[hover] : null;
  const dayLabel = (day: string) => dateLabel(new Date(`${day}T12:00:00Z`));
  const ticks = days.filter((_, i) => i % 7 === 0 || i === days.length - 1);
  return (
    <Panel
      title="Per day"
      subtitle={measure === "requests" ? "Last 30 days · failures bright · click a day for its requests" : "Last 30 days · click a day for its requests"}
      action={<Segmented label="Measure" value={measure} onChange={setMeasure} options={[{ value: "requests", label: "Requests" }, { value: "spend", label: "Spend" }, { value: "tokens", label: "Tokens" }]} />}
    >
      <div className="px-[18px] pb-3 pt-2">
        <p className="h-5 text-[12.5px] text-secondary-foreground" aria-live="polite">
          {h ? (
            <><span className="font-medium text-foreground">{dayLabel(h.day)}</span> · {fmt(value(h))} {measure === "requests" ? `requests${h.failed ? ` · ${compact(h.failed)} failed` : ""}` : measure === "tokens" ? "tokens" : ""}</>
          ) : (
            <>{fmt(total)} {measure === "requests" ? "requests" : measure === "tokens" ? "tokens" : "spent"} in 30 days</>
          )}
        </p>
        <div className="flex h-[180px] items-end gap-[3px] border-b border-border pt-2" onMouseLeave={() => setHover(null)}>
          {days.map((d, i) => {
            const v = value(d);
            const failed = measure === "requests" ? d.failed : 0;
            return (
              <Link
                key={d.day}
                href={logsHref({ team: tenantId, since: d.day, until: d.day })}
                onMouseEnter={() => setHover(i)}
                onFocus={() => setHover(i)}
                aria-label={`${dayLabel(d.day)}: ${fmt(v)}`}
                className="flex h-full flex-1 flex-col justify-end rounded-t-[2px] hover:bg-muted/40"
              >
                {failed > 0 && <span className="block rounded-t-[2px] bg-foreground" style={{ height: `${(failed / max) * 100}%` }} />}
                <span className={cn("block bg-muted-foreground/70", failed === 0 && "rounded-t-[2px]", hover === i && "bg-secondary-foreground")} style={{ height: `${(Math.max(0, v - failed) / max) * 100}%` }} />
              </Link>
            );
          })}
        </div>
        <div className="flex justify-between pt-1.5 font-mono text-[10.5px] text-muted-foreground">
          {ticks.map((d, i) => <span key={d.day}>{i === ticks.length - 1 ? "today" : dayLabel(d.day)}</span>)}
        </div>
      </div>
    </Panel>
  );
}

function Row({ label, children, href }: { label: string; children: React.ReactNode; href?: string }) {
  return (
    <div className="grid grid-cols-[104px_minmax(0,1fr)_auto] items-center gap-3 border-t border-border py-2 text-[13px] first:border-t-0">
      <span className="text-muted-foreground">{label}</span>
      <span className="min-w-0 text-secondary-foreground">{children}</span>
      {href ? <a href={href} onClick={(e) => { e.preventDefault(); jumpTo(href.slice(1)); }} className="text-[12px] text-muted-foreground hover:text-foreground">Change</a> : <span />}
    </div>
  );
}

export function TenantPage({
  tenant,
  tenants,
  keys,
  people,
  budgets,
  models,
}: {
  tenant: Tenant;
  tenants: Tenant[];
  keys: ApiKey[];
  people: AdminUser[];
  budgets: BudgetUsage[];
  models: string[];
}) {
  const router = useRouter();
  const { confirm, confirmElement } = useConfirm();
  const [pending, start] = useTransition();
  const [notice, setNotice] = useState<{ text: string; strong?: boolean } | null>(null);
  const [dirty, setDirty] = useState<string[]>([]);
  // Only this tenant's row: the rest of the view changing does not re-render the page.
  const fairshare = useFairshareLive({ select: (view) => view.tenants.find((t) => t.tenant_id === tenant.id) ?? null });
  const overview = useQuery({
    queryKey: ["tenant-overview", tenant.id],
    queryFn: () => getJson<TenantOverviewData>(`/api/live/tenants/${tenant.id}/overview`),
    refetchInterval: 60_000,
  });
  const data = overview.data;

  const usage = budgets.find((b) => b.scope === "tenant" && b.id === tenant.id);
  const budget = budgetView(usage);
  const live = fairshare.data;
  const share = groupShare(tenants, tenant.id, tenant.fairshare_group, tenant.weight);
  const groupWeight = tenants.filter((t) => t.fairshare_group === tenant.fairshare_group && t.status === "active").reduce((n, t) => n + t.weight, 0);
  const usageByKey = new Map((data?.keys ?? []).map((k) => [k.key_id, k]));
  const sortedKeys = [...keys].sort((a, b) => (usageByKey.get(b.id)?.last_used_ms ?? 0) - (usageByKey.get(a.id)?.last_used_ms ?? 0) || Number(a.disabled) - Number(b.disabled));
  const keysUsed = (data?.keys ?? []).filter((k) => k.requests > 0).length;
  const allowed = tenant.allowed_models?.length ?? 0;

  function setStatus(status: string) {
    if (status === tenant.status) return;
    start(async () => {
      if (status !== "active") {
        const ok = await confirm({
          title: `${status === "suspended" ? "Suspend" : "Archive"} ${tenant.name}?`,
          description: "Its keys stop working straight away. Its settings and history are kept, and you can make it active again.",
          confirmLabel: status === "suspended" ? "Suspend" : "Archive",
        });
        if (!ok) return;
      }
      const result = await setTenantStatusAction(tenant.id, status);
      setNotice(result.ok ? null : { text: result.error, strong: true });
      router.refresh();
    });
  }

  function remove() {
    start(async () => {
      const ok = await confirm({
        title: `Delete ${tenant.name}?`,
        description: `Its ${keys.length} key${keys.length === 1 ? " is" : "s are"} deleted with it and stop working at once. Past usage stays in the logs. This cannot be undone.`,
        confirmLabel: "Delete",
      });
      if (!ok) return;
      const result = await deleteTenantAction(tenant.id);
      if (result.ok) router.push("/tenants");
      else setNotice({ text: `Delete failed: ${result.error}`, strong: true });
    });
  }

  const onSaved = useCallback((form: FormData) => {
    const name = String(form.get("name") ?? "").trim();
    if (name && name !== tenant.name) router.replace(tenantHref({ name }));
    else router.refresh();
  }, [router, tenant.name]);

  const nav: NavItem[] = ([
    { id: "overview" },
    { id: "profile" },
    { id: "limits" },
    { id: "hours", count: tenant.weekly_windows?.length || tenant.active_from || tenant.active_until ? "set" : undefined },
    { id: "budget", count: budget ? `${Math.round(budget.share * 100)}%` : undefined },
    { id: "models", count: allowed ? allowed : "all" },
    { id: "guardrails", count: tenant.guardrails_policy ? "on" : "off" },
    { id: "compression", count: tenant.compression_policy ? (tenant.compression_policy.enabled ? "on" : "off") : undefined },
    { id: "advanced" },
    { id: "keys", count: keys.length },
    { id: "people", count: people.length },
  ] as { id: NavId; count?: string | number }[]).map((n) => ({ ...n, label: CARD_LABEL[n.id] }));

  const failShare = data && data.requests24h ? data.failed24h / data.requests24h : 0;
  const pct = (x: number) => (x >= 0.1 ? `${Math.round(x * 100)}%` : `${(x * 100).toFixed(1)}%`);
  const topModels = (data?.models ?? []).slice(0, 5);
  const topMax = Math.max(1, ...topModels.map((m) => m.requests));

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-5">
      {confirmElement}
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <p className="text-[12.5px] text-muted-foreground">
            <Link href="/tenants" className="text-secondary-foreground hover:text-foreground">Tenants</Link>
            {tenant.organization ? ` / ${tenant.organization}` : ""}
          </p>
          <h1 className="truncate text-[26px] font-semibold tracking-tight">{tenant.name}</h1>
          <div className="flex flex-wrap items-center gap-2">
            <Pill><StatusDot glyph={tenantGlyph(tenant.status)} className="h-[7px] w-[7px]" />{TENANT_STATUS_LABEL[tenant.status] ?? tenant.status}</Pill>
            <Pill>{tenant.fairshare_group} group · weight {tenant.weight}</Pill>
            <Pill>{tenant.weekly_windows?.length || tenant.active_from || tenant.active_until ? accessLabel(tenant) : "Open all hours"}</Pill>
            {live && live.queued > 0 && <Pill inverted>{live.queued} waiting for slots</Pill>}
            {budget && budget.share >= 1 && <Pill inverted>Budget used up</Pill>}
            {tenant.synthetic && <Pill>Synthetic</Pill>}
            {tenant.contact_email && <span className="text-[12.5px] text-muted-foreground">contact {tenant.contact_email}</span>}
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button type="button" variant="outline" size="sm" className="h-9" disabled={pending} aria-label="Status">
                <StatusDot glyph={tenantGlyph(tenant.status)} />{TENANT_STATUS_LABEL[tenant.status] ?? tenant.status}<ChevronDown className="h-3.5 w-3.5" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => setStatus("active")}>Active: its keys work</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => setStatus("suspended")}>Suspended: keys refused for now</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => setStatus("archived")}>Archived: done with it</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
          <Button asChild variant="outline" size="sm" className="h-9"><Link href={logsHref({ team: tenant.id, window: "24h" })}>See its requests</Link></Button>
          <Button asChild variant="outline" size="sm" className="h-9"><Link href={`/keys?new=${tenant.id}`}>New key</Link></Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button type="button" variant="outline" size="icon" className="h-9 w-9" aria-label="More actions"><MoreHorizontal className="h-4 w-4" /></Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => router.push(`/keys?tenant=${tenant.id}`)}>All its keys</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => { void navigator.clipboard?.writeText(tenant.id); }}>Copy the tenant id</DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onSelect={remove}>Delete tenant…</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      {notice && <Notice onDismiss={() => setNotice(null)} strong={notice.strong}>{notice.text}</Notice>}

      <div className="grid items-start gap-6 lg:grid-cols-[200px_minmax(0,1fr)]">
        <SettingsNav label="Tenant sections" items={nav} after={[{ id: "activity", label: "Activity" }]} find={FIND} findId="tenant-setting-results" dirty={dirty} />

        <div className="flex min-w-0 flex-col gap-4">
          <div id="overview" className="flex scroll-mt-24 flex-col gap-4">
            <div className="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-5">
              <Tile
                label="Requests · 24h"
                value={data ? compact(data.requests24h) : "—"}
                href={data?.requests24h ? logsHref({ team: tenant.id, window: "24h" }) : undefined}
                detail={data ? changeVs(data.requests24h, data.previous24h) : null}
              />
              <Tile
                label="Failed · 24h"
                value={data ? compact(data.failed24h) : "—"}
                emphasis={failShare >= 0.05}
                href={data?.failed24h ? logsHref({ team: tenant.id, window: "24h", status: "error" }) : undefined}
                detail={data?.failed24h ? `${pct(failShare)}${data.topFailure ? ` · mostly ${data.topFailure.status_code} on ${data.topFailure.model}` : ""}` : data ? "none" : null}
              />
              <div className={cn("flex min-w-0 flex-col gap-1 rounded-xl border bg-card px-4 py-3", budget && budget.share >= 0.8 ? "border-muted-foreground" : "border-border")}>
                <SectionLabel>Budget{usage?.period === "monthly" ? ` · ${formatDate(new Date(), { month: "long" })}` : ""}</SectionLabel>
                {budget ? (
                  <>
                    <span className="truncate text-[15px] font-semibold leading-[30px]">{budget.label}</span>
                    <BudgetBar share={budget.share} />
                    <span className="truncate text-xs text-muted-foreground">
                      {[budget.pace, budget.resetsAt ? `resets ${dateLabel(budget.resetsAt)}` : null].filter(Boolean).join(" · ") || " "}
                    </span>
                  </>
                ) : (
                  <>
                    <span className="text-[24px] font-semibold leading-tight">None</span>
                    <a href="#set-budget" onClick={(e) => { e.preventDefault(); jumpTo("set-budget"); }} className="text-xs text-muted-foreground hover:text-foreground">Set a cap</a>
                  </>
                )}
              </div>
              <Tile
                label="Running now"
                value={live ? live.in_flight : "—"}
                href="/fairshare"
                detail={live ? `fair share ${Math.round(live.expected_slots)}${live.queued ? ` · ${live.queued} waiting` : ""}` : fairshare.isLoading ? null : "not seen by the gateway"}
              />
              <Tile
                label="Tokens · 30 days"
                value={data ? compact(data.tokens30d) : "—"}
                detail={data && data.tokens30d ? `${Math.round((data.inputTokens30d / data.tokens30d) * 100)}% in · ${keysUsed} key${keysUsed === 1 ? "" : "s"} used` : null}
              />
            </div>

            <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_360px]">
              {data ? <DayChart days={data.days} tenantId={tenant.id} /> : <div className="skeleton h-[290px] rounded-xl border border-border" />}
              <div className="flex flex-col gap-4">
                <Panel title="Limits right now">
                  <div className="px-[18px] pb-2 pt-1.5">
                    <Row label="Token rate" href="#set-rate">{tenant.tokens_per_minute > 0 ? `${compact(tenant.tokens_per_minute)} a minute` : "No limit"}</Row>
                    <Row label="In flight" href="#set-in-flight">{live ? `${live.in_flight} running` : "—"}{tenant.max_in_flight ? ` · cap ${tenant.max_in_flight} per model` : " · no cap"}</Row>
                    <Row label="Share" href="#set-weight">weight {tenant.weight} of {groupWeight} in {tenant.fairshare_group} · {Math.round(share * 100)}% when busy</Row>
                    <Row label="Models" href="#set-models">{allowed ? `${allowed} of ${models.length}` : `all ${models.length}`}</Row>
                    <Row label="Hours" href="#set-hours">{windowsSummary(tenant.weekly_windows)}</Row>
                  </div>
                </Panel>
                <Panel title="Models it uses · 30 days">
                  <div className="px-[18px] pb-2 pt-1.5">
                    {topModels.length === 0 && <p className="py-3 text-[12.5px] text-muted-foreground">{data ? "No requests in 30 days." : "Loading…"}</p>}
                    {topModels.map((m) => (
                      <Link key={m.model} href={logsHref({ team: tenant.id, model: m.model, window: "30d" })} className="grid grid-cols-[minmax(0,1fr)_80px_48px] items-center gap-3 border-t border-border py-2 first:border-t-0 hover:text-foreground">
                        <span className="truncate font-mono text-[12.5px]">{m.model}</span>
                        <Meter value={m.requests} max={topMax} />
                        <span className="text-right font-mono text-[12px] tabular-nums">{compact(m.requests)}</span>
                      </Link>
                    ))}
                  </div>
                </Panel>
              </div>
            </div>
          </div>

          <TenantSettings tenant={tenant} tenants={tenants} models={models} budget={budget && usage ? { ...budget, usedCost: usage.used_cost_usd, usedTokens: usage.used_tokens } : null} onDirty={setDirty} onSaved={onSaved} />

          <div className="grid gap-4 xl:grid-cols-[minmax(0,1.4fr)_minmax(0,1fr)]">
            <section id="keys" aria-label="Keys" className="scroll-mt-24 rounded-xl border border-border bg-card">
              <header className="flex items-center justify-between px-[18px] pb-2 pt-4">
                <h2 className="text-sm font-semibold">Keys <span className="font-normal text-muted-foreground">· {keys.length}</span></h2>
                <Link href={`/keys?new=${tenant.id}`} className="text-[12.5px] text-secondary-foreground hover:text-foreground">New key</Link>
              </header>
              <div className="px-[18px] pb-2">
                <div className="grid grid-cols-[minmax(0,1fr)_96px_64px_64px] gap-3 py-2 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">
                  <span>Key</span><span>Last used</span><span className="text-right">Req 30d</span><span className="text-right">Spend</span>
                </div>
                {keys.length === 0 && <p className="border-t border-border py-3 text-[12.5px] text-muted-foreground">No keys yet. People linked here can make their own in the portal.</p>}
                {sortedKeys.slice(0, 6).map((k) => {
                  const u = usageByKey.get(k.id);
                  return (
                    <Link key={k.id} href={`/keys?key=${k.id}`} className={cn("grid grid-cols-[minmax(0,1fr)_96px_64px_64px] items-center gap-3 border-t border-border py-2 text-[13px] hover:bg-muted/30", k.disabled && "text-muted-foreground")}>
                      <span className="flex min-w-0 items-center gap-2">
                        <StatusDot glyph={k.disabled ? "off" : "on"} />
                        <span className="truncate">{k.name || k.key_prefix}</span>
                        <span className="shrink-0 font-mono text-[11.5px] text-muted-foreground">{k.kind === "identity" ? "identity" : `${k.key_prefix}…`}</span>
                        {k.disabled && <span className="text-[11.5px]">· off</span>}
                      </span>
                      <span className="font-mono text-[12px] text-secondary-foreground">{lastUsed(u?.last_used_ms ?? 0)}</span>
                      <span className="text-right font-mono text-[12px]">{u?.requests ? compact(u.requests) : "—"}</span>
                      <span className="text-right font-mono text-[12px]">{u?.cost_usd ? money(u.cost_usd) : "—"}</span>
                    </Link>
                  );
                })}
                {keys.length > 0 && (
                  <p className="border-t border-border py-2.5 text-xs text-muted-foreground">
                    {keys.length > 6 ? `${keys.length - 6} more · ` : ""}<Link href={`/keys?tenant=${tenant.id}`} className="text-secondary-foreground hover:text-foreground">All keys for {tenant.name} ›</Link>
                  </p>
                )}
              </div>
            </section>

            <section id="people" aria-label="People" className="scroll-mt-24 rounded-xl border border-border bg-card">
              <header className="flex items-center justify-between px-[18px] pb-2 pt-4">
                <h2 className="text-sm font-semibold">People <span className="font-normal text-muted-foreground">· {people.length ? `${people.length} can use the portal` : "none linked"}</span></h2>
                <Link href="/users" className="text-[12.5px] text-secondary-foreground hover:text-foreground">Link a user ›</Link>
              </header>
              <div className="px-[18px] pb-2">
                {people.map((p) => (
                  <div key={p.id} className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3 border-t border-border py-2 text-[13px] first:border-t-0">
                    <span className="truncate">{p.email}</span>
                    <Pill>{p.role}</Pill>
                  </div>
                ))}
                <p className="border-t border-border py-2.5 text-xs text-muted-foreground">People linked here make and manage their own keys in the portal.</p>
              </div>
            </section>
          </div>

          <Panel title="Activity" subtitle="Recent changes to this tenant" className="scroll-mt-24" label="Activity">
            <div id="activity" className="scroll-mt-24 px-[18px] pb-3 pt-2">
              {!data ? (
                <p className="py-2 text-[12.5px] text-muted-foreground">Loading…</p>
              ) : data.audit.length === 0 ? (
                <p className="py-2 text-[12.5px] text-muted-foreground">No changes recorded yet.</p>
              ) : (
                data.audit.map((e, i) => (
                  <div key={i} className="grid grid-cols-[150px_minmax(0,1fr)_auto] gap-3 border-t border-border py-2 text-[12.5px] first:border-t-0">
                    <span className="font-mono text-[11.5px] text-muted-foreground">{dateTime(e.ts)}</span>
                    <span className="truncate">{e.action.replace(/_/g, " ")}</span>
                    <span className="truncate text-muted-foreground">{e.actor}</span>
                  </div>
                ))
              )}
            </div>
          </Panel>
        </div>
      </div>
    </div>
  );
}
