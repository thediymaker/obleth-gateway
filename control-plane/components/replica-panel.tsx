"use client";

import { useTransition } from "react";
import { useQuery } from "@tanstack/react-query";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { clearLostReplicasAction } from "@/app/actions";
import type { ModelReplica } from "@/lib/obleth";

const STATE_COLOR: Record<string, string> = {
  healthy: "text-emerald-500",
  starting: "text-amber-500",
  pending: "text-amber-500",
  draining: "text-muted-foreground",
  lost: "text-destructive",
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
    <Card className="flex h-full min-h-0 flex-col">
      <CardHeader className="flex flex-row items-center justify-between">
        <CardTitle>
          Replicas{" "}
          <span className="text-sm text-muted-foreground">
            ({healthy} healthy)
          </span>
        </CardTitle>
        {hasLost && (
          <Button
            size="sm"
            variant="outline"
            disabled={isPending}
            onClick={() =>
              startTransition(async () => { await clearLostReplicasAction(modelId); })
            }
          >
            Retry failed
          </Button>
        )}
      </CardHeader>
      <CardContent className="min-h-0 flex flex-1 flex-col">
        {replicas.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            No replicas yet. The Slurm provisioner launches them on its next tick.
            If none appear, confirm the provisioner is running under{" "}
            <span className="font-medium">Settings → Slurm provisioning</span>.
          </p>
        ) : (
          <div className="min-h-0 flex-1 overflow-y-auto max-h-[calc(100dvh-22rem)] lg:max-h-none">
            <table className="w-full text-sm">
              <thead className="sticky top-0 bg-card">
                <tr className="text-left text-muted-foreground">
                  <th className="py-1">Job</th>
                  <th>Nodes</th>
                  <th>State</th>
                  <th>Message</th>
                </tr>
              </thead>
              <tbody>
                {replicas.map((r) => (
                  <tr key={r.id} className="border-t border-border/50">
                    <td className="py-1 font-mono">{r.slurm_job_id}</td>
                    <td className="font-mono">{r.nodes ?? "-"}</td>
                    <td className={STATE_COLOR[r.state] ?? ""}>{r.state}</td>
                    <td className="text-muted-foreground">{r.last_message ?? ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
