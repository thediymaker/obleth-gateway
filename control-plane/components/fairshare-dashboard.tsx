"use client";

import Link from "next/link";
import { useMemo, useState } from "react";
import { useRouter } from "next/navigation";
import { useQueryClient } from "@tanstack/react-query";
import { ChevronDown, RefreshCw } from "lucide-react";
import { Select } from "@/components/ui/select";
import { isWaitingBelowShare } from "@/lib/fairshare";
import {
  buildPoolRows,
  groupShares,
  limitsNote,
  nextInLine,
  poolTenantRows,
  scopedView,
  slotModeBadge,
  tenantPools,
  whyWaiting,
  GROUP_TONES,
  OTHER_TONE,
  type PoolRow,
  type PoolTenantRow,
} from "@/lib/fairshare-model";
import type { FairshareLiveView } from "@/lib/obleth";
import { cn, formatNumber } from "@/lib/utils";
import { HealthGlyph, Meter, Panel, PanelLink, Pill, SectionLabel } from "@/components/overview/ui";
import { HelpTip } from "@/components/fairshare/help";
import { RunningHistory } from "@/components/fairshare/history";
import { TenantPanel } from "@/components/fairshare/tenant-panel";
import { useCapacityDiscovery, useFairshareHistory, useFairshareLive, useModelRoutes, useTenantSeries } from "@/components/fairshare/hooks";

export {
  appendHistoryTail,
  buildHistoryChart,
  fetchHistory,
  limitDivisor,
  limitsNote,
  replicaShare,
  scopedView,
  slotModeBadge,
  thinHistory,
} from "@/lib/fairshare-model";
export type { FairshareLiveView, GroupFairshareView, KeyFairshareView, ModelPoolView, TenantFairshareView } from "@/lib/obleth";

const FIRST_POOLS = 8;
const STATE_LABEL: Record<PoolRow["state"], string> = { full: "Full", busy: "Busy", normal: "Normal", idle: "Idle" };

function Tile({ label, value, detail, meter, emphasis }: { label: string; value: React.ReactNode; detail: React.ReactNode; meter?: [number, number]; emphasis?: boolean }) {
  return (
    <div className={cn("flex min-w-0 flex-col gap-1.5 rounded-xl border bg-card px-4 py-3.5", emphasis ? "border-muted-foreground" : "border-border")}>
      <SectionLabel className={cn(emphasis && "text-secondary-foreground")}>{label}</SectionLabel>
      <span className="text-[26px] font-semibold leading-tight tracking-tight tabular-nums">{value}</span>
      {meter && <Meter value={meter[0]} max={meter[1]} className="mt-1" strong={meter[1] > 0 && meter[0] >= meter[1]} />}
      <span className={cn("truncate text-xs", emphasis ? "text-foreground" : "text-muted-foreground")}>{detail}</span>
    </div>
  );
}

/** The four sentences behind the page, one hover away instead of on it. */
function HowItDecides() {
  return (
    <HelpTip label="How fairshare decides">
      <span className="block"><strong className="font-medium text-foreground">How fairshare decides.</strong> Each request holds one slot in its model&apos;s pool while it runs.</span>
      <span className="block">While a pool has free slots, anyone can use them, even beyond their share. That is borrowing, and it is normal.</span>
      <span className="block">When a pool is full, new requests wait, and the next free slot goes to whoever has had the least compared with their weight.</span>
      <span className="block">A tenant&apos;s fair share is its weight against everyone active in that pool, split by group first.</span>
    </HelpTip>
  );
}

export function FairshareDashboard({ tenantNames, initialPool = "all" }: { tenantNames: Record<string, string>; initialPool?: string }) {
  const queryClient = useQueryClient();
  const router = useRouter();
  const live = useFairshareLive();
  const routes = useModelRoutes();
  const discovery = useCapacityDiscovery();
  const [scope, setScopeState] = useState(initialPool);
  const [selected, setSelected] = useState<string | null>(null);
  const [allPools, setAllPools] = useState(false);
  const series = useTenantSeries(selected !== null);
  const { history, groupKeys, oldestTs, retentionMs } = useFairshareHistory(scope);

  const raw = live.data;
  // A pool the answering gateway has served has tenants to show; one it has
  // not (idle since it started) still opens, with its size and occupancy.
  const scoped = scope !== "all";
  const inPool = scoped && !!raw?.pools?.some((p) => p.model === scope);
  const view = useMemo(() => scopedView(raw, inPool ? scope : "all"), [raw, scope, inPool]);
  const pools = useMemo(() => buildPoolRows(routes.data ?? [], raw, discovery.data), [routes.data, raw, discovery.data]);
  const pool = inPool ? raw?.pools?.find((p) => p.model === scope) : undefined;
  const poolRow = pools.find((p) => p.model === scope);
  const shared = raw?.mode === "shared";

  const setScope = (next: string) => {
    setScopeState(next);
    setSelected(null);
    router.replace(next === "all" ? "/fairshare" : `/fairshare?pool=${encodeURIComponent(next)}`, { scroll: false });
  };
  const refresh = () => {
    for (const key of ["fairshare-live", "model-routes", "capacity-discovery", "fairshare-history", "fairshare-history-tail"]) void queryClient.invalidateQueries({ queryKey: [key] });
  };

  const selectedTenant = view?.tenants.find((t) => t.tenant_id === selected);
  const scopeOptions = [
    { value: "all", label: "All model pools" },
    ...pools.map((p) => ({ value: p.model, label: `${p.model} · ${p.inFlight}/${p.cap || "—"}` })),
  ];
  const modeBadge = raw ? slotModeBadge(raw) : "";
  const note = raw ? limitsNote(raw) : "";

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-5">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          {scoped && <div className="text-[12.5px] text-muted-foreground"><button type="button" onClick={() => setScope("all")} className="hover:text-foreground">Fairshare</button> / model pool</div>}
          <div className="flex items-center gap-2.5">
            <h1 className="truncate text-[26px] font-semibold tracking-tight">{scoped ? scope : "Fairshare"}</h1>
            {!scoped && <HowItDecides />}
          </div>
          <StatusLine view={view} raw={raw} pool={poolRow} inPool={inPool} loading={!raw} poolCount={pools.length} />
          {live.isError && (
            <p role="alert" className="text-xs text-secondary-foreground">
              {raw ? `Showing the last snapshot from ${new Date(live.dataUpdatedAt).toLocaleTimeString()}. Refresh failed; retrying automatically.` : "Could not load scheduler state. Retrying automatically."}
            </p>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {modeBadge && (
            <span className="inline-flex items-center gap-1.5">
              <Pill inverted={raw?.mode === "fallback"} className="h-[26px]"><span data-testid="slot-mode">{modeBadge}</span></Pill>
              {note && <HelpTip label="How limits hold across gateways" align="right"><span className="block">{note}</span></HelpTip>}
            </span>
          )}
          {pools.length > 0 && (
            <Select aria-label="Model pool" className="w-64" value={scope} onValueChange={setScope} options={scopeOptions} searchPlaceholder="Find a model pool" />
          )}
          <Link href="/fairshare/groups" className="inline-flex h-9 items-center rounded-lg border border-border px-3 text-[13px] hover:bg-accent">Groups &amp; weights</Link>
          <button type="button" onClick={refresh} aria-label="Refresh" title="Refresh" className="flex h-9 w-9 items-center justify-center rounded-lg border border-border text-muted-foreground hover:bg-accent hover:text-foreground">
            <RefreshCw className={cn("h-3.5 w-3.5", live.isFetching && "animate-spin")} />
          </button>
        </div>
      </div>

      {!raw ? (
        <p className="rounded-xl border border-dashed border-border p-10 text-center text-sm text-muted-foreground">{live.isError ? "The scheduler did not answer." : "Loading scheduler state…"}</p>
      ) : inPool && pool && view ? (
        <PoolView view={view} raw={raw} poolRow={poolRow} shared={shared} onOpen={setSelected} />
      ) : scoped ? (
        <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
          <Tile label="Running" value={<>{formatNumber(poolRow?.inFlight ?? 0)} <span className="text-sm font-normal text-muted-foreground">of {poolRow?.cap ? formatNumber(poolRow.cap) : "—"} slots</span></>} meter={[poolRow?.inFlight ?? 0, poolRow?.cap ?? 0]} detail={shared ? "Cluster-wide" : "This gateway"} />
          <div className="col-span-1 flex items-center rounded-xl border border-dashed border-border px-4 py-3.5 text-[13px] text-muted-foreground xl:col-span-3">
            Nothing has run or waited in this pool on the answering gateway since it started, so there are no tenants to show. Traffic on other gateways still counts in Running when slots are shared.
          </div>
        </div>
      ) : (
        <AllPools raw={raw} pools={pools} expanded={allPools} onExpand={() => setAllPools(!allPools)} onScope={setScope} onOpen={setSelected} tenantTotal={Object.keys(tenantNames).length} />
      )}

      {raw && <RunningHistory history={history} groups={groupKeys} oldestTs={oldestTs} retentionMs={retentionMs} scopeLabel={scoped ? scope : "all pools"} />}

      <TenantPanel tenant={selectedTenant} view={raw} keys={view?.keys} series={series.data} onClose={() => setSelected(null)} />
    </div>
  );
}

function StatusLine({ view, raw, pool, inPool, loading, poolCount }: { view?: FairshareLiveView; raw?: FairshareLiveView; pool?: PoolRow; inPool: boolean; loading: boolean; poolCount: number }) {
  if (loading || !view || !raw) return <p className="text-[13.5px] text-muted-foreground">Loading scheduler…</p>;
  const below = view.tenants.filter(isWaitingBelowShare).length;
  const queued = inPool ? pool?.queued ?? view.global_queued : view.global_queued;
  const running = inPool ? pool?.inFlight ?? view.global_in_flight : raw.mode === "shared" ? raw.cluster_in_flight ?? raw.global_in_flight : raw.global_in_flight;
  const gateways = raw.replicas ?? 1;
  return (
    <div role="status" className="flex flex-wrap items-center gap-2.5 text-[13.5px] text-secondary-foreground">
      {queued > 0
        ? <><Pill inverted>{inPool && pool?.state === "full" ? "Full · " : ""}{formatNumber(queued)} waiting</Pill>{inPool && <WhatYouCanDo pool={pool} view={view} />}</>
        : <><HealthGlyph state="healthy" className="h-[9px] w-[9px]" /><strong className="font-semibold text-foreground">No one is waiting</strong></>}
      <span>
        {queued > 0 && below > 0 ? `${below} tenant${below === 1 ? " is" : "s are"} waiting below their fair share · ` : ""}
        {inPool
          ? `${formatNumber(running)} of ${formatNumber(pool?.cap ?? view.max_in_flight)} slots in use`
          : `${formatNumber(running)} requests running across ${formatNumber(poolCount)} model pools${gateways > 1 ? ` · ${gateways} gateways` : ""}`}
      </span>
    </div>
  );
}

/** Ways out of a full pool, one hover away. */
function WhatYouCanDo({ pool, view }: { pool?: PoolRow; view: FairshareLiveView }) {
  const rows = poolTenantRows(view.tenants);
  const holder = rows.filter((r) => r.above > 0).sort((a, b) => b.above - a.above)[0];
  const waiting = rows.find((r) => r.standing === "next");
  return (
    <HelpTip label="What you can do about a full pool">
      <span className="block"><strong className="font-medium text-foreground">Grow the pool.</strong> Add capacity to the model; a discovered pool follows its backend by itself. <Link className="underline underline-offset-2" href={`/models?model=${encodeURIComponent(pool?.model ?? "")}`}>Capacity settings</Link></span>
      <span className="block"><strong className="font-medium text-foreground">Cap a tenant on this model.</strong> {holder ? `${holder.tenant.name} holds ${holder.above} above its share. ` : ""}A per-model limit stops one tenant borrowing the whole pool next time; running requests are not cut off.</span>
      <span className="block"><strong className="font-medium text-foreground">Give someone a bigger share.</strong> {waiting ? `Raise ${waiting.tenant.name}'s weight, or its group's, ` : "Raise a tenant's or a group's weight "}to move it up the line when pools are full. <Link className="underline underline-offset-2" href="/fairshare/groups">Groups &amp; weights</Link></span>
    </HelpTip>
  );
}

function AllPools({ raw, pools, expanded, onExpand, onScope, onOpen, tenantTotal }: {
  raw: FairshareLiveView; pools: PoolRow[]; expanded: boolean; onExpand: () => void; onScope: (model: string) => void; onOpen: (id: string) => void; tenantTotal: number;
}) {
  const running = raw.mode === "shared" ? raw.cluster_in_flight ?? raw.global_in_flight : raw.global_in_flight;
  const slots = (raw.mode === "shared" ? raw.configured_max_in_flight : undefined) ?? raw.max_in_flight;
  const active = raw.tenants.filter((t) => t.in_flight > 0 || t.queued > 0);
  const borrowing = active.filter((t) => t.in_flight - t.expected_slots >= 1).length;
  const waitingTenants = raw.tenants.filter((t) => t.queued > 0);
  const busiest = pools.find((p) => p.cap > 0 && p.inFlight > 0);
  const visible = expanded ? pools : pools.slice(0, FIRST_POOLS);
  const idle = pools.filter((p) => p.state === "idle").length;

  return (
    <>
      <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
        <Tile label="Running" value={<>{formatNumber(running)} <span className="text-sm font-normal text-muted-foreground">of {formatNumber(slots)} slots</span></>} meter={[running, slots]} detail={slots ? `${Math.round((running / slots) * 100)}% of every pool combined` : "No pool sizes known"} />
        <Tile label="Waiting" value={formatNumber(raw.global_queued)} emphasis={raw.global_queued > 0} detail={raw.global_queued ? `From ${waitingTenants.length} tenant${waitingTenants.length === 1 ? "" : "s"}` : "Nothing queued in any pool"} />
        <Tile label="Tenants active" value={<>{formatNumber(active.length)} {tenantTotal > 0 && <span className="text-sm font-normal text-muted-foreground">of {formatNumber(tenantTotal)}</span>}</>} detail={borrowing ? `${borrowing} using idle capacity beyond their share` : "Everyone within their share"} />
        <Tile label="Busiest pool" value={busiest ? `${Math.round((busiest.inFlight / busiest.cap) * 100)}%` : "—"} detail={busiest ? `${busiest.model} · ${busiest.inFlight} of ${busiest.cap} slots` : "Every pool is idle"} />
      </div>

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_400px]">
        <Panel title="Model pools" subtitle="Every model has its own pool of slots, and fair share is decided inside each pool. Open one to see who holds its slots.">
          <div className="mt-3 overflow-x-auto">
            <div className="min-w-[36rem]">
              <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,11rem)_4rem_4rem_4rem_4.5rem] gap-3 border-t border-border px-[18px] py-2 text-[11.5px] text-muted-foreground"><span>Pool</span><span>Slots in use</span><span className="text-right">Running</span><span className="text-right">Waiting</span><span className="text-right">Tenants</span><span className="text-right">State</span></div>
              {visible.map((p) => (
                <button key={p.model} type="button" onClick={() => onScope(p.model)} className="grid w-full grid-cols-[minmax(0,1fr)_minmax(0,11rem)_4rem_4rem_4rem_4.5rem] items-center gap-3 border-t border-border px-[18px] py-[9px] text-left text-[13px] hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring">
                  <span className="truncate" title={p.model}>{p.model}</span>
                  <Meter value={p.inFlight} max={p.cap} strong={p.state === "full"} />
                  <span className="text-right font-mono text-xs tabular-nums">{p.inFlight}/{p.cap || "—"}</span>
                  <span className={cn("text-right font-mono text-xs tabular-nums", p.queued > 0 && "font-semibold text-foreground")}>{p.queued}</span>
                  <span className="text-right font-mono text-xs tabular-nums">{p.tenants}</span>
                  <span className="text-right">{p.state === "full" ? <Pill inverted>Full</Pill> : <span className={cn("text-xs", p.state === "busy" ? "text-foreground" : "text-muted-foreground")}>{STATE_LABEL[p.state]}</span>}</span>
                </button>
              ))}
            </div>
          </div>
          <div className="flex flex-wrap items-center justify-between gap-2 border-t border-border px-[18px] pb-4 pt-3">
            {pools.length > FIRST_POOLS
              ? <button type="button" onClick={onExpand} className="inline-flex items-center gap-1.5 text-[12.5px] text-secondary-foreground hover:text-foreground">{expanded ? "Show fewer" : `Show ${pools.length - FIRST_POOLS} more pools${idle ? ` · ${idle} idle` : ""}`}<ChevronDown className={cn("h-3.5 w-3.5 transition-transform", expanded && "rotate-180")} aria-hidden="true" /></button>
              : <span />}
            <span className="text-xs text-muted-foreground">Sorted by how full · Busy = over 80%</span>
          </div>
        </Panel>
        <GroupsCard raw={raw} />
      </div>

      <TenantsNow raw={raw} onOpen={onOpen} />

      {waitingTenants.length > 0 && <WaitingNow raw={raw} onOpen={onOpen} />}
    </>
  );
}

function GroupsCard({ raw }: { raw: FairshareLiveView }) {
  const total = raw.groups.reduce((n, g) => n + g.in_flight, 0);
  const shares = groupShares(raw.groups.map((g) => ({ name: g.name, weight: g.weight, active: g.in_flight > 0 || g.queued > 0 })));
  const ordered = [...raw.groups].sort((a, b) => b.in_flight - a.in_flight);
  return (
    <Panel title="Groups" subtitle="Share when everyone is busy, and what each group uses now" action={<PanelLink href="/fairshare/groups">Edit weights</PanelLink>}>
      <div className="flex flex-col gap-4 px-[18px] pb-4 pt-3">
        {ordered.length === 0 && <p className="text-[12.5px] text-muted-foreground">No group has work right now.</p>}
        {ordered.map((g, i) => {
          const share = shares.find((s) => s.name === g.name)?.share ?? 0;
          const using = total ? g.in_flight / total : 0;
          const tone = i < GROUP_TONES.length ? GROUP_TONES[i] : OTHER_TONE;
          const note = !g.in_flight && !g.queued ? "Idle" : using - share > 0.05 ? "Borrowing slots others are not using" : share - using > 0.05 ? (g.queued ? "Under its share and waiting" : "Under its share, with nothing waiting") : "About its share";
          return (
            <div key={g.name} className="space-y-1.5">
              <div className="flex justify-between gap-2 text-[13px]"><span className="inline-flex min-w-0 items-center gap-2"><span className="h-2.5 w-2.5 shrink-0 rounded-sm" style={{ background: tone }} /><span className="truncate">{g.name}</span><span className="text-muted-foreground">· weight {g.weight}</span></span><span className="font-mono text-xs">{Math.round(using * 100)}% <span className="text-muted-foreground">of {Math.round(share * 100)}%</span></span></div>
              <span className="relative block"><Meter value={using} max={1} className="h-2" /><span className="absolute -top-[3px] h-3.5 w-0.5 bg-foreground" style={{ left: `${Math.min(100, share * 100)}%` }} aria-hidden="true" /></span>
              <span className="text-xs text-muted-foreground">{note}</span>
            </div>
          );
        })}
        {ordered.length > 0 && <span className="flex items-center gap-2 text-[11.5px] text-muted-foreground"><span className="h-3 w-0.5 bg-foreground" aria-hidden="true" />marks the share its weight guarantees when every active group is busy</span>}
      </div>
    </Panel>
  );
}

function TenantsNow({ raw, onOpen }: { raw: FairshareLiveView; onOpen: (id: string) => void }) {
  const [all, setAll] = useState(false);
  const active = raw.tenants.filter((t) => t.in_flight > 0 || t.queued > 0).sort((a, b) => Number(isWaitingBelowShare(b)) - Number(isWaitingBelowShare(a)) || b.in_flight - a.in_flight || b.queued - a.queued);
  const top = active[0]?.in_flight ?? 0;
  const rows = all ? active : active.slice(0, 8);
  const cols = "grid-cols-[minmax(0,1.2fr)_minmax(0,6rem)_4rem_minmax(0,1.4fr)_4rem_3.5rem_minmax(0,1fr)]";
  return (
    <Panel title="Tenants right now" subtitle="Across every pool. Open a tenant to change its weight, see its keys, or set a cap." action={active.length > 8 ? <button type="button" onClick={() => setAll(!all)} className="text-[12.5px] text-secondary-foreground hover:text-foreground">{all ? "Show fewer" : `Show all ${active.length}`}</button> : undefined}>
      <div className="mt-3 overflow-x-auto pb-2">
        <div className="min-w-[44rem]">
          <div className={cn("grid gap-3 border-t border-border px-[18px] py-2 text-[11.5px] text-muted-foreground", cols)}><span>Tenant</span><span>Group</span><span className="text-right">Weight</span><span>Running</span><span className="text-right">Waiting</span><span className="text-right">Pools</span><span>Most used pool</span></div>
          {rows.length === 0 && <p className="border-t border-border px-[18px] py-6 text-center text-sm text-muted-foreground">No tenant has work running or waiting.</p>}
          {rows.map((t) => {
            const p = tenantPools(raw, t.tenant_id);
            return (
              <button key={t.tenant_id} type="button" onClick={() => onOpen(t.tenant_id)} className={cn("grid w-full items-center gap-3 border-t border-border px-[18px] py-[9px] text-left text-[13px] hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring", cols)}>
                <span className="flex min-w-0 items-center gap-2"><span className="truncate">{t.name || "Unnamed tenant"}</span>{isWaitingBelowShare(t) && <Pill inverted>Below share</Pill>}</span>
                <span className="truncate text-muted-foreground">{t.fairshare_group || "default"}</span>
                <span className="text-right font-mono text-xs">{t.weight}</span>
                <span className="flex items-center gap-2.5"><Meter value={t.in_flight} max={top} className="flex-1" /><span className="w-9 text-right font-mono text-xs tabular-nums">{t.in_flight}</span></span>
                <span className={cn("text-right font-mono text-xs tabular-nums", t.queued > 0 && "font-semibold text-foreground")}>{t.queued}</span>
                <span className="text-right font-mono text-xs tabular-nums">{p.count || "—"}</span>
                <span className="truncate text-muted-foreground">{p.top ?? "—"}</span>
              </button>
            );
          })}
        </div>
      </div>
    </Panel>
  );
}

function WaitingNow({ raw, onOpen }: { raw: FairshareLiveView; onOpen: (id: string) => void }) {
  const line = nextInLine(raw.tenants);
  return (
    <Panel title="Waiting now" subtitle="Tenants waiting below their fair share first, then everyone else in the order freed slots reach them">
      <ul className="mt-3 pb-2">
        {[...line].sort((a, b) => Number(isWaitingBelowShare(b)) - Number(isWaitingBelowShare(a))).map((t) => (
          <li key={t.tenant_id}>
            <button type="button" onClick={() => onOpen(t.tenant_id)} className="grid w-full grid-cols-[minmax(0,1fr)_auto_5rem] items-center gap-3 border-t border-border px-[18px] py-[9px] text-left text-[13px] hover:bg-accent/40">
              <span className="truncate">{t.name || "Unnamed tenant"}</span>
              {isWaitingBelowShare(t) ? <Pill inverted>Below share</Pill> : <Pill>At or above share</Pill>}
              <span className="text-right font-mono text-xs">{t.queued} waiting</span>
            </button>
          </li>
        ))}
      </ul>
    </Panel>
  );
}

// ---------------------------------------------------------------------------
// One pool
// ---------------------------------------------------------------------------

function PoolView({ view, raw, poolRow, shared, onOpen }: { view: FairshareLiveView; raw: FairshareLiveView; poolRow?: PoolRow; shared: boolean; onOpen: (id: string) => void }) {
  const rows = poolTenantRows(view.tenants);
  const cap = poolRow?.cap || view.max_in_flight;
  const running = poolRow?.inFlight ?? view.global_in_flight;
  const waiting = rows.filter((r) => r.tenant.queued > 0);
  const mostWaiting = [...waiting].sort((a, b) => b.tenant.queued - a.tenant.queued)[0];
  const below = rows.filter((r) => r.standing === "next");
  const above = rows.filter((r) => r.above > 0).sort((a, b) => b.above - a.above);
  const aboveTotal = above.reduce((n, r) => n + r.above, 0);
  const scale = Math.max(cap, ...rows.map((r) => r.tenant.in_flight + r.tenant.queued), 1);
  const names = new Map(view.tenants.map((t) => [t.tenant_id, t.name]));
  const keys = [...(view.keys ?? [])].filter((k) => k.in_flight > 0 || k.queued > 0).sort((a, b) => b.queued - a.queued || b.in_flight - a.in_flight);
  const groupWeights = view.groups.map((g) => `${g.name} ${g.weight}`).join(", ");

  return (
    <>
      <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
        <Tile label="Running" value={<>{formatNumber(running)} <span className="text-sm font-normal text-muted-foreground">of {formatNumber(cap)} slots</span></>} meter={[running, cap]} detail={running >= cap ? "Every slot is taken" : `${formatNumber(Math.max(0, cap - running))} free${shared ? " · cluster-wide" : ""}`} />
        <Tile label="Waiting" value={formatNumber(view.global_queued)} emphasis={view.global_queued > 0} detail={waiting.length ? `From ${waiting.length} tenant${waiting.length === 1 ? "" : "s"} · ${mostWaiting.tenant.queued} from ${mostWaiting.tenant.name}` : "Nothing waiting"} />
        <Tile label="Below fair share" value={`${below.length} tenant${below.length === 1 ? "" : "s"}`} detail={below.length ? `${below.map((r) => r.tenant.name).join(" and ")} ${below.length === 1 ? "is" : "are"} next in line` : "No one is short of their share"} />
        <Tile label="Above fair share" value={aboveTotal ? `+${aboveTotal}` : "0"} detail={above.length ? `${above[0].tenant.name}${above.length > 1 ? ` and ${above.length - 1} more` : ""}, borrowed while there was room` : "No one is over their share"} />
      </div>

      <Panel
        label="Who holds the slots"
        title={`Who holds this pool's ${formatNumber(cap)} slots`}
        subtitle="Waiting tenants below their share first. Open a tenant to change its weight or cap it on this model."
        action={
          <span className="flex flex-wrap items-center gap-3.5 text-[11.5px] text-muted-foreground">
            <span className="inline-flex items-center gap-1.5"><span className="h-2.5 w-3.5 rounded-sm bg-secondary-foreground" />Running</span>
            <span className="inline-flex items-center gap-1.5"><span className="h-2.5 w-3.5 rounded-sm [background:repeating-linear-gradient(135deg,hsl(240_5%_84%)_0_3px,transparent_3px_6px)]" />Above share</span>
            <span className="inline-flex items-center gap-1.5"><span className="h-2.5 w-3.5 rounded-sm border-[1.5px] border-dashed border-muted-foreground" />Waiting</span>
            <span className="inline-flex items-center gap-1.5"><span className="h-3.5 w-0.5 bg-foreground" />Fair share</span>
          </span>
        }
      >
        <ul className="mt-3.5">
          {rows.length === 0 && <li className="border-t border-border px-[18px] py-6 text-center text-sm text-muted-foreground">Nothing is running or waiting in this pool.</li>}
          {rows.map((r) => <TenantRow key={r.tenant.tenant_id} row={r} rows={rows} scale={scale} onOpen={onOpen} />)}
        </ul>
        <p className="px-[18px] pb-4 pt-1 text-xs text-muted-foreground">
          Bars are drawn against the pool&apos;s {formatNumber(cap)} slots.{groupWeights ? ` Fair shares split the pool by group weight (${groupWeights}), then by tenant weight inside each group.` : ""}
          {raw.replicas && raw.replicas > 1 && shared ? " Queues and shares are the answering gateway's view; slots are counted cluster-wide." : ""}
        </p>
      </Panel>

      {keys.length > 0 && (
        <Panel title="Keys in this pool" subtitle="Inside a tenant, slots are split between its keys by key weight">
          <div className="mt-3 overflow-x-auto pb-2">
            <div className="min-w-[40rem]">
              <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,10rem)_4.5rem_4.5rem_4.5rem_4.5rem] gap-3 border-t border-border px-[18px] py-2 text-[11.5px] text-muted-foreground"><span>Key</span><span>Tenant</span><span className="text-right">Weight</span><span className="text-right">Cap</span><span className="text-right">Running</span><span className="text-right">Waiting</span></div>
              {keys.slice(0, 12).map((k) => (
                <button key={k.key_id} type="button" onClick={() => onOpen(k.tenant_id)} className="grid w-full grid-cols-[minmax(0,1fr)_minmax(0,10rem)_4.5rem_4.5rem_4.5rem_4.5rem] items-center gap-3 border-t border-border px-[18px] py-[9px] text-left text-[13px] hover:bg-accent/40">
                  <span className="truncate">{k.name || k.key_id.slice(0, 8)}</span>
                  <span className="truncate text-muted-foreground">{names.get(k.tenant_id) ?? "—"}</span>
                  <span className="text-right font-mono text-xs">{k.weight}</span>
                  <span className="text-right font-mono text-xs text-muted-foreground">{k.max_in_flight ?? "—"}</span>
                  <span className="text-right font-mono text-xs">{k.in_flight}</span>
                  <span className={cn("text-right font-mono text-xs", k.queued > 0 && "font-semibold text-foreground")}>{k.queued}</span>
                </button>
              ))}
            </div>
          </div>
        </Panel>
      )}
    </>
  );
}

function TenantRow({ row, rows, scale, onOpen }: { row: PoolTenantRow; rows: PoolTenantRow[]; scale: number; onOpen: (id: string) => void }) {
  const t = row.tenant;
  const pct = (n: number) => `${Math.max(0, Math.min(100, (n / scale) * 100))}%`;
  const inShare = Math.min(t.in_flight, t.expected_slots);
  const over = Math.max(0, t.in_flight - t.expected_slots);
  const label = row.standing === "next" ? "Next in line" : row.standing === "waiting" ? "At share" : row.standing === "above" ? "Above share" : "Within share";
  return (
    <li className="grid gap-3 border-t border-border px-[18px] py-3.5 md:grid-cols-[minmax(0,12rem)_minmax(0,1fr)_minmax(0,16rem)_8.5rem] md:items-center md:gap-[18px]">
      <button type="button" onClick={() => onOpen(t.tenant_id)} className="flex min-w-0 flex-col gap-0.5 text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
        <span className="truncate text-[13.5px] font-medium hover:underline">{t.name || "Unnamed tenant"}</span>
        <span className="text-[11.5px] text-muted-foreground">{t.fairshare_group || "default"} · weight {t.weight}</span>
      </button>
      <span className="relative block h-3" aria-hidden="true">
        <span className="absolute inset-0 rounded-[3px] bg-muted/50" />
        <span className="absolute inset-y-0 left-0 rounded-[3px] bg-secondary-foreground" style={{ width: pct(inShare) }} />
        {over > 0 && <span className="absolute inset-y-0 rounded-r-[3px] [background:repeating-linear-gradient(135deg,hsl(240_5%_84%)_0_3px,transparent_3px_6px)]" style={{ left: pct(inShare), width: pct(over) }} />}
        {t.queued > 0 && <span className="absolute inset-y-0.5 rounded-sm border-[1.5px] border-dashed border-muted-foreground" style={{ left: `calc(${pct(t.in_flight)} + 3px)`, width: pct(t.queued) }} />}
        <span className="absolute -inset-y-1 w-0.5 bg-foreground" style={{ left: pct(t.expected_slots) }} />
      </span>
      <span className="text-[12.5px] text-secondary-foreground">
        <span className="font-mono text-xs text-foreground">{t.in_flight}</span> running · fair share <span className="font-mono text-xs text-foreground">{Math.round(t.expected_slots)}</span> · {t.queued > 0 ? <strong className={cn("font-semibold", row.standing === "next" && "text-foreground")}>{t.queued} waiting</strong> : row.above > 0 ? <strong className="font-semibold text-foreground">{row.above} above</strong> : "nothing waiting"}
      </span>
      <span className="flex items-center gap-1.5 md:justify-self-end">
        <Pill inverted={row.standing === "next"}>{label}</Pill>
        {t.queued > 0 && <WhyTip row={row} rows={rows} />}
      </span>
    </li>
  );
}

function WhyTip({ row, rows }: { row: PoolTenantRow; rows: PoolTenantRow[] }) {
  const { holders, line, place } = whyWaiting(row, rows);
  const name = row.tenant.name || "This tenant";
  return (
    <HelpTip label={`Why ${name} is waiting`} align="right">
      <span className="block"><strong className="font-medium text-foreground">Why {name} is waiting.</strong>{" "}
        {holders.length
          ? `${holders.map((h) => `${h.tenant.name} holds ${h.above} above its share`).join("; ")}, taken while the pool still had room. Running requests are never cut off, so those slots come back as its requests finish.`
          : "Every slot is held at or under its share: the pool is simply full."}
      </span>
      <span className="block">Each freed slot goes to whoever has had the least compared with their weight.{place ? ` ${name} is number ${place} in line.` : ""}</span>
      {line.length > 1 && <span className="block">Next in line: {line.slice(0, 4).map((r) => `${r.place} ${r.tenant.name} (${r.tenant.queued})`).join(", ")}.</span>}
    </HelpTip>
  );
}

