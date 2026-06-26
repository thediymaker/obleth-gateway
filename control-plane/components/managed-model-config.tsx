"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Check, ChevronDown, RefreshCw, Save, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { useClusterResources } from "@/lib/use-cluster-resources";
import type { ManagedModelSpec, PutManagedModel } from "@/lib/obleth";
import { cn } from "@/lib/utils";

type Message = { tone: "success" | "error"; text: string };

export function ManagedModelConfig({ modelId, onSaved }: { modelId: string; onSaved?: () => void }) {
  const qc = useQueryClient();
  const cluster = useClusterResources();
  const [msg, setMsg] = useState<Message | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
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
    onSuccess: () => {
      onSaved?.();
      qc.invalidateQueries({ queryKey: ["managed", modelId] });
      qc.invalidateQueries({ queryKey: ["replicas", modelId] });
    },
    onError: () => setMsg({ tone: "error", text: "Remove failed." }),
  });

  async function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (saving) return;
    setMsg(null);
    setSaved(false);
    setSaving(true);
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
    try {
      const r = await fetch(`/api/live/models/${modelId}/managed`, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      if (!r.ok) {
        setMsg({ tone: "error", text: "Save failed." });
        return;
      }
      setSaved(true);
      setMsg({ tone: "success", text: "Saved. The provisioner will relaunch replicas with these settings." });
      onSaved?.();
      qc.invalidateQueries({ queryKey: ["managed", modelId] });
      qc.invalidateQueries({ queryKey: ["replicas", modelId] });
      window.setTimeout(() => setSaved(false), 1800);
    } catch {
      setMsg({ tone: "error", text: "Save failed." });
    } finally {
      setSaving(false);
    }
  }

  if (isLoading || !data) return null;
  const d = data;

  const isRecipe =
    (d.launcher_spec as { source?: string } | null)?.source === "recipe" ||
    (!!d.script_body && d.script_body.trim() !== "");

  const saveLabel = saving ? "Saving..." : saved ? "Saved" : "Save provisioning";

  return (
    <form onSubmit={onSubmit} className="space-y-4">
      {/* Suggestions pulled live from the cluster; empty when slurm is unreachable. */}
      <datalist id="slurm-partitions" hidden>
        {cluster.partitions.map((p) => (
          <option key={p.name} value={p.name} />
        ))}
      </datalist>
      <datalist id="slurm-qos" hidden>
        {cluster.qos.map((q) => (
          <option key={q} value={q} />
        ))}
      </datalist>
      <datalist id="slurm-accounts" hidden>
        {cluster.accounts.map((a) => (
          <option key={a} value={a} />
        ))}
      </datalist>

      <section className="overflow-hidden rounded-lg border border-border bg-card/45 shadow-sm">
        <header className="flex flex-col gap-3 border-b border-border/60 bg-background/35 px-4 py-3 sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0">
            <p className="text-sm font-medium">Provisioning settings</p>
            <p className="text-xs text-muted-foreground">Slurm launch, placement, and replica targets.</p>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <Button type="submit" size="sm" disabled={saving || remove.isPending} aria-busy={saving}>
              {saving ? (
                <RefreshCw className="h-3.5 w-3.5 animate-spin" aria-hidden />
              ) : saved ? (
                <Check className="h-3.5 w-3.5" aria-hidden />
              ) : (
                <Save className="h-3.5 w-3.5" aria-hidden />
              )}
              {saveLabel}
            </Button>
          </div>
        </header>

        <div className="divide-y divide-border/60">
          <div className="p-4">
            <SectionTitle title="Launch script" detail={isRecipe ? "Recipe managed" : "Custom"} />
            <textarea
              name="slurm_script_body"
              defaultValue={d.script_body || d.launch_command || ""}
              rows={13}
              spellCheck={false}
              className="mt-3 min-h-72 w-full resize-y rounded-md border border-border/80 bg-background/80 p-3 font-mono text-[12px] leading-relaxed shadow-inner outline-none transition-colors placeholder:text-muted-foreground focus:border-primary/50 focus:ring-1 focus:ring-primary/35"
            />
          </div>

          <div className="grid divide-y divide-border/60 xl:grid-cols-[minmax(0,1fr)_minmax(300px,0.72fr)] xl:divide-x xl:divide-y-0">
            <FormSection title="Placement">
              <FieldCell label="Partition" htmlFor="slurm_partition">
                <Input id="slurm_partition" name="slurm_partition" defaultValue={d.partition} list="slurm-partitions" />
              </FieldCell>
              <FieldCell label="GRES" htmlFor="slurm_gres">
                <Input id="slurm_gres" name="slurm_gres" defaultValue={d.gres ?? ""} className="font-mono" />
              </FieldCell>
              <FieldCell label="CPUs per task" htmlFor="slurm_cpus_per_task">
                <Input id="slurm_cpus_per_task" name="slurm_cpus_per_task" defaultValue={d.cpus_per_task != null ? String(d.cpus_per_task) : ""} />
              </FieldCell>
              <FieldCell label="Memory" htmlFor="slurm_mem">
                <Input id="slurm_mem" name="slurm_mem" defaultValue={d.mem ?? ""} />
              </FieldCell>
              <FieldCell label="Nodes" htmlFor="slurm_nodes">
                <Input id="slurm_nodes" name="slurm_nodes" defaultValue={String(d.nodes ?? 1)} />
              </FieldCell>
              <FieldCell label="QoS" htmlFor="slurm_qos">
                <Input id="slurm_qos" name="slurm_qos" defaultValue={d.qos ?? ""} list="slurm-qos" />
              </FieldCell>
              <FieldCell label="Account" htmlFor="slurm_account">
                <Input id="slurm_account" name="slurm_account" defaultValue={d.account ?? ""} list="slurm-accounts" />
              </FieldCell>
              <FieldCell label="Time limit" htmlFor="slurm_time_limit">
                <Input id="slurm_time_limit" name="slurm_time_limit" defaultValue={d.time_limit ?? ""} />
              </FieldCell>
              <FieldCell label="Constraints" htmlFor="slurm_constraints">
                <Input id="slurm_constraints" name="slurm_constraints" defaultValue={d.constraints ?? ""} />
              </FieldCell>
              <FieldCell label="Exclude" htmlFor="slurm_exclude">
                <Input id="slurm_exclude" name="slurm_exclude" defaultValue={d.exclude ?? ""} />
              </FieldCell>
            </FormSection>

            <FormSection title="Service" columns={2}>
              <FieldCell label="Serving port" htmlFor="slurm_serving_port">
                <Input id="slurm_serving_port" name="slurm_serving_port" defaultValue={String(d.serving_port)} />
              </FieldCell>
              <FieldCell label="Health path" htmlFor="slurm_health_path">
                <Input id="slurm_health_path" name="slurm_health_path" defaultValue={d.health_path} className="font-mono" />
              </FieldCell>
              <FieldCell label="Min replicas" htmlFor="slurm_min_replicas">
                <Input id="slurm_min_replicas" name="slurm_min_replicas" defaultValue={String(d.min_replicas ?? 1)} />
              </FieldCell>
              <FieldCell label="Target replicas" htmlFor="slurm_target_replicas">
                <Input id="slurm_target_replicas" name="slurm_target_replicas" defaultValue={String(d.target_replicas ?? 2)} />
              </FieldCell>
              <FieldCell label="Max job failures" htmlFor="slurm_max_job_failures">
                <Input id="slurm_max_job_failures" name="slurm_max_job_failures" defaultValue={String(d.max_job_failures ?? 0)} />
              </FieldCell>
            </FormSection>
          </div>

          {!isRecipe && (
            <details className="group">
              <summary className="flex cursor-pointer list-none items-center justify-between gap-3 px-4 py-3 text-sm font-medium transition-colors hover:bg-muted/20">
                <span>Advanced</span>
                <ChevronDown className="h-4 w-4 text-muted-foreground transition-transform group-open:rotate-180" aria-hidden />
              </summary>
              <div className="grid gap-x-4 gap-y-3 px-4 pb-4 sm:grid-cols-2">
                <FieldCell label="Apptainer image" htmlFor="slurm_image">
                  <Input id="slurm_image" name="slurm_image" defaultValue={d.image ?? ""} className="font-mono" />
                </FieldCell>
                <FieldCell label="Log output dir" htmlFor="slurm_log_output_dir">
                  <Input id="slurm_log_output_dir" name="slurm_log_output_dir" defaultValue={d.log_output_dir ?? ""} className="font-mono" />
                </FieldCell>
                <div className="space-y-1.5 sm:col-span-2">
                  <Label htmlFor="slurm_preamble">Extra preamble</Label>
                  <textarea
                    id="slurm_preamble"
                    name="slurm_preamble"
                    defaultValue={d.preamble ?? ""}
                    rows={3}
                    className="w-full rounded-md border border-border/80 bg-background/80 p-3 font-mono text-xs leading-relaxed outline-none transition-colors focus:border-primary/50 focus:ring-1 focus:ring-primary/35"
                  />
                </div>
                <input type="hidden" name="slurm_launch_command" value={d.launch_command ?? ""} readOnly />
              </div>
            </details>
          )}
        </div>
      </section>

      {msg && (
        <p
          className={cn(
            "text-sm",
            msg.tone === "error" ? "text-destructive" : "text-emerald-500",
          )}
        >
          {msg.text}
        </p>
      )}

      {isRecipe && (
        <>
          <input type="hidden" name="slurm_image" value={d.image ?? ""} readOnly />
          <input type="hidden" name="slurm_preamble" value={d.preamble ?? ""} readOnly />
          <input type="hidden" name="slurm_log_output_dir" value={d.log_output_dir ?? ""} readOnly />
          <input type="hidden" name="slurm_launch_command" value={d.launch_command ?? ""} readOnly />
        </>
      )}

      {managed && (
        <section className="rounded-lg border border-destructive/25 bg-destructive/5 px-4 py-3">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
            <div className="min-w-0">
              <p className="text-sm font-medium text-destructive">Remove provisioning</p>
              <p className="text-xs text-muted-foreground">Keep the model route but stop Slurm-managed replacement.</p>
            </div>
            <Button type="button" size="sm" variant="destructive" onClick={() => remove.mutate()} disabled={remove.isPending || saving} aria-busy={remove.isPending}>
              {remove.isPending ? (
                <RefreshCw className="h-3.5 w-3.5 animate-spin" aria-hidden />
              ) : (
                <Trash2 className="h-3.5 w-3.5" aria-hidden />
              )}
              {remove.isPending ? "Removing..." : "Remove"}
            </Button>
          </div>
        </section>
      )}
    </form>
  );
}

function SectionTitle({ title, detail }: { title: string; detail?: string }) {
  return (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <p className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">{title}</p>
      {detail && (
        <span className="rounded-sm border border-border bg-background px-2 py-0.5 text-[10px] text-muted-foreground">
          {detail}
        </span>
      )}
    </div>
  );
}

function FormSection({
  title,
  columns = 2,
  children,
}: {
  title: string;
  columns?: 1 | 2;
  children: React.ReactNode;
}) {
  return (
    <div className="min-w-0 p-4">
      <SectionTitle title={title} />
      <div className={cn("mt-3 grid gap-x-4 gap-y-3", columns === 2 && "sm:grid-cols-2")}>{children}</div>
    </div>
  );
}

function FieldCell({
  label,
  htmlFor,
  children,
}: {
  label: string;
  htmlFor: string;
  children: React.ReactNode;
}) {
  return (
    <div className="min-w-0 space-y-1.5">
      <Label htmlFor={htmlFor}>{label}</Label>
      {children}
    </div>
  );
}
