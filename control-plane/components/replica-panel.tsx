"use client";

import { useQuery } from "@tanstack/react-query";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import type { ModelReplica } from "@/lib/obleth";

const STATE_COLOR: Record<string, string> = {
  healthy: "text-emerald-500",
  starting: "text-amber-500",
  pending: "text-amber-500",
  draining: "text-muted-foreground",
  lost: "text-destructive",
};

export function ReplicaPanel({ modelId }: { modelId: string }) {
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

  return (
    <Card className="flex h-full min-h-0 flex-col">
      <CardHeader>
        <CardTitle>
          Replicas{" "}
          <span className="text-sm text-muted-foreground">
            ({healthy} healthy)
          </span>
        </CardTitle>
      </CardHeader>
      <CardContent className="min-h-0 flex flex-1 flex-col">
        {replicas.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            No replicas. Enable provisioning to launch some.
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
                    <td className="text-muted-foreground">
                      {r.last_message ?? ""}
                    </td>
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
