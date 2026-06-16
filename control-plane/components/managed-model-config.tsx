"use client";

import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import type { ManagedModelSpec, PutManagedModel } from "@/lib/obleth";

const EMPTY: PutManagedModel = {
  enabled: true,
  partition: "",
  gres: "",
  nodes: 1,
  image: "",
  preamble: "",
  log_output_dir: "",
  launch_command: "",
  serving_port: 8000,
  health_path: "/health",
  target_replicas: 2,
  max_job_failures: 0,
};

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

  const [form, setForm] = useState<PutManagedModel>(EMPTY);
  const [managed, setManaged] = useState(false);

  useEffect(() => {
    if (data) {
      setManaged(true);
      setForm({ ...data });
    } else if (data === null) {
      setManaged(false);
      setForm(EMPTY);
    }
  }, [data]);

  const save = useMutation({
    mutationFn: async (body: PutManagedModel) => {
      const r = await fetch(`/api/live/models/${modelId}/managed`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      if (!r.ok) throw new Error("save failed");
      return r.json();
    },
    onSuccess: () => qc.invalidateQueries({ queryKey: ["managed", modelId] }),
  });

  const remove = useMutation({
    mutationFn: async () => {
      const r = await fetch(`/api/live/models/${modelId}/managed`, {
        method: "DELETE",
      });
      if (!r.ok) throw new Error("delete failed");
    },
    onSuccess: () => qc.invalidateQueries({ queryKey: ["managed", modelId] }),
  });

  if (isLoading) return null;

  const field = (
    label: string,
    key: keyof PutManagedModel,
    placeholder = "",
    type: "text" | "number" = "text",
  ) => (
    <label className="flex flex-col gap-1 text-sm">
      <span className="text-muted-foreground">{label}</span>
      <Input
        type={type}
        placeholder={placeholder}
        value={(form[key] as string | number | undefined) ?? ""}
        onChange={(e) =>
          setForm({
            ...form,
            [key]: type === "number" ? Number(e.target.value) : e.target.value,
          })
        }
      />
    </label>
  );

  return (
    <Card>
      <CardHeader>
        <CardTitle>Slurm provisioning</CardTitle>
        <CardDescription>
          Host this model on the cluster. Keeps {form.target_replicas ?? 2}{" "}
          replicas alive and replaces preempted ones. Leave off for a static
          (manually-registered) model.
        </CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            className="h-4 w-4 rounded border-border accent-primary"
            checked={form.enabled ?? true}
            onChange={(e) => setForm({ ...form, enabled: e.target.checked })}
          />
          Provisioning enabled
        </label>
        <div className="grid grid-cols-2 gap-3">
          {field("Partition", "partition", "e.g. gpu-preempt")}
          {field("GRES", "gres", "e.g. gpu:h100:2")}
          {field("Nodes", "nodes", "1", "number")}
          {field("Target replicas", "target_replicas", "2", "number")}
          {field("Serving port", "serving_port", "8000", "number")}
          {field("Health path", "health_path", "/health")}
          {field("Time limit", "time_limit", "e.g. 12:00:00")}
          {field("Max job failures", "max_job_failures", "0 = unlimited", "number")}
          {field("Account", "account", "optional")}
          {field("QOS", "qos", "optional")}
          {field("Constraints", "constraints", "optional")}
          {field("Log output dir", "log_output_dir", "e.g. /shared/logs (empty = Slurm default)")}
        </div>
        {field("Apptainer image", "image", "e.g. /shared/images/vllm.sif")}
        <label className="flex flex-col gap-1 text-sm">
          <span className="text-muted-foreground">
            Preamble{" "}
            <span className="text-xs text-muted-foreground/60">
              (shell lines before apptainer exec, e.g. module load apptainer)
            </span>
          </span>
          <textarea
            rows={3}
            placeholder={"module load apptainer/1.3.4"}
            value={form.preamble ?? ""}
            onChange={(e) => setForm({ ...form, preamble: e.target.value })}
            className="flex w-full rounded-md border border-border bg-background px-3 py-2 text-sm shadow-sm transition-colors placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50 font-mono resize-y"
          />
        </label>
        {field(
          "Launch command",
          "launch_command",
          "e.g. vllm serve <model> --port 8000",
        )}
        <div className="flex gap-2">
          <Button onClick={() => save.mutate(form)} disabled={save.isPending}>
            {managed ? "Save" : "Enable provisioning"}
          </Button>
          {managed && (
            <Button
              variant="destructive"
              onClick={() => remove.mutate()}
              disabled={remove.isPending}
            >
              Remove
            </Button>
          )}
        </div>
        {save.isError && (
          <p className="text-sm text-destructive">Save failed.</p>
        )}
      </CardContent>
    </Card>
  );
}
