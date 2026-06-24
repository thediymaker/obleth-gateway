"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useClusterResources } from "@/lib/use-cluster-resources";
import type { ManagedModelSpec, PutManagedModel } from "@/lib/obleth";

export function ManagedModelConfig({ modelId }: { modelId: string }) {
  const qc = useQueryClient();
  const cluster = useClusterResources();
  const [msg, setMsg] = useState<string | null>(null);
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
      const r = await fetch(`/api/live/models/${modelId}/managed`, { method: "DELETE" });
      if (!r.ok) throw new Error("delete failed");
    },
    onSuccess: () => qc.invalidateQueries({ queryKey: ["managed", modelId] }),
  });

  async function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setMsg(null);
    const fd = new FormData(e.currentTarget);
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
      min_replicas: n("slurm_min_replicas", 1),
      target_replicas: n("slurm_target_replicas", 2),
      max_job_failures: n("slurm_max_job_failures", 0),
      launcher_spec: data?.launcher_spec ?? null,
    };
    const r = await fetch(`/api/live/models/${modelId}/managed`, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!r.ok) {
      setMsg("Save failed.");
      return;
    }
    setMsg("Saved. The provisioner will relaunch replicas with these settings.");
    qc.invalidateQueries({ queryKey: ["managed", modelId] });
  }

  if (isLoading || !data) return null;
  const d = data;

  const isRecipe =
    (d.launcher_spec as { source?: string } | null)?.source === "recipe" ||
    (!!d.script_body && d.script_body.trim() !== "");

  return (
    <form onSubmit={onSubmit} className="space-y-5">
      {/* Suggestions pulled live from the cluster; empty when slurm is unreachable. */}
      <datalist id="slurm-partitions">
        {cluster.partitions.map((p) => (
          <option key={p.name} value={p.name} />
        ))}
      </datalist>
      <datalist id="slurm-qos">
        {cluster.qos.map((q) => (
          <option key={q} value={q} />
        ))}
      </datalist>
      <datalist id="slurm-accounts">
        {cluster.accounts.map((a) => (
          <option key={a} value={a} />
        ))}
      </datalist>
      <div>
        <p className="text-sm font-medium">Launch script</p>
        <p className="text-xs text-muted-foreground">The full sbatch body submitted to slurmrestd.</p>
        <textarea
          name="slurm_script_body"
          defaultValue={d.script_body || d.launch_command || ""}
          rows={12}
          className="mt-2 w-full rounded-md border border-border bg-background p-2 font-mono text-xs"
        />
      </div>

      <div>
        <p className="text-sm font-medium">Placement</p>
        <div className="mt-2 grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          <div className="space-y-1.5">
            <Label htmlFor="slurm_partition">Partition</Label>
            <Input id="slurm_partition" name="slurm_partition" defaultValue={d.partition} list="slurm-partitions" />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_gres">GRES</Label>
            <Input id="slurm_gres" name="slurm_gres" defaultValue={d.gres ?? ""} className="font-mono" />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_cpus_per_task">CPUs per task</Label>
            <Input id="slurm_cpus_per_task" name="slurm_cpus_per_task" defaultValue={d.cpus_per_task != null ? String(d.cpus_per_task) : ""} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_mem">Memory</Label>
            <Input id="slurm_mem" name="slurm_mem" defaultValue={d.mem ?? ""} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_nodes">Nodes</Label>
            <Input id="slurm_nodes" name="slurm_nodes" defaultValue={String(d.nodes ?? 1)} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_qos">QoS</Label>
            <Input id="slurm_qos" name="slurm_qos" defaultValue={d.qos ?? ""} list="slurm-qos" />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_account">Account</Label>
            <Input id="slurm_account" name="slurm_account" defaultValue={d.account ?? ""} list="slurm-accounts" />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_time_limit">Time limit</Label>
            <Input id="slurm_time_limit" name="slurm_time_limit" defaultValue={d.time_limit ?? ""} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_constraints">Constraints</Label>
            <Input id="slurm_constraints" name="slurm_constraints" defaultValue={d.constraints ?? ""} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_exclude">Exclude</Label>
            <Input id="slurm_exclude" name="slurm_exclude" defaultValue={d.exclude ?? ""} />
          </div>
        </div>
      </div>

      <div>
        <p className="text-sm font-medium">Service</p>
        <div className="mt-2 grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          <div className="space-y-1.5">
            <Label htmlFor="slurm_serving_port">Serving port</Label>
            <Input id="slurm_serving_port" name="slurm_serving_port" defaultValue={String(d.serving_port)} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_health_path">Health path</Label>
            <Input id="slurm_health_path" name="slurm_health_path" defaultValue={d.health_path} className="font-mono" />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_min_replicas">Min replicas</Label>
            <Input id="slurm_min_replicas" name="slurm_min_replicas" defaultValue={String(d.min_replicas ?? 1)} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_target_replicas">Target replicas</Label>
            <Input id="slurm_target_replicas" name="slurm_target_replicas" defaultValue={String(d.target_replicas ?? 2)} />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_max_job_failures">Max job failures</Label>
            <Input id="slurm_max_job_failures" name="slurm_max_job_failures" defaultValue={String(d.max_job_failures ?? 0)} />
          </div>
        </div>
      </div>

      {!isRecipe && (
        <details className="rounded-md border border-border/70 bg-background/35 p-3">
          <summary className="cursor-pointer text-sm text-muted-foreground">Advanced</summary>
          <div className="mt-3 grid gap-3 sm:grid-cols-2">
            <div className="space-y-1.5">
              <Label htmlFor="slurm_image">Apptainer image</Label>
              <Input id="slurm_image" name="slurm_image" defaultValue={d.image ?? ""} className="font-mono" />
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="slurm_log_output_dir">Log output dir</Label>
              <Input id="slurm_log_output_dir" name="slurm_log_output_dir" defaultValue={d.log_output_dir ?? ""} className="font-mono" />
            </div>
            <div className="space-y-1.5 sm:col-span-2">
              <Label htmlFor="slurm_preamble">Extra preamble</Label>
              <textarea
                id="slurm_preamble"
                name="slurm_preamble"
                defaultValue={d.preamble ?? ""}
                rows={3}
                className="w-full rounded-md border border-border bg-background p-2 font-mono text-xs"
              />
            </div>
            <input type="hidden" name="slurm_launch_command" value={d.launch_command ?? ""} readOnly />
          </div>
        </details>
      )}

      {msg && <p className="text-sm text-muted-foreground">{msg}</p>}

      {isRecipe && (
        <>
          <input type="hidden" name="slurm_image" value={d.image ?? ""} readOnly />
          <input type="hidden" name="slurm_preamble" value={d.preamble ?? ""} readOnly />
          <input type="hidden" name="slurm_log_output_dir" value={d.log_output_dir ?? ""} readOnly />
          <input type="hidden" name="slurm_launch_command" value={d.launch_command ?? ""} readOnly />
        </>
      )}

      <div className="flex gap-2">
        <Button type="submit">Save</Button>
        {managed && (
          <Button type="button" variant="destructive" onClick={() => remove.mutate()} disabled={remove.isPending}>
            Remove provisioning
          </Button>
        )}
      </div>
    </form>
  );
}
