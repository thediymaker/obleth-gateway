"use client";

import { useTransition } from "react";
import { useQuery } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { clearLostReplicasAction } from "@/app/actions";
import type { ModelReplica } from "@/lib/obleth";
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

export function ReplicaPanel({ modelId }: { modelId: string }) {
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

  const replicas = data ?? [];
  const healthy = replicas.filter((r) => r.state === "healthy").length;
  const hasLost = replicas.some((r) => r.state === "lost");

  return (
    <Card className="overflow-hidden border-border bg-card/45 shadow-sm">
      <CardHeader className="flex flex-row items-center justify-between gap-3 space-y-0 border-b border-border/60 bg-background/35 px-4 py-3">
        <div className="min-w-0">
          <CardTitle>Replicas</CardTitle>
          <p className="mt-0.5 text-xs text-muted-foreground">
            {healthy} healthy / {replicas.length} total
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
        {replicas.length === 0 ? (
          <p className="m-4 rounded-md border border-dashed border-border/70 bg-background/25 px-3 py-4 text-sm text-muted-foreground">
            No attempts yet.
          </p>
        ) : (
          <ul className="divide-y divide-border/50">
            {replicas.map((replica) => (
              <AttemptRow key={replica.id} replica={replica} />
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}

function AttemptRow({ replica }: { replica: ModelReplica }) {
  const node = replica.nodes || "";
  const lastSeen = timeAgo(replica.updated_at) || "-";
  const message = replica.state === "lost" ? replica.last_message?.trim() : "";

  return (
    <li className="grid grid-cols-[minmax(0,1fr)_auto] gap-3 px-4 py-3">
      <div className="min-w-0">
        <div className="flex min-w-0 items-center gap-2">
          <span className={cn("h-1.5 w-1.5 shrink-0 rounded-full", DOT_STYLE[replica.state] ?? "bg-muted-foreground/55")} aria-hidden />
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
          seen {lastSeen}
          {message ? ` / ${message}` : ""}
        </p>
      </div>
      <StatePill state={replica.state} />
    </li>
  );
}

function StatePill({ state }: { state: string }) {
  return (
    <span className={cn("self-start rounded-sm border px-2 py-0.5 text-[10px] font-medium", STATE_STYLE[state] ?? "border-border bg-muted/25 text-muted-foreground")}>
      {formatState(state)}
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
