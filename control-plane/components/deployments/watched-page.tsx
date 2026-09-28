"use client";

import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import { ChangesList, InFlightChart, useModelDay } from "@/components/deployments/detail-ui";
import { StateMark } from "@/components/deployments/ui";
import { useFairshareLive } from "@/components/fairshare/hooks";
import { Tile } from "@/components/models/ui";
import { Meter, Panel, Pill } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import type { DeploymentsData } from "@/lib/deployments-data";
import { buildDeploymentRows } from "@/lib/deployments-model";
import { logsHref } from "@/lib/log-links";
import { modelHref } from "@/lib/models-model";
import type { AuditEntry } from "@/lib/obleth";
import { compact } from "@/lib/overview-model";
import { getJson } from "@/lib/utils";

function secsAgo(iso: string | null | undefined): string {
  if (!iso) return "never";
  const s = Math.max(0, Math.round((Date.now() - new Date(iso).getTime()) / 1000));
  return s < 60 ? `${s} s ago` : s < 3600 ? `${Math.round(s / 60)} min ago` : `${Math.round(s / 3600)} h ago`;
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[130px_minmax(0,1fr)] items-center gap-3 border-t border-border py-2 text-[13px] first:border-t-0">
      <span className="text-muted-foreground">{label}</span>
      <span className="min-w-0 text-secondary-foreground">{children}</span>
    </div>
  );
}

/** A model a Kubernetes cluster runs: obleth routes to it and sizes its pool from the Service. */
export function WatchedPage({ modelId, initial, changes }: { modelId: string; initial: DeploymentsData; changes: AuditEntry[] }) {
  const live = useQuery({ queryKey: ["deployments"], queryFn: () => getJson<DeploymentsData>("/api/live/deployments"), initialData: initial, refetchInterval: 15_000 });
  const fairshare = useFairshareLive();
  const data = live.data ?? initial;
  const row = buildDeploymentRows(data.models, data.specs, data.replicas, data.discovery?.models ?? [], data.requests).find((r) => r.model.id === modelId);
  const model = row?.model ?? initial.models.find((m) => m.id === modelId)!;
  const day = useModelDay(model.model_name);
  const status = row?.discovery?.status;
  const inFlight = row?.discovery?.cluster_in_flight ?? row?.discovery?.in_flight ?? 0;
  const queued = fairshare.data?.model_queued?.[model.model_name] ?? 0;
  const cap = row?.poolCap ?? 0;
  const failShare = day.data?.requests ? day.data.errors / day.data.requests : 0;

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-5">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <p className="text-[12.5px] text-muted-foreground"><Link href="/deployments" className="text-secondary-foreground hover:text-foreground">Deployments</Link> / Kubernetes</p>
          <h1 className="truncate font-mono text-[24px] font-medium">{model.model_name}</h1>
          <div className="flex flex-wrap items-center gap-2">
            {row && <StateMark state={row.view.state} />}
            <Pill>{row?.ready ?? 0} replica{row?.ready === 1 ? "" : "s"} ready</Pill>
            <Pill>Kubernetes · watched</Pill>
            {status?.namespace && <Pill className="font-mono text-[11px]">{status.namespace} / {status.service}</Pill>}
            <span className="text-[12.5px] text-muted-foreground">the cluster starts and scales it; obleth routes to it</span>
          </div>
        </div>
        <div className="flex flex-wrap gap-2">
          <Button asChild variant="outline" size="sm" className="h-9"><Link href={logsHref({ model: model.model_name, window: "24h" })}>See its requests</Link></Button>
          <Button asChild variant="outline" size="sm" className="h-9"><Link href={modelHref(model.model_name)}>Model settings</Link></Button>
          {model.enabled && <Button asChild variant="outline" size="sm" className="h-9"><Link href={`/playground?model=${encodeURIComponent(model.model_name)}`}>Try in Playground</Link></Button>}
        </div>
      </div>

      {row?.view.attention && (
        <section className="rounded-xl border-[1.5px] border-foreground bg-card px-5 py-4">
          <p className="text-[15px] font-semibold">No replicas are ready</p>
          <p className="mt-1 max-w-3xl text-[13px] text-secondary-foreground">
            The Service {status?.service ? <span className="font-mono">{status.service}</span> : null} has no ready endpoints, so the pool is empty: requests wait for a slot, then fail with 503. Check the pods in {status?.namespace ?? "the cluster"}; obleth picks them up as soon as they&apos;re ready.
          </p>
        </section>
      )}

      <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
        <Tile label="Ready replicas" value={row?.ready ?? "—"} detail={status ? `read ${secsAgo(status.last_success)}` : "not seen by discovery"} />
        <div className="flex min-w-0 flex-col gap-1 rounded-xl border border-border bg-card px-4 py-3">
          <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Pool now</span>
          <span className="text-[24px] font-semibold leading-tight tabular-nums">{inFlight} <span className="text-[13px] font-normal text-muted-foreground">of {cap}</span></span>
          <Meter value={inFlight} max={cap} strong={cap > 0 && inFlight >= cap} />
          <span className="text-xs text-muted-foreground">{queued ? `${queued} waiting` : "none waiting"}</span>
        </div>
        <Tile label="Requests · 24h" value={day.data ? compact(day.data.requests) : "—"} href={day.data?.requests ? logsHref({ model: model.model_name, window: "24h" }) : undefined} detail={day.data?.requests ? "see them" : null} />
        <Tile
          label="Failed · 24h"
          value={day.data ? compact(day.data.errors) : "—"}
          emphasis={failShare >= 0.05}
          href={day.data?.errors ? logsHref({ model: model.model_name, window: "24h", status: "error" }) : undefined}
          detail={day.data?.errors ? `${failShare >= 0.1 ? Math.round(failShare * 100) : (failShare * 100).toFixed(1)}% of requests` : day.data ? "none" : null}
        />
      </div>

      <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_400px]">
        <InFlightChart model={model.model_name} cap={cap} />
        <Panel title="How obleth sizes it" action={<Link href={modelHref(model.model_name, "capacity")} className="text-[12.5px] text-secondary-foreground hover:text-foreground">Change ›</Link>}>
          <div className="px-[18px] pb-2 pt-1.5">
            <Row label="Service"><span className="font-mono text-[12px]">{status?.service ?? model.capacity_service ?? "—"}</span></Row>
            <Row label="Namespace"><span className="font-mono text-[12px]">{status?.namespace ?? model.capacity_namespace ?? (status?.namespaces ?? []).join(", ")}</span></Row>
            <Row label="Ready endpoints">{status?.ready_replicas ?? 0}, read {secsAgo(status?.last_success)}</Row>
            <Row label="Per replica">{status?.per_replica_max_in_flight ?? "differs by replica"} at once{status?.per_replica_source ? ` (${status.per_replica_source})` : ""}</Row>
            <Row label="Headroom">× {status?.headroom ?? model.capacity_headroom ?? 1}</Row>
            <Row label="Pool"><b className="font-medium text-foreground">{status?.ready_replicas != null && status.per_replica_max_in_flight != null ? `${status.ready_replicas} × ${status.per_replica_max_in_flight}${status.headroom !== 1 ? ` × ${status.headroom}` : ""} = ` : ""}{cap} slots</b></Row>
            {status?.reason && <Row label="Note">{status.reason}</Row>}
          </div>
          <p className="border-t border-border px-[18px] py-2.5 text-xs text-muted-foreground">If the Service has no ready endpoints the pool drops to 0 and requests wait, then fail with 503.</p>
        </Panel>
      </div>

      <ChangesList changes={changes} />
    </div>
  );
}
