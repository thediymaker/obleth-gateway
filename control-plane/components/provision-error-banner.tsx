"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, X } from "lucide-react";
import type { ManagedModelSpec } from "@/lib/obleth";
import { timeAgo } from "@/lib/relative-time";

/** Shows the provisioner's last submit failure for a model (e.g. a Slurm
 *  account/partition rejection). Polls the managed spec so it clears itself
 *  once a submit succeeds. Renders nothing when there's no error. The payload
 *  is the full sbatch spec, so the poll stays coarse; submits take tens of
 *  seconds anyway. */
export function ProvisionErrorBanner({ modelId }: { modelId: string }) {
  const qc = useQueryClient();
  const { data } = useQuery({
    queryKey: ["managed", modelId],
    refetchInterval: 15_000,
    queryFn: async (): Promise<ManagedModelSpec | null> => {
      const r = await fetch(`/api/live/models/${modelId}/managed`);
      if (!r.ok) throw new Error("failed to load managed spec");
      return r.json();
    },
  });

  const dismiss = useMutation({
    mutationFn: async () => {
      const r = await fetch(`/api/live/models/${modelId}/managed/provision-error`, {
        method: "PATCH",
      });
      if (!r.ok) throw new Error("failed to clear provision error");
    },
    onSuccess: () => qc.invalidateQueries({ queryKey: ["managed", modelId] }),
  });

  const err = data?.last_provision_error;
  if (!err) return null;

  const when = timeAgo(data?.last_provision_error_at ?? null);
  return (
    <div className="flex items-start gap-2 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2.5 text-sm text-destructive">
      <AlertTriangle className="mt-0.5 h-4 w-4 shrink-0" />
      <div className="min-w-0 flex-1">
        <p className="font-medium">
          Provisioning failed{when ? ` (${when})` : ""}
        </p>
        <p className="mt-0.5 break-words font-mono text-xs opacity-90">{err}</p>
        <p className="mt-1 text-xs opacity-80">
          The job was rejected before launch — no replica is created. Check the
          model&apos;s account / partition / QoS against the Slurm user&apos;s
          associations. Fix the settings and save, or dismiss this notice. It
          returns if the next launch also fails.
        </p>
      </div>
      <button
        type="button"
        onClick={() => dismiss.mutate()}
        disabled={dismiss.isPending}
        aria-label="Dismiss provisioning error"
        title="Dismiss"
        className="shrink-0 rounded p-1 opacity-70 transition-colors hover:bg-destructive/15 hover:opacity-100 disabled:opacity-40"
      >
        <X className="h-4 w-4" />
      </button>
    </div>
  );
}
