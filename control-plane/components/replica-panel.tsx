"use client";

import { useTransition } from "react";
import { useQuery } from "@tanstack/react-query";
import { AlertTriangle, RefreshCw } from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { clearLostReplicasAction } from "@/app/actions";
import type { ModelReplica, SlurmSettingsView } from "@/lib/obleth";
import { cn } from "@/lib/utils";
import { timeAgo } from "@/lib/relative-time";

const STATE_STYLE: Record<string, string> = {
  healthy: "border-emerald-500/30 bg-emerald-500/10 text-emerald-400",
  starting: "border-amber-500/30 bg-amber-500/10 text-amber-400",
  pending: "border-amber-500/30 bg-amber-500/10 text-amber-400",
  draining: "border-border bg-muted/25 text-muted-foreground",
  lost: "border-destructive/35 bg-destructive/10 text-destructive",
};

const DOT_STYLE: Record<string, string> = {
  healthy: "bg-emerald-400",
  starting: "bg-amber-400",
  pending: "bg-amber-400",
  draining: "bg-muted-foreground/55",
  lost: "bg-destructive",
};

// The muted look every pill/dot degrades to while replica state is frozen
// (provisioner down or reconcile held): stored states may no longer be true.
const STALE_PILL = "border-border bg-muted/25 text-muted-foreground";
const STALE_DOT = "bg-muted-foreground/55";

function fmtSecs(secs: number | null | undefined): string {
  if (secs == null) return "never";
  if (secs < 90) return `${Math.max(secs, 0)}s ago`;
  if (secs < 5400) return `${Math.round(secs / 60)}m ago`;
  if (secs < 129600) return `${Math.round(secs / 3600)}h ago`;
  return `${Math.round(secs / 86400)}d ago`;
}

// Why the replica list can't be trusted right now, or null when it can.
function staleness(slurm: SlurmSettingsView | undefined): string | null {
  if (!slurm) return null;
  if (!slurm.enabled) {
    return "Slurm provisioning is disabled in Settings — these replicas are not being reconciled (jobs are not cancelled by disabling).";
  }
  if (!slurm.provisioner_running) {
    return `Provisioner not running (last polled ${fmtSecs(slurm.provisioner_last_seen_secs)}) — replica states below are frozen at their last reconciled values.`;
  }
  const held = slurm.provisioner_held_secs ?? 0;
  if (slurm.provisioner_tick_status && slurm.provisioner_tick_status !== "ok" && held > 60) {
    const detail = slurm.provisioner_tick_detail ? ` (${slurm.provisioner_tick_detail})` : "";
    return `Provisioner is running but can't reconcile with Slurm — failing for ${fmtSecs(held).replace(" ago", "")}${detail}. Replica states below are frozen; last successful reconcile ${fmtSecs(slurm.provisioner_last_ok_secs)}.`;
  }
  return null;
}

export function ReplicaPanel({ modelId, healthStatus }: { modelId: string; healthStatus?: string }) {
  const [isPending, startTransition] = useTransition();
  const { data } = useQuery({
    queryKey: ["replicas", modelId],
    refetchInterval: 5000,
    queryFn: async (): Promise<ModelReplica[]> => {
      const r = await fetch(`/api/live/models/${modelId}/replicas`);
      if (!r.ok) throw new Error("failed to load replicas");
      return r.json();
    },
  });
  const { data: slurm } = useQuery({
    queryKey: ["slurm-status"],
    refetchInterval: 15000,
    queryFn: async (): Promise<SlurmSettingsView> => {
      const r = await fetch(`/api/live/slurm/status`);
      if (!r.ok) throw new Error("failed to load slurm status");
      return r.json();
    },
  });

  const replicas = data ?? [];
  const healthy = replicas.filter((r) => r.state === "healthy").length;
  const hasLost = replicas.some((r) => r.state === "lost");
  const staleReason = replicas.length > 0 ? staleness(slurm) : null;
  // The gateway's endpoint probes disagree with the (live) provisioner state:
  // Slurm says the jobs run, the gateway can't reach what's inside them.
  const conflict =
    !staleReason &&
    replicas.length > 0 &&
    healthy === replicas.length &&
    (healthStatus === "unhealthy" || healthStatus === "degraded");

  return (
    <Card className="overflow-hidden border-border bg-card/45 shadow-sm">
      <CardHeader className="flex flex-row items-center justify-between gap-3 space-y-0 border-b border-border/60 bg-background/35 px-4 py-3">
        <div className="min-w-0">
          <CardTitle>Replicas</CardTitle>
          <p className="mt-0.5 text-xs text-muted-foreground">
            {staleReason ? `${healthy} healthy / ${replicas.length} total (stale)` : `${healthy} healthy / ${replicas.length} total`}
          </p>
        </div>
        {hasLost && (
          <Button
            size="sm"
            variant="outline"
            disabled={isPending}
            onClick={() =>
              startTransition(async () => {
                await clearLostReplicasAction(modelId);
              })
            }
          >
            <RefreshCw className={cn("h-3.5 w-3.5", isPending && "animate-spin")} aria-hidden />
            Retry
          </Button>
        )}
      </CardHeader>
      <CardContent className="px-0 py-0">
        {staleReason && (
          <p className="flex items-start gap-2 border-b border-amber-500/30 bg-amber-500/10 px-4 py-2.5 text-xs text-amber-600 dark:text-amber-400">
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" aria-hidden />
            <span>{staleReason}</span>
          </p>
        )}
        {conflict && (
          <p className="flex items-start gap-2 border-b border-amber-500/30 bg-amber-500/10 px-4 py-2.5 text-xs text-amber-600 dark:text-amber-400">
            <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0" aria-hidden />
            <span>
              Gateway health probes report this model {healthStatus}, but Slurm still reports these
              jobs running. If the probes keep failing, the provisioner restarts the affected
              replicas automatically.
            </span>
          </p>
        )}
        {replicas.length === 0 ? (
          <p className="m-4 rounded-md border border-dashed border-border/70 bg-background/25 px-3 py-4 text-sm text-muted-foreground">
            No attempts yet.
          </p>
        ) : (
          <ul className="divide-y divide-border/50">
            {replicas.map((replica) => (
              <AttemptRow key={replica.id} replica={replica} stale={Boolean(staleReason)} />
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}

function AttemptRow({ replica, stale }: { replica: ModelReplica; stale: boolean }) {
  const node = replica.nodes || "";
  const lastSeen = timeAgo(replica.updated_at) || "-";
  const message = replica.state === "lost" ? replica.last_message?.trim() : "";

  return (
    <li className="grid grid-cols-[minmax(0,1fr)_auto] gap-3 px-4 py-3">
      <div className="min-w-0">
        <div className="flex min-w-0 items-center gap-2">
          <span
            className={cn(
              "h-1.5 w-1.5 shrink-0 rounded-full",
              stale ? STALE_DOT : (DOT_STYLE[replica.state] ?? "bg-muted-foreground/55"),
            )}
            aria-hidden
          />
          <span className="truncate font-mono text-xs text-foreground" title={replica.slurm_job_id}>
            job {replica.slurm_job_id}
          </span>
          <span className="min-w-0 truncate text-[11px] text-muted-foreground" title={node || "No node assigned yet"}>
            {node ? (
              <>
                on <span className="font-mono text-foreground/90">{node}</span>
              </>
            ) : (
              "waiting for node"
            )}
          </span>
        </div>
        <p className="mt-1 truncate pl-3.5 text-[11px] text-muted-foreground">
          updated {lastSeen}
          {message ? ` / ${message}` : ""}
        </p>
      </div>
      <StatePill state={replica.state} stale={stale} />
    </li>
  );
}

function StatePill({ state, stale }: { state: string; stale: boolean }) {
  return (
    <span
      className={cn(
        "self-start rounded-sm border px-2 py-0.5 text-[10px] font-medium",
        stale ? STALE_PILL : (STATE_STYLE[state] ?? "border-border bg-muted/25 text-muted-foreground"),
      )}
      title={stale ? "Last reconciled state — the provisioner has not been able to refresh it" : undefined}
    >
      {stale ? `${formatState(state)}?` : formatState(state)}
    </span>
  );
}

function formatState(state: string) {
  return state
    .split(/[_\s-]+/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(" ");
}
