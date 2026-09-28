"use client";

import { useEffect, useMemo, useState, useTransition } from "react";
import Link from "next/link";
import { logsHref } from "@/lib/log-links";
import { useQueryClient } from "@tanstack/react-query";
import { Minus, Plus, X } from "lucide-react";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import { Dialog, DialogClose, DialogDescription, DialogOverlay, DialogPortal, DialogTitle } from "@/components/ui/dialog";
import { setTenantMaxInFlightAction, setWeightAction } from "@/app/actions";
import type { FairshareLiveView, KeyFairshareView, TenantFairshareView } from "@/lib/obleth";
import { isWaitingBelowShare } from "@/lib/fairshare";
import { previewTenantWeight, tenantKeys, tenantPools } from "@/lib/fairshare-model";
import { Pill, SectionLabel, Sparkline } from "@/components/overview/ui";
import { cn } from "@/lib/utils";
import type { TenantSeriesRow } from "./hooks";

const slots = (n: number) => (n >= 10 ? Math.round(n) : Math.round(n * 10) / 10).toLocaleString();

/**
 * One tenant's fairshare: its weight (with what a change would do in every
 * pool it is active in), its per-model cap, and its keys.
 */
export function TenantPanel({ tenant, view, keys, series, onClose }: {
  tenant: TenantFairshareView | undefined;
  /** The whole snapshot, so the weight preview can see every pool. */
  view: FairshareLiveView | undefined;
  /** Keys in the current scope (all pools, or the open pool). */
  keys: KeyFairshareView[] | undefined;
  series: TenantSeriesRow[] | undefined;
  onClose: () => void;
}) {
  const open = !!tenant;
  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogPortal>
        <DialogOverlay />
        <DialogPrimitive.Content
          aria-label="Tenant details"
          className="fixed inset-y-0 right-0 z-50 flex w-full max-w-[520px] flex-col border-l border-border bg-card shadow-2xl focus:outline-none data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:slide-out-to-right data-[state=open]:slide-in-from-right"
        >
          {tenant && <Body key={tenant.tenant_id} tenant={tenant} view={view} keys={keys} series={series} />}
        </DialogPrimitive.Content>
      </DialogPortal>
    </Dialog>
  );
}

function Body({ tenant, view, keys, series }: { tenant: TenantFairshareView; view: FairshareLiveView | undefined; keys: KeyFairshareView[] | undefined; series: TenantSeriesRow[] | undefined }) {
  const queryClient = useQueryClient();
  const [draft, setDraft] = useState(String(tenant.weight));
  const [capDraft, setCapDraft] = useState(tenant.max_in_flight == null ? "" : String(tenant.max_in_flight));
  const [status, setStatus] = useState<{ kind: "weight" | "cap"; ok: boolean; text: string } | null>(null);
  const [pending, start] = useTransition();
  useEffect(() => { setCapDraft(tenant.max_in_flight == null ? "" : String(tenant.max_in_flight)); }, [tenant.max_in_flight]);

  const weight = Number(draft);
  const validWeight = Number.isInteger(weight) && weight >= 1;
  const changed = validWeight && weight !== tenant.weight;
  const preview = useMemo(() => (validWeight ? previewTenantWeight(view, tenant.tenant_id, weight) : []), [view, tenant.tenant_id, weight, validWeight]);
  const pools = tenantPools(view, tenant.tenant_id);
  const myKeys = tenantKeys(keys, tenant.tenant_id);
  const trend = useMemo(() => (series ?? []).filter((r) => r.tenant_id === tenant.tenant_id).sort((a, b) => a.bucket_ms - b.bucket_ms), [series, tenant.tenant_id]);
  const requests = trend.reduce((n, r) => n + Number(r.requests), 0);
  const moved = preview.filter((p) => Math.abs(p.to - p.from) >= 0.5);
  const alone = preview.filter((p) => p.groupPeers.length === 0).map((p) => p.model);
  const peers = [...new Set(moved.flatMap((p) => p.groupPeers))];

  const saveWeight = () => start(async () => {
    try {
      await setWeightAction(tenant.tenant_id, weight);
      setStatus({ kind: "weight", ok: true, text: `Weight saved: ${weight}.` });
      await queryClient.invalidateQueries({ queryKey: ["fairshare-live"] });
    } catch (e) {
      setStatus({ kind: "weight", ok: false, text: `Could not save weight: ${e instanceof Error ? e.message : String(e)}` });
    }
  });
  const capValue = capDraft.trim() === "" ? null : Number(capDraft);
  const validCap = capValue === null || (Number.isInteger(capValue) && capValue >= 1);
  const capChanged = validCap && capValue !== (tenant.max_in_flight ?? null);
  const saveCap = () => start(async () => {
    const result = await setTenantMaxInFlightAction(tenant.tenant_id, capValue);
    setStatus(result.ok ? { kind: "cap", ok: true, text: capValue === null ? "Cap removed." : `Cap saved: ${capValue} per model.` } : { kind: "cap", ok: false, text: `Could not save the cap: ${result.error}` });
    if (result.ok) await queryClient.invalidateQueries({ queryKey: ["fairshare-live"] });
  });

  return (
    <>
      <div className="flex flex-col gap-3 border-b border-border px-6 pb-4 pt-5">
        <div className="flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <DialogTitle className="truncate text-[17px] font-semibold">{tenant.name || "Unnamed tenant"}</DialogTitle>
            <DialogDescription className="text-[12.5px] text-muted-foreground">
              Group {tenant.fairshare_group || "default"} · {myKeys.length} key{myKeys.length === 1 ? "" : "s"} active · {pools.count} pool{pools.count === 1 ? "" : "s"}
            </DialogDescription>
          </div>
          <DialogClose className="flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-accent hover:text-foreground" aria-label="Close"><X className="h-4 w-4" /></DialogClose>
        </div>
        <div className="flex flex-wrap gap-2">
          {isWaitingBelowShare(tenant) ? <Pill inverted>{tenant.queued.toLocaleString()} waiting below share</Pill> : tenant.queued > 0 ? <Pill>{tenant.queued.toLocaleString()} waiting</Pill> : null}
          {!tenant.queued && <Pill>No queued requests</Pill>}
          <Pill>{tenant.in_flight.toLocaleString()} running</Pill>
          <Link href="/tenants" className="inline-flex h-[22px] items-center rounded-full border border-border px-2 text-[11.5px] font-medium hover:bg-accent">Open tenant ›</Link>
          <Link href={logsHref({ team: tenant.tenant_id, window: "1h" })} className="inline-flex h-[22px] items-center rounded-full border border-border px-2 text-[11.5px] font-medium hover:bg-accent">Its requests ›</Link>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-col gap-6 overflow-y-auto px-6 py-[18px]">
        <section className="space-y-2.5">
          <div className="flex items-baseline justify-between gap-3"><SectionLabel>Weight</SectionLabel><span className="text-xs text-muted-foreground">Relative to others in its group</span></div>
          <div className="flex items-center gap-2">
            <button type="button" aria-label="Decrease weight" disabled={!validWeight || weight <= 1} onClick={() => setDraft(String(Math.max(1, weight - (weight > 100 ? 50 : 10))))} className="flex h-9 w-9 items-center justify-center rounded-lg border border-border hover:bg-accent disabled:opacity-40"><Minus className="h-3.5 w-3.5" /></button>
            <input aria-label="Fairshare weight" inputMode="numeric" value={draft} onChange={(e) => setDraft(e.target.value.replace(/[^0-9]/g, ""))} className="h-9 w-24 rounded-lg border border-border bg-background text-center font-mono text-sm outline-none focus:ring-1 focus:ring-ring" />
            <button type="button" aria-label="Increase weight" onClick={() => setDraft(String((validWeight ? weight : tenant.weight) + (weight >= 100 ? 50 : 10)))} className="flex h-9 w-9 items-center justify-center rounded-lg border border-border hover:bg-accent"><Plus className="h-3.5 w-3.5" /></button>
            {changed && <span className="text-[12.5px] text-muted-foreground">was <span className="font-mono line-through">{tenant.weight}</span></span>}
          </div>
          {!validWeight && <p className="text-xs text-muted-foreground">Weight is a whole number of at least 1.</p>}
          {changed && (
            <div className="overflow-hidden rounded-[10px] border border-border">
              <div className="bg-background/60 px-3.5 py-2.5 text-[12.5px] text-secondary-foreground">Fair share if every tenant in the pool is busy, after this change:</div>
              {preview.length === 0 && <div className="border-t border-border px-3.5 py-2.5 text-[12.5px] text-muted-foreground">Not active in any pool right now, so nothing moves until it is.</div>}
              {preview.map((p) => (
                <div key={p.model} className="grid grid-cols-[minmax(0,1fr)_7rem_3.5rem] gap-2.5 border-t border-border px-3.5 py-2 text-[13px]">
                  <span className="truncate">{p.model} <span className="text-muted-foreground">· {p.cap.toLocaleString()} slots</span></span>
                  <span className="text-right font-mono text-xs"><span className="text-muted-foreground">{slots(p.from)} →</span> {slots(p.to)}</span>
                  <span className={cn("text-right font-mono text-xs", Math.abs(p.to - p.from) < 0.5 && "text-muted-foreground")}>{p.to - p.from >= 0 ? "+" : "−"}{slots(Math.abs(p.to - p.from))}</span>
                </div>
              ))}
              {preview.length > 0 && (
                <p className="border-t border-border px-3.5 py-2.5 text-[12.5px] leading-relaxed text-muted-foreground">
                  {peers.length > 0 && <>The difference comes from <strong className="font-medium text-foreground">{peers.join(", ")}</strong>, the other active tenant{peers.length === 1 ? "" : "s"} in {tenant.fairshare_group || "its group"}. </>}
                  {alone.length > 0 && <>Where it is its group&apos;s only tenant ({alone.join(", ")}), nothing moves. </>}
                  To take slots from other groups, <Link href="/fairshare/groups" className="underline underline-offset-[3px] hover:text-foreground">raise the group&apos;s weight</Link>.
                </p>
              )}
            </div>
          )}
          <div className="flex items-center gap-2">
            <button type="button" disabled={!changed || pending} onClick={saveWeight} aria-label="Apply weight" className="inline-flex h-9 items-center rounded-lg border border-foreground bg-foreground px-3.5 text-[13px] font-medium text-background hover:bg-foreground/90 disabled:opacity-40">Save weight</button>
            {changed && <button type="button" onClick={() => setDraft(String(tenant.weight))} className="inline-flex h-9 items-center rounded-lg border border-border px-3.5 text-[13px] hover:bg-accent">Reset</button>}
          </div>
          {status?.kind === "weight" && <p role={status.ok ? "status" : "alert"} className="text-xs text-secondary-foreground">{status.text}</p>}
        </section>

        <section className="space-y-2">
          <SectionLabel>Most it may run on any one model</SectionLabel>
          <div className="flex items-center gap-2">
            <input aria-label="Per-model cap" inputMode="numeric" placeholder="No limit" value={capDraft} onChange={(e) => setCapDraft(e.target.value.replace(/[^0-9]/g, ""))} className="h-9 w-32 rounded-lg border border-border bg-background px-2.5 font-mono text-sm outline-none placeholder:text-muted-foreground focus:ring-1 focus:ring-ring" />
            <button type="button" disabled={!capChanged || pending} onClick={saveCap} className="inline-flex h-9 items-center rounded-lg border border-border px-3.5 text-[13px] font-medium hover:bg-accent disabled:opacity-40">{capValue === null && tenant.max_in_flight != null ? "Remove cap" : "Save cap"}</button>
          </div>
          <p className="text-[12.5px] text-muted-foreground">Stops this tenant borrowing a whole pool. Requests already running are not cut off.</p>
          {status?.kind === "cap" && <p role={status.ok ? "status" : "alert"} className="text-xs text-secondary-foreground">{status.text}</p>}
        </section>

        <section className="space-y-1">
          <div className="flex items-baseline justify-between gap-3 pb-1"><SectionLabel>Keys</SectionLabel><span className="text-xs text-muted-foreground">Its share splits between keys by key weight</span></div>
          {myKeys.length === 0 ? <p className="text-[12.5px] text-muted-foreground">No active keys.</p> : (
            <>
              <div className="grid grid-cols-[minmax(0,1fr)_3.5rem_3.5rem_4rem_4rem] gap-2.5 py-1.5 text-[11.5px] text-muted-foreground"><span>Key</span><span className="text-right">Weight</span><span className="text-right">Cap</span><span className="text-right">Running</span><span className="text-right">Waiting</span></div>
              {myKeys.map((k) => (
                <div key={k.key_id} className="grid grid-cols-[minmax(0,1fr)_3.5rem_3.5rem_4rem_4rem] gap-2.5 border-t border-border py-2 text-[13px]">
                  <span className="truncate" title={k.name}>{k.name || k.key_id.slice(0, 8)}</span>
                  <span className="text-right font-mono text-xs">{k.weight}</span>
                  <span className="text-right font-mono text-xs text-muted-foreground">{k.max_in_flight ?? "—"}</span>
                  <span className="text-right font-mono text-xs">{k.in_flight}</span>
                  <span className="text-right font-mono text-xs">{k.queued}</span>
                </div>
              ))}
              <Link href="/keys" className="inline-block pt-1 text-xs text-muted-foreground underline-offset-[3px] hover:text-foreground hover:underline">Change key weights and caps on API Keys</Link>
            </>
          )}
        </section>

        <section className="space-y-2">
          <div className="flex justify-between"><SectionLabel>Requests · last 30 minutes</SectionLabel><span className="text-xs text-muted-foreground">{requests.toLocaleString()}</span></div>
          <Sparkline values={trend.map((r) => Number(r.requests))} className="h-14" label={`${tenant.name} requests over the last 30 minutes`} />
        </section>
      </div>
    </>
  );
}
