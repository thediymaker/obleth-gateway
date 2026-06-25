"use client";

import { useQuery } from "@tanstack/react-query";
import { AlertTriangle } from "lucide-react";
import type { ManagedModelSpec } from "@/lib/obleth";
import { timeAgo } from "@/lib/relative-time";

/** Shows the provisioner's last submit failure for a model (e.g. a Slurm
 *  account/partition rejection). Polls the managed spec so it clears itself
 *  once a submit succeeds. Renders nothing when there's no error. */
export function ProvisionErrorBanner({ modelId }: { modelId: string }) {
  const { data } = useQuery({
    queryKey: ["managed", modelId],
    refetchInterval: 5000,
    queryFn: async (): Promise<ManagedModelSpec | null> => {
      const r = await fetch(`/api/live/models/${modelId}/managed`);
      if (!r.ok) throw new Error("failed to load managed spec");
      return r.json();
    },
  });

  const err = data?.last_provision_error;
  if (!err) return null;

  const when = timeAgo(data?.last_provision_error_at ?? null);
  return (
    <div className="flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2.5 text-sm text-destructive">
      <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
      <div className="min-w-0">
        <p className="font-medium">
          Provisioning failed{when ? ` (${when})` : ""}
        </p>
        <p className="mt-0.5 break-words font-mono text-xs opacity-90">{err}</p>
        <p className="mt-1 text-xs opacity-80">
          The job was rejected before launch — no replica is created. Check the
          model&apos;s account / partition / QoS against the Slurm user&apos;s
          associations.
        </p>
      </div>
    </div>
  );
}
