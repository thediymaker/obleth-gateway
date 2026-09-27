"use client";

import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import { X } from "lucide-react";
import { Dialog, DialogClose, DialogDescription, DialogPortal, DialogOverlay, DialogTitle } from "@/components/ui/dialog";
import * as DialogPrimitive from "@radix-ui/react-dialog";
import type { AuditEntry, ModelHealthDetail, ModelRoute, ModelUsageTimePoint, UsageBreakdownEntry, UsageModelAgg } from "@/lib/obleth";
import { compact, describeAudit, formatMs, modelTypeLabel, RANGE_BUCKET_MS, RANGE_LABEL, RANGE_MS, type FleetTile, type OverviewRange } from "@/lib/overview-model";
import { cn, getJson } from "@/lib/utils";
import { HealthGlyph, Meter, Pill, SectionLabel, Sparkline } from "./ui";

const HEALTH_LABEL = { healthy: "Healthy", unhealthy: "Failing", unknown: "No health data", maintenance: "In maintenance" } as const;

/**
 * A model's recent story without leaving the Overview: load, traffic,
 * its last health checks, who uses it, and what changed.
 */
export function ModelPeek({ tile, model, usage, audit, range, onClose }: {
  tile: FleetTile | undefined;
  model: ModelRoute | undefined;
  usage: UsageModelAgg | undefined;
  audit: AuditEntry[];
  range: OverviewRange;
  onClose: () => void;
}) {
  const open = !!tile;
  const name = tile?.name ?? "";
  const since = Date.now() - RANGE_MS[range];
  const series = useQuery({
    queryKey: ["peek-series", name, range],
    queryFn: () => getJson<ModelUsageTimePoint[]>(`/api/live/usage/series/models?model=${encodeURIComponent(name)}&bucket_ms=${RANGE_BUCKET_MS[range]}&since_ms=${since}`),
    enabled: open,
  });
  const health = useQuery({
    queryKey: ["peek-health", tile?.id],
    queryFn: () => getJson<ModelHealthDetail>(`/api/live/models/${tile!.id}/health`),
    enabled: open,
  });
  const breakdown = useQuery({
    queryKey: ["peek-tenants", name, range],
    queryFn: () => getJson<UsageBreakdownEntry[]>(`/api/live/usage/breakdown?model=${encodeURIComponent(name)}&since_ms=${since}&limit=50`),
    enabled: open,
  });

  // The breakdown is per key; the peek speaks in tenants.
  const tenants = [...(breakdown.data ?? []).reduce((m, r) => m.set(r.tenant_name || "Unknown tenant", (m.get(r.tenant_name || "Unknown tenant") ?? 0) + Number(r.requests)), new Map<string, number>())]
    .sort((a, b) => b[1] - a[1]).slice(0, 4);
  const checks = [...(health.data?.checks ?? [])].sort((a, b) => a.checked_at.localeCompare(b.checked_at)).slice(-30);
  const lastCheck = checks.at(-1);
  const changes = audit.filter((e) => e.entity_type === "model" && e.entity_id === tile?.id).slice(0, 3);

  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) onClose(); }}>
      <DialogPortal>
        <DialogOverlay />
        <DialogPrimitive.Content
          className="fixed inset-y-0 right-0 z-50 flex w-full max-w-[480px] flex-col border-l border-border bg-card shadow-2xl focus:outline-none data-[state=open]:animate-in data-[state=closed]:animate-out data-[state=closed]:slide-out-to-right data-[state=open]:slide-in-from-right"
        >
          {tile && (
            <>
              <div className="flex flex-col gap-3.5 border-b border-border px-6 pb-4 pt-5">
                <div className="flex items-start gap-3">
                  <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-[10px] border border-border bg-secondary font-semibold" aria-hidden="true">{(name.match(/[a-z0-9]/i)?.[0] ?? "?").toUpperCase()}</span>
                  <div className="min-w-0 flex-1">
                    <DialogTitle className="truncate text-[17px] font-semibold">{name}</DialogTitle>
                    <DialogDescription className="text-[12.5px] text-muted-foreground">
                      {[modelTypeLabel(tile.type), model?.context_window ? `${compact(model.context_window)} context` : null, model?.tags.length ? `tags ${model.tags.join(", ")}` : null].filter(Boolean).join(" · ")}
                    </DialogDescription>
                  </div>
                  <DialogClose className="flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-accent hover:text-foreground" aria-label="Close"><X className="h-4 w-4" /></DialogClose>
                </div>
                <div className="flex flex-wrap gap-2">
                  <Pill inverted={tile.health === "unhealthy"}>{tile.health !== "unhealthy" && <HealthGlyph state={tile.health} className="h-[7px] w-[7px]" />}{HEALTH_LABEL[tile.health]}</Pill>
                  {model?.capacity_mode && model.capacity_mode !== "static" && <Pill>{model.capacity_mode === "discovered" ? "Discovered capacity" : "Tuned capacity"}</Pill>}
                  {model && !model.auto_eligible && <Pill>Not eligible for auto</Pill>}
                </div>
                <div className="flex flex-wrap gap-2">
                  <Link href={`/models?model=${encodeURIComponent(name)}`} className="inline-flex h-8 items-center rounded-lg border border-foreground bg-foreground px-3 text-[12.5px] font-medium text-background hover:bg-foreground/90">Open model</Link>
                  <Link href="/playground" className="inline-flex h-8 items-center rounded-lg border border-border px-3 text-[12.5px] font-medium hover:bg-accent">Try in Playground</Link>
                  <Link href="/logs" className="inline-flex h-8 items-center rounded-lg border border-border px-3 text-[12.5px] font-medium hover:bg-accent">Request logs</Link>
                </div>
              </div>

              <div className="flex min-h-0 flex-1 flex-col gap-[22px] overflow-y-auto px-6 py-[18px]">
                <section className="space-y-2">
                  <div className="flex justify-between"><SectionLabel>Load now</SectionLabel><span className="font-mono text-xs">{tile.cap ? `${tile.inFlight} of ${tile.cap}` : `${tile.inFlight} in flight`} · {tile.queued} queued</span></div>
                  <Meter value={tile.inFlight} max={tile.cap} className="h-2" strong={tile.attention === "full"} />
                </section>

                <section className="grid grid-cols-3 gap-x-3 gap-y-4">
                  <Stat value={compact(Number(usage?.requests ?? 0))} label="requests" />
                  <Stat value={compact(Number(usage?.total_tokens ?? 0))} label="tokens" />
                  <Stat value={compact(Number(usage?.users ?? 0))} label="users" />
                  <Stat value={formatMs(Number(usage?.p50_ttft_ms ?? 0))} label="first token · p50" />
                  <Stat value={usage?.gen_tokens_per_sec ? `${Math.round(Number(usage.gen_tokens_per_sec))} tok/s` : "—"} label="generation" />
                  <Stat value={formatMs(Number(usage?.p50_total_ms ?? 0))} label="total · p50" />
                </section>

                <section className="space-y-2">
                  <div className="flex justify-between"><SectionLabel>Requests · {RANGE_LABEL[range].toLowerCase()}</SectionLabel></div>
                  {series.isLoading ? <div className="h-16 animate-pulse rounded-lg bg-muted/40" /> : (
                    <Sparkline values={(series.data ?? []).map((p) => Number(p.requests))} className="h-16" label={`${name} requests over the ${RANGE_LABEL[range].toLowerCase()}`} />
                  )}
                </section>

                <section className="space-y-2">
                  <div className="flex justify-between"><SectionLabel>Health checks · last {checks.length || 30}</SectionLabel>{health.data?.summary.check_interval_secs ? <span className="text-xs text-muted-foreground">every {health.data.summary.check_interval_secs} s</span> : null}</div>
                  {checks.length === 0 ? <p className="text-[12.5px] text-muted-foreground">{health.isLoading ? "Loading…" : "No checks recorded yet."}</p> : (
                    <>
                      <div className="flex gap-[3px]" role="img" aria-label={`${checks.filter((c) => c.status === "healthy").length} of ${checks.length} recent checks passed`}>
                        {checks.map((c) => (
                          <span key={c.id} title={`${new Date(c.checked_at).toLocaleTimeString()} · ${c.status}${c.http_status ? ` · HTTP ${c.http_status}` : ""}${c.latency_ms ? ` · ${c.latency_ms} ms` : ""}`}
                            className={cn("block h-[18px] flex-1 rounded-sm", c.status === "healthy" ? ((c.latency_ms ?? 0) > 1000 ? "bg-muted-foreground" : "bg-secondary-foreground") : "border border-foreground bg-transparent")} />
                        ))}
                      </div>
                      <p className="text-xs text-muted-foreground">
                        {lastCheck ? `Last check ${new Date(lastCheck.checked_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}${lastCheck.http_status ? ` · HTTP ${lastCheck.http_status}` : ""}${lastCheck.latency_ms ? ` in ${lastCheck.latency_ms} ms` : ""}` : ""} · hollow bars failed, darker bars took over 1 s
                      </p>
                    </>
                  )}
                </section>

                <section className="space-y-2.5">
                  <SectionLabel>Top tenants on this model</SectionLabel>
                  {tenants.length === 0 ? <p className="text-[12.5px] text-muted-foreground">{breakdown.isLoading ? "Loading…" : "No traffic in this window."}</p> : tenants.map(([tenant, n]) => (
                    <div key={tenant} className="grid grid-cols-[minmax(0,9.5rem)_minmax(0,1fr)_3.5rem] items-center gap-3 text-[13px]">
                      <span className="truncate">{tenant}</span><Meter value={n} max={tenants[0][1]} className="h-1" /><span className="text-right font-mono text-xs">{compact(n)}</span>
                    </div>
                  ))}
                </section>

                {changes.length > 0 && (
                  <section className="space-y-2">
                    <SectionLabel>Recent changes</SectionLabel>
                    {changes.map((e) => {
                      const d = describeAudit(e, { models: new Map(), tenants: new Map() });
                      return <p key={e.id} className="text-[13px] text-secondary-foreground">{d.actor.replace(/@.*/, "")} {d.action} <span className="text-muted-foreground">· {new Date(e.ts).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}</span></p>;
                    })}
                  </section>
                )}
              </div>
            </>
          )}
        </DialogPrimitive.Content>
      </DialogPortal>
    </Dialog>
  );
}

function Stat({ value, label }: { value: string; label: string }) {
  return <div className="flex flex-col gap-0.5"><span className="text-lg font-semibold tabular-nums">{value}</span><span className="text-[11.5px] text-muted-foreground">{label}</span></div>;
}
