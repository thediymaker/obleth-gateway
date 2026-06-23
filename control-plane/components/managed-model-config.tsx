"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import {
  SlurmLauncher,
  type LauncherSpec,
} from "@/components/slurm-launcher/slurm-launcher";
import type { ManagedModelSpec, PutManagedModel } from "@/lib/obleth";

function fallbackSpec(d: ManagedModelSpec): LauncherSpec {
  return {
    backendId: "custom",
    scriptBody: d.script_body || d.launch_command || "",
    resources: {
      partition: d.partition,
      gres: d.gres,
      cpusPerTask: d.cpus_per_task != null ? String(d.cpus_per_task) : "",
      mem: d.mem ?? "",
    },
    nodes: String(d.nodes ?? 1),
    replicas: String(d.target_replicas ?? 2),
    healthPath: d.health_path,
    port: String(d.serving_port),
    maxJobFailures: String(d.max_job_failures ?? 0),
    image: d.image ?? "",
    preamble: d.preamble ?? "",
    logOutputDir: d.log_output_dir ?? "",
    account: d.account ?? "",
    qos: d.qos ?? "",
    timeLimit: d.time_limit ?? "",
    constraints: d.constraints ?? "",
    exclude: d.exclude ?? "",
  };
}

export function ManagedModelConfig({ modelId }: { modelId: string }) {
  const qc = useQueryClient();
  const { data, isLoading } = useQuery({
    queryKey: ["managed", modelId],
    queryFn: async (): Promise<ManagedModelSpec | null> => {
      const r = await fetch(`/api/live/models/${modelId}/managed`);
      if (!r.ok) throw new Error("failed to load managed spec");
      return r.json();
    },
  });

  const managed = !!data;

  const remove = useMutation({
    mutationFn: async () => {
      const r = await fetch(`/api/live/models/${modelId}/managed`, {
        method: "DELETE",
      });
      if (!r.ok) throw new Error("delete failed");
    },
    onSuccess: () => qc.invalidateQueries({ queryKey: ["managed", modelId] }),
  });

  async function editSubmit(fd: FormData): Promise<{ ok: boolean; error?: string }> {
    const s = (k: string) => String(fd.get(k) ?? "");
    const sOrNull = (k: string) => {
      const v = s(k).trim();
      return v ? v : null;
    };
    const n = (k: string, d: number) => {
      const raw = s(k).trim();
      const v = Number(raw);
      return raw !== "" && Number.isFinite(v) ? v : d;
    };
    let launcher_spec: Record<string, unknown> | null = null;
    const rawSpec = s("slurm_launcher_spec");
    if (rawSpec) {
      try {
        launcher_spec = JSON.parse(rawSpec) as Record<string, unknown>;
      } catch {
        /* keep null */
      }
    }
    const body: PutManagedModel = {
      enabled: true,
      partition: s("slurm_partition"),
      gres: s("slurm_gres"),
      nodes: n("slurm_nodes", 1),
      constraints: sOrNull("slurm_constraints"),
      exclude: sOrNull("slurm_exclude"),
      account: sOrNull("slurm_account"),
      qos: sOrNull("slurm_qos"),
      time_limit: sOrNull("slurm_time_limit"),
      cpus_per_task: s("slurm_cpus_per_task").trim() ? n("slurm_cpus_per_task", 0) : null,
      mem: sOrNull("slurm_mem"),
      image: s("slurm_image"),
      preamble: s("slurm_preamble"),
      log_output_dir: s("slurm_log_output_dir"),
      launch_command: s("slurm_launch_command"),
      script_body: s("slurm_script_body"),
      serving_port: n("slurm_serving_port", 8000),
      health_path: s("slurm_health_path") || "/health",
      target_replicas: n("slurm_target_replicas", 2),
      max_job_failures: n("slurm_max_job_failures", 0),
      launcher_spec,
    };
    const r = await fetch(`/api/live/models/${modelId}/managed`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!r.ok) return { ok: false, error: "Save failed." };
    qc.invalidateQueries({ queryKey: ["managed", modelId] });
    return { ok: true };
  }

  if (isLoading) return null;

  const initialSpec: LauncherSpec | undefined = data
    ? data.launcher_spec
      ? (data.launcher_spec as unknown as LauncherSpec)
      : fallbackSpec(data)
    : undefined;

  return (
    <div className="space-y-3">
      <SlurmLauncher mode="edit" initialSpec={initialSpec} onSubmit={editSubmit} />
      {managed && (
        <Button
          variant="destructive"
          onClick={() => remove.mutate()}
          disabled={remove.isPending}
        >
          Remove provisioning
        </Button>
      )}
    </div>
  );
}
