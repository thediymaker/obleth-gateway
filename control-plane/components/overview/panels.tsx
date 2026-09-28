"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import type { AuditEntry, CacheStats, RouterReadinessView, TenantFairshareView } from "@/lib/obleth";
import type { OverviewSummary } from "@/lib/overview-summary";
import type { OverviewTenantRow, OverviewWindow } from "@/lib/overview-data";
import { compact, describeAudit, RANGE_LABEL, type AttentionItem, type OverviewRange } from "@/lib/overview-model";
import { logsHref } from "@/lib/log-links";
import { cn } from "@/lib/utils";
import { HealthGlyph, Meter, Panel, PanelLink, Pill, SectionLabel } from "./ui";

const HIDE_KEY = "obleth-overview:hidden";
const HIDE_MS = 3_600_000;

/** Items hidden in this browser, keyed by item, until a time. Best effort: storage may be unavailable. */
function useHidden() {
  const [hidden, setHidden] = useState<Record<string, number>>({});
  useEffect(() => {
    try {
      const raw = JSON.parse(localStorage.getItem(HIDE_KEY) ?? "{}") as Record<string, number>;
      const now = Date.now();
      setHidden(Object.fromEntries(Object.entries(raw).filter(([, until]) => typeof until === "number" && until > now)));
    } catch { /* nothing hidden */ }
  }, []);
  const save = (next: Record<string, number>) => {
    setHidden(next);
    try { localStorage.setItem(HIDE_KEY, JSON.stringify(next)); } catch { /* kept for this tab */ }
  };
  return { hidden, save };
}

/**
 * The conditions that need a person, most urgent first. Absent when there are
 * none: an all-clear is said once, in the page header, not in a big empty box.
 */
export function NeedsAttention({ items, watching }: { items: AttentionItem[]; watching: string[] }) {
  const { hidden, save } = useHidden();
  const now = Date.now();
  const shown = items.filter((i) => !(hidden[i.key] > now));
  const hiddenCount = items.length - shown.length;
  if (!shown.length && !hiddenCount) {
    return watching.length ? <Watching watching={watching} standalone /> : null;
  }
  if (!shown.length) {
    return (
      <p className="flex flex-wrap items-center gap-2 rounded-xl border border-dashed border-border px-4 py-2.5 text-[12.5px] text-muted-foreground">
        {hiddenCount} attention item{hiddenCount === 1 ? "" : "s"} hidden for now in this browser.
        <button type="button" className="text-secondary-foreground underline underline-offset-2 hover:text-foreground" onClick={() => save({})}>Show them</button>
      </p>
    );
  }
  return (
    <Panel
      title="Needs attention"
      subtitle="Most urgent first. Each item clears by itself once the condition does."
      className="border-muted-foreground/60"
      action={
        <button
          type="button"
          onClick={() => save({ ...hidden, ...Object.fromEntries(shown.map((i) => [i.key, Date.now() + HIDE_MS])) })}
          className="inline-flex h-8 items-center rounded-lg border border-border px-3 text-[12.5px] font-medium hover:bg-accent"
          title="Hides these items in this browser for an hour. New problems still appear."
        >
          Hide for 1 hour
        </button>
      }
    >
      <ul className="mt-3.5">
        {shown.map((item) => (
          <li key={item.key} className="grid gap-3 border-t border-border px-[18px] py-4 md:grid-cols-[6.5rem_minmax(0,1fr)_auto] md:items-center md:gap-4">
            <Pill inverted={item.urgent} className="justify-self-start">{item.badge}</Pill>
            <div className="min-w-0 space-y-1">
              <div className="text-[14.5px] font-semibold">{item.title}</div>
              <div className="text-[12.5px] text-muted-foreground">{item.detail}</div>
            </div>
            <div className="flex flex-wrap gap-2">
              {item.actions.map((a) => (
                <Link key={a.label} href={a.href} className={cn("inline-flex h-8 items-center rounded-lg border px-3 text-[12.5px] font-medium", a.primary ? "border-foreground bg-foreground text-background hover:bg-foreground/90" : "border-border hover:bg-accent")}>
                  {a.label}
                </Link>
              ))}
            </div>
          </li>
        ))}
      </ul>
      {(watching.length > 0 || hiddenCount > 0) && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-border px-[18px] py-3 text-[12.5px] text-muted-foreground">
          {watching.length > 0 && <span className="inline-flex items-center gap-2"><HealthGlyph state="unknown" />Also watching: {watching.join(" · ")}</span>}
          {hiddenCount > 0 && <button type="button" className="underline underline-offset-2 hover:text-foreground" onClick={() => save({})}>{hiddenCount} hidden · show</button>}
        </div>
      )}
    </Panel>
  );
}

function Watching({ watching, standalone }: { watching: string[]; standalone?: boolean }) {
  return (
    <p className={cn("flex items-center gap-2 text-[12.5px] text-muted-foreground", standalone && "rounded-xl border border-border px-4 py-2.5")}>
      <HealthGlyph state="unknown" />Watching: {watching.join(" · ")}
    </p>
  );
}

export function TopTenants({ window, live }: { window: OverviewWindow; live: Map<string, TenantFairshareView> }) {
  const rows = window.tenants;
  const top = rows[0]?.requests ?? 0;
  const hasCost = rows.some((r) => r.cost !== null);
  const cols = hasCost ? "grid-cols-[minmax(0,1fr)_4.5rem_4rem_4.5rem_3.5rem]" : "grid-cols-[minmax(0,1fr)_4.5rem_4rem_3.5rem]";
  const money = (v: number | null) => (v === null ? "—" : `$${v >= 100 ? v.toFixed(0) : v.toFixed(2)}`);
  const line = (r: Pick<OverviewTenantRow, "requests" | "tokens" | "cost">, now: number | string, name: React.ReactNode, key: string, muted = false, tenantId?: string) => (
    <div key={key} className={cn("grid items-center gap-3 border-t border-border px-[18px] py-[9px] text-[13px]", cols, muted && "text-muted-foreground")}>
      {name}
      {tenantId ? (
        <Link href={logsHref({ team: tenantId, window: window.range })} title="See these requests" className="text-right font-mono text-xs tabular-nums underline-offset-2 hover:underline">{compact(r.requests)}</Link>
      ) : (
        <span className="text-right font-mono text-xs tabular-nums">{compact(r.requests)}</span>
      )}
      <span className="text-right font-mono text-xs tabular-nums">{compact(r.tokens)}</span>
      {hasCost && <span className="text-right font-mono text-xs tabular-nums">{money(r.cost)}</span>}
      <span className="text-right font-mono text-xs tabular-nums">{now}</span>
    </div>
  );
  return (
    <Panel title="Top tenants" subtitle={RANGE_LABEL[window.range]} action={<PanelLink href="/tenants">All tenants</PanelLink>}>
      <div className="mt-3">
        {rows.length === 0 ? (
          <p className="border-t border-border px-[18px] py-6 text-center text-sm text-muted-foreground">No tenant traffic in this window.</p>
        ) : (
          <>
            <div className={cn("grid gap-3 border-t border-border px-[18px] py-2 text-[11.5px] text-muted-foreground", cols)}>
              <span>Tenant</span><span className="text-right">Requests</span><span className="text-right">Tokens</span>{hasCost && <span className="text-right">Spend</span>}<span className="text-right" title="Requests in flight right now">Now</span>
            </div>
            {rows.map((r) => line(r, live.get(r.id)?.in_flight ?? 0, (
              <span className="flex min-w-0 flex-col gap-1.5"><span className="truncate" title={r.name}>{r.name}</span><Meter value={r.requests} max={top} className="h-[3px]" /></span>
            ), r.id, false, r.id))}
            {window.otherTenants.count > 0 && line(window.otherTenants, "", <span>{window.otherTenants.count} more tenant{window.otherTenants.count === 1 ? "" : "s"}</span>, "rest", true)}
          </>
        )}
      </div>
      <div className="pb-2" />
    </Panel>
  );
}

function agoShort(iso: string, now: number) {
  const s = Math.max(0, (now - new Date(iso).getTime()) / 1000);
  if (!Number.isFinite(s)) return "";
  if (s < 3600) return `${Math.max(1, Math.round(s / 60))}m`;
  if (s < 86_400) return `${Math.round(s / 3600)}h`;
  return `${Math.round(s / 86_400)}d`;
}

export function RecentChanges({ audit, models, tenants }: { audit: AuditEntry[]; models: Map<string, string>; tenants: Map<string, string> }) {
  const now = Date.now();
  return (
    <Panel title="Recent changes" subtitle="From the audit log" action={<PanelLink href="/audit">Audit log</PanelLink>}>
      <ul className="mt-3 pb-2">
        {audit.length === 0 && <li className="border-t border-border px-[18px] py-6 text-center text-sm text-muted-foreground">No changes recorded yet.</li>}
        {audit.slice(0, 6).map((e) => {
          const d = describeAudit(e, { models, tenants });
          const initials = d.actor.replace(/@.*/, "").slice(0, 2).toUpperCase();
          // Dashboard users act under their email; anything else is the API token or automation.
          const system = !d.actor.includes("@");
          return (
            <li key={e.id} className="grid grid-cols-[1.5rem_minmax(0,1fr)_2.5rem] items-center gap-3 border-t border-border px-[18px] py-[9px] text-[13px]">
              <span className={cn("flex h-6 w-6 items-center justify-center rounded-full text-[10.5px] font-semibold", system ? "border border-dashed border-muted-foreground/60" : "bg-secondary")} aria-hidden="true">{initials}</span>
              <span className="min-w-0 truncate" title={`${d.actor} ${d.action} ${d.target}`}><strong className="font-medium">{d.actor.replace(/@.*/, "")}</strong> <span className="text-muted-foreground">{d.action}</span> {d.target}</span>
              <span className="text-right font-mono text-xs text-muted-foreground" title={new Date(e.ts).toLocaleString()}>{agoShort(e.ts, now)}</span>
            </li>
          );
        })}
      </ul>
    </Panel>
  );
}

export function FooterTiles({ summary, cache, readiness, range }: { summary: OverviewSummary; cache: CacheStats | null; readiness: RouterReadinessView | null; range: OverviewRange }) {
  const lookups = cache ? Number(cache.hits) + Number(cache.misses) : 0;
  const hitRate = lookups ? (Number(cache!.hits) / lookups) * 100 : null;
  const warn = readiness?.findings.filter((f) => f.severity === "warn") ?? [];
  const tile = "flex min-w-0 flex-col gap-1.5 rounded-xl border border-border bg-card px-[18px] py-3.5 transition-colors hover:border-muted-foreground/60";
  const suffix = range;
  return (
    <div className="grid gap-4 md:grid-cols-3">
      <Link href="/settings" className={tile}>
        <SectionLabel>Response cache · {suffix}</SectionLabel>
        <span className="text-xl font-semibold">{hitRate === null ? "No lookups" : `${hitRate.toFixed(1)}% hits`}</span>
        <span className="truncate text-xs text-muted-foreground">{cache && Number(cache.tokens_saved) > 0 ? `${compact(Number(cache.tokens_saved))} tokens not recomputed` : "Caching is set per model"}</span>
      </Link>
      <Link href="/settings" className={tile}>
        <SectionLabel>Energy · {suffix}</SectionLabel>
        <span className="text-xl font-semibold">{summary.energyWh > 0 ? `${summary.energyWh >= 1000 ? (summary.energyWh / 1000).toFixed(1) : summary.energyWh.toFixed(0)} ${summary.energyWh >= 1000 ? "kWh" : "Wh"}` : "Not measured"}</span>
        <span className="truncate text-xs text-muted-foreground">{summary.energyWh > 0 ? `${summary.co2G >= 1000 ? `${(summary.co2G / 1000).toFixed(1)} kg` : `${summary.co2G.toFixed(0)} g`} CO₂${summary.energyCostUsd > 0 ? ` · $${summary.energyCostUsd.toFixed(2)} at the set rate` : ""}` : "Turn on energy tracking in Settings"}</span>
      </Link>
      <Link href="/settings" className={tile}>
        <SectionLabel>Routing readiness</SectionLabel>
        <span className="text-xl font-semibold">{readiness ? (warn.length ? `${warn.length} finding${warn.length === 1 ? "" : "s"}` : "Ready") : "Unavailable"}</span>
        <span className="truncate text-xs text-muted-foreground">{readiness ? [warn[0]?.title, readiness.classifier_active ? "classifier on" : "classifier off"].filter(Boolean).join(" · ") : "Could not read the router"}</span>
      </Link>
    </div>
  );
}
