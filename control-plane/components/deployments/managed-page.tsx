"use client";

import { useCallback, useEffect, useMemo, useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Minus, MoreHorizontal, Plus } from "lucide-react";
import {
  clearLostReplicasAction,
  removeDeploymentAction,
  restartReplicaAction,
  saveDeploymentSettingsAction,
  setDeploymentEnabledAction,
  setDeploymentReplicasAction,
  saveRecipeFromDeploymentAction,
} from "@/app/actions";
import { SettingsForm } from "@/components/access/settings-form";
import { SettingsCard } from "@/components/access/ui";
import { ChangesList, InFlightChart, useModelDay } from "@/components/deployments/detail-ui";
import { Glyph, StateMark } from "@/components/deployments/ui";
import { useFairshareLive } from "@/components/fairshare/hooks";
import { Setting, TextArea, TextField } from "@/components/models/fields";
import { Notice, Tile } from "@/components/models/ui";
import { Meter, Pill } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Select } from "@/components/ui/select";
import type { DeploymentsData } from "@/lib/deployments-data";
import {
  buildDeploymentRows,
  countReplicas,
  duration,
  gresCount,
  memToMb,
  parseTimeLimit,
  partitionFits,
  replicaPhase,
  slurmErrorText,
  slurmStatus,
  walltimeChoices,
  walltimeLabel,
  formatTimeLimit,
  type ReplicaPhase,
} from "@/lib/deployments-model";
import { logsHref } from "@/lib/log-links";
import { modelHref } from "@/lib/models-model";
import type { AuditEntry, ManagedModelSpec, ModelReplica } from "@/lib/obleth";
import { compact } from "@/lib/overview-model";
import { useClusterResources } from "@/lib/use-cluster-resources";
import { cn, getJson } from "@/lib/utils";

type Card = "script" | "placement" | "service";

function cardOf(name: string): Card | null {
  if (name === "slurm_script_body") return "script";
  if (["slurm_partition", "slurm_gres", "slurm_cpus_per_task", "slurm_mem", "slurm_nodes", "slurm_account", "slurm_qos", "slurm_time_limit", "slurm_constraints", "slurm_exclude"].includes(name)) return "placement";
  if (["slurm_serving_port", "slurm_health_path", "slurm_min_replicas", "slurm_max_job_failures"].includes(name)) return "service";
  return null;
}

const LABELS: Record<string, string> = {
  slurm_script_body: "Job script",
  slurm_partition: "Partition",
  slurm_gres: "GPUs",
  slurm_cpus_per_task: "CPUs",
  slurm_mem: "Memory",
  slurm_nodes: "Nodes",
  slurm_account: "Account",
  slurm_qos: "QoS",
  slurm_time_limit: "Walltime",
  slurm_constraints: "Constraints",
  slurm_exclude: "Excluded nodes",
  slurm_serving_port: "Port",
  slurm_health_path: "Health path",
  slurm_min_replicas: "Serve from",
  slurm_max_job_failures: "Stop after",
};

const STEPS: { phase: ReplicaPhase; label: string }[] = [
  { phase: "queued", label: "Queued" },
  { phase: "running", label: "Loading" },
  { phase: "serving", label: "Serving" },
];

function ReplicaCard({ r, letter, onRestart, pending }: { r: ModelReplica; letter: string; onRestart: () => void; pending: boolean }) {
  const phase = replicaPhase(r);
  const at = STEPS.findIndex((s) => s.phase === phase);
  const slurm = slurmStatus(r.last_message);
  const since = Date.now() - new Date(phase === "serving" ? r.updated_at : r.created_at).getTime();
  return (
    <section aria-label={`Replica ${letter}`} className={cn("flex flex-col gap-2.5 rounded-xl border bg-card px-4 py-3.5", phase === "queued" ? "border-muted-foreground/60" : "border-border")}>
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="text-sm font-semibold">Replica {letter} <span className="font-mono text-[12px] font-normal text-muted-foreground">job {r.slurm_job_id || "—"} · {r.nodes || "no node yet"}</span></p>
          <p className="mt-0.5 text-[12.5px] text-secondary-foreground">
            {phase === "serving" ? `Serving for ${duration(since)}` : phase === "draining" ? "Stopping: taken out of rotation" : phase === "queued" ? `Waiting ${duration(since)}` : `Loading for ${duration(since)}`}
          </p>
        </div>
        <Pill><Glyph glyph={phase} className="h-[7px] w-[7px]" />{phase === "serving" ? "Serving" : phase === "draining" ? "Stopping" : phase === "queued" ? "Queued" : "Loading"}</Pill>
      </div>
      {phase !== "draining" && (
        <div className="flex gap-1.5" aria-hidden="true">
          {STEPS.map((s, i) => (
            <div key={s.phase} className="flex flex-1 flex-col gap-1">
              <span className={cn("h-1 rounded-full", i < at ? "bg-muted-foreground" : i === at ? "bg-foreground" : "bg-muted")} />
              <span className={cn("text-[11px]", i === at ? "text-foreground" : "text-muted-foreground")}>{s.label}</span>
            </div>
          ))}
        </div>
      )}
      {phase === "queued" && slurm.reason && (
        <p className="text-[12.5px] leading-relaxed">Slurm says <span className="font-mono text-[12px]">{slurm.reason}</span>{slurm.reason === "Resources" ? ": no matching node is free yet." : slurm.reason === "Priority" ? ": other jobs are ahead of it." : "."}</p>
      )}
      {phase === "running" && <p className="text-[12.5px] text-secondary-foreground">The job is running; obleth checks its health path until the model answers, then warms it up.</p>}
      <div className="flex justify-end">
        <button type="button" disabled={pending} onClick={onRestart} className="text-xs text-secondary-foreground underline underline-offset-[3px] hover:text-foreground disabled:opacity-50">Restart</button>
      </div>
    </section>
  );
}

function PlacementFields({ spec }: { spec: ManagedModelSpec }) {
  const resources = useClusterResources();
  const [partition, setPartition] = useState(spec.partition);
  const [gres, setGres] = useState(spec.gres);
  const [cpus, setCpus] = useState(spec.cpus_per_task ? String(spec.cpus_per_task) : "");
  const [mem, setMem] = useState(spec.mem ?? "");
  const [time, setTime] = useState(spec.time_limit ?? "");
  const [account, setAccount] = useState(spec.account ?? "");
  const [qos, setQos] = useState(spec.qos ?? "");
  const fits = useMemo(() => partitionFits(resources, { gpus: gresCount(gres), cpus: cpus ? Number(cpus) : null, memMb: memToMb(mem) }), [resources, gres, cpus, mem]);
  const fit = fits.find((f) => f.name === partition);
  const minutes = parseTimeLimit(time);
  const partitions = [...new Set([spec.partition, ...resources.partitions.map((p) => p.name)])].filter(Boolean);
  const pick = (list: string[], current: string) => [...new Set([current, ...list])].filter(Boolean);

  return (
    <>
      <Setting id="set-partition" label="Partition" hint="Where Slurm runs each replica." fields={["slurm_partition"]} was={{ field: "slurm_partition" }}>
        <div className="flex flex-wrap items-center gap-2.5">
          {resources.partitions.length ? (
            <div className="w-56"><Select name="slurm_partition" aria-label="Partition" value={partition} onValueChange={setPartition} className="h-9 text-[13px]" options={partitions.map((p) => ({ value: p, label: p }))} /></div>
          ) : (
            <TextField name="slurm_partition" label="Partition" required value={partition} onChange={(e) => setPartition(e.target.value)} mono className="w-56" />
          )}
          {fit && <span className={cn("text-xs", fit.fits ? "text-muted-foreground" : "font-medium text-foreground")}>{fit.fits ? `${fit.shape} · ${fit.nodes.length} nodes${fit.idleFitting != null ? ` · ${fit.idleFitting} idle that fit` : ""}${fit.maxMinutes ? ` · max ${walltimeLabel(fit.maxMinutes)}` : ""}` : `Won't fit: ${fit.reason}`}</span>}
        </div>
      </Setting>
      <Setting id="set-resources" label="Each replica gets" hint="GPUs as gres, CPUs, and memory per node. Blank uses the partition's default." fields={["slurm_gres", "slurm_cpus_per_task", "slurm_mem", "slurm_nodes"]}>
        <div className="flex flex-wrap items-center gap-2">
          <TextField name="slurm_gres" label="GPUs (gres)" value={gres} onChange={(e) => setGres(e.target.value)} placeholder="gpu:1" mono className="w-28" />
          <TextField name="slurm_cpus_per_task" label="CPUs" inputMode="numeric" value={cpus} onChange={(e) => setCpus(e.target.value)} placeholder="CPUs" mono className="w-24" />
          <TextField name="slurm_mem" label="Memory" value={mem} onChange={(e) => setMem(e.target.value)} placeholder="560G" mono className="w-24" />
          <TextField name="slurm_nodes" label="Nodes" inputMode="numeric" defaultValue={spec.nodes} mono className="w-20" />
          <span className="text-xs text-muted-foreground">nodes per job</span>
        </div>
      </Setting>
      <Setting id="set-account" label="Account · QoS" hint="Blank uses your Slurm user's defaults." fields={["slurm_account", "slurm_qos"]}>
        <div className="flex flex-wrap gap-2">
          {resources.accounts.length ? (
            <div className="w-48"><Select name="slurm_account" aria-label="Account" value={account} onValueChange={setAccount} className="h-9 text-[13px]" options={[{ value: "", label: "Default account" }, ...pick(resources.accounts, account).map((a) => ({ value: a, label: a }))]} /></div>
          ) : (
            <TextField name="slurm_account" label="Account" value={account} onChange={(e) => setAccount(e.target.value)} placeholder="Default account" mono className="w-48" />
          )}
          {resources.qos.length ? (
            <div className="w-40"><Select name="slurm_qos" aria-label="QoS" value={qos} onValueChange={setQos} className="h-9 text-[13px]" options={[{ value: "", label: "Default QoS" }, ...pick(resources.qos, qos).map((a) => ({ value: a, label: a }))]} /></div>
          ) : (
            <TextField name="slurm_qos" label="QoS" value={qos} onChange={(e) => setQos(e.target.value)} placeholder="Default QoS" mono className="w-40" />
          )}
        </div>
      </Setting>
      <Setting id="set-walltime" label="Walltime per job" hint="When a job's time is up, obleth submits a new one." fields={["slurm_time_limit"]} was={{ field: "slurm_time_limit" }}>
        <div className="flex flex-wrap items-center gap-1.5">
          {walltimeChoices(fit?.maxMinutes ?? null).map((m, i, all) => (
            <button key={m} type="button" aria-pressed={minutes === m} onClick={() => setTime(formatTimeLimit(m))} className={cn("inline-flex h-8 items-center rounded-full border px-3 text-[12.5px]", minutes === m ? "border-foreground bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground")}>
              {walltimeLabel(m)}{fit?.maxMinutes && i === all.length - 1 ? " · most" : ""}
            </button>
          ))}
          <TextField name="slurm_time_limit" label="Walltime" value={time} onChange={(e) => setTime(e.target.value)} placeholder="D-HH:MM:SS" mono className="ml-1 w-36" />
        </div>
        {fit?.maxMinutes && minutes && minutes > fit.maxMinutes ? <span className="text-xs font-medium">Over {partition}&apos;s {walltimeLabel(fit.maxMinutes)} limit: Slurm will refuse it.</span> : null}
      </Setting>
      <Setting id="set-constraints" label="Constraints · excluded nodes" hint="Node features to require, and nodes to keep off." fields={["slurm_constraints", "slurm_exclude"]}>
        <div className="flex flex-wrap gap-2">
          <TextField name="slurm_constraints" label="Constraints" defaultValue={spec.constraints ?? ""} placeholder="e.g. h200&nvlink" mono className="w-56" />
          <TextField name="slurm_exclude" label="Excluded nodes" defaultValue={spec.exclude ?? ""} placeholder="e.g. node[01-04]" mono className="w-56" />
        </div>
      </Setting>
    </>
  );
}

function RemoveDialog({ open, onClose, name, onRemove, pending }: { open: boolean; onClose: () => void; name: string; onRemove: (deleteModel: boolean) => void; pending: boolean }) {
  return (
    <Dialog open={open} onOpenChange={(v) => { if (!v) onClose(); }}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Remove {name}?</DialogTitle>
          <DialogDescription>obleth cancels its jobs and stops launching it. Requests to it fail once its last replica is gone.</DialogDescription>
        </DialogHeader>
        <DialogFooter className="flex-col gap-2 sm:flex-col">
          <Button type="button" disabled={pending} onClick={() => onRemove(false)}>Stop launching, keep the model</Button>
          <Button type="button" variant="outline" disabled={pending} onClick={() => onRemove(true)}>Delete the model too</Button>
          <Button type="button" variant="ghost" disabled={pending} onClick={onClose}>Cancel</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** A model obleth launches on Slurm: its replicas, what they run, and where. */
export function ManagedPage({ modelId, initial, changes }: { modelId: string; initial: DeploymentsData; changes: AuditEntry[] }) {
  const router = useRouter();
  const queryClient = useQueryClient();
  const { confirm, confirmElement } = useConfirm();
  const [pending, start] = useTransition();
  const [notice, setNotice] = useState<{ text: string; strong?: boolean } | null>(null);
  const [removing, setRemoving] = useState(false);
  const [saveName, setSaveName] = useState<string | null>(null);
  const [dirty, setDirty] = useState<string[]>([]);
  const [active, setActive] = useState("replicas");
  const live = useQuery({ queryKey: ["deployments"], queryFn: () => getJson<DeploymentsData>("/api/live/deployments"), initialData: initial, refetchInterval: 10_000 });
  const fairshare = useFairshareLive();
  const data = live.data ?? initial;
  const row = buildDeploymentRows(data.models, data.specs, data.replicas, data.discovery?.models ?? [], data.requests).find((r) => r.model.id === modelId);
  const spec = row?.spec ?? initial.specs.find((s) => s.model_id === modelId)!;
  const model = row?.model ?? initial.models.find((m) => m.id === modelId)!;
  const replicas = row?.replicas ?? [];
  const current = replicas.filter((r) => r.state !== "lost").sort((a, b) => a.created_at.localeCompare(b.created_at));
  const lost = replicas.filter((r) => r.state === "lost").sort((a, b) => b.updated_at.localeCompare(a.updated_at));
  const counts = countReplicas(replicas);
  const day = useModelDay(model.model_name);
  const inFlight = fairshare.data?.model_in_flight?.[model.model_name] ?? 0;
  const queued = fairshare.data?.model_queued?.[model.model_name] ?? 0;
  const cap = model.max_in_flight ?? 0;
  const engine = typeof spec.launcher_spec?.name === "string" ? (spec.launcher_spec.name as string) : null;
  const fromRecipe = typeof spec.launcher_spec?.recipe_id === "string";
  const provisionerOk = !!data.slurm?.enabled && !!data.slurm.provisioner_running;

  const refresh = useCallback(async () => {
    await queryClient.invalidateQueries({ queryKey: ["deployments"] });
    router.refresh();
  }, [queryClient, router]);

  function run(task: () => Promise<{ ok: boolean; error?: string }>, done?: string) {
    start(async () => {
      const res = await task();
      setNotice(res.ok ? (done ? { text: done } : null) : { text: res.error ?? "That didn't work.", strong: true });
      await refresh();
    });
  }

  function scale(to: number) {
    if (to < 1) return;
    run(() => setDeploymentReplicasAction(modelId, to));
  }

  function pause() {
    start(async () => {
      if (spec.enabled) {
        const ok = await confirm({ title: `Pause ${model.model_name}?`, description: "obleth cancels its jobs and launches none until you resume. Requests to it fail once the last replica stops.", confirmLabel: "Pause" });
        if (!ok) return;
      }
      const res = await setDeploymentEnabledAction(modelId, !spec.enabled);
      setNotice(res.ok ? null : { text: res.error, strong: true });
      await refresh();
    });
  }

  function restartAll() {
    start(async () => {
      const ok = await confirm({ title: `Restart all ${current.length} replicas?`, description: "Each job is cancelled and a new one submitted with the saved settings. Nothing serves until the new ones are healthy.", confirmLabel: "Restart all" });
      if (!ok) return;
      for (const r of current) await restartReplicaAction(r.id);
      setNotice({ text: `Restarting ${current.length} replica${current.length === 1 ? "" : "s"}.` });
      await refresh();
    });
  }

  function remove(deleteModel: boolean) {
    start(async () => {
      const res = await removeDeploymentAction(modelId, deleteModel);
      if (!res.ok) { setNotice({ text: res.error, strong: true }); return; }
      setRemoving(false);
      router.push(deleteModel ? "/deployments" : modelHref(model.model_name));
    });
  }

  useEffect(() => {
    const ids = ["replicas", "script", "placement", "service", "changes"];
    const els = ids.map((id) => document.getElementById(id)).filter((e): e is HTMLElement => !!e);
    const observer = new IntersectionObserver((entries) => {
      const visible = entries.filter((e) => e.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
      if (visible[0]) setActive(visible[0].target.id);
    }, { rootMargin: "-20% 0px -65% 0px" });
    els.forEach((e) => observer.observe(e));
    return () => observer.disconnect();
  }, []);

  const onDirty = useCallback((sections: string[]) => setDirty(sections), []);
  const sectionOf = useCallback((n: string) => cardOf(n), []);
  const labelOf = useCallback((n: string) => LABELS[n] ?? n, []);
  const stopped = row?.view.state === "stopped";
  const nav: { id: string; label: string; count?: string | number }[] = [
    { id: "replicas", label: "Replicas", count: `${counts.healthy} / ${spec.enabled ? spec.target_replicas : 0}` },
    { id: "script", label: "Script" },
    { id: "placement", label: "Placement" },
    { id: "service", label: "Service" },
    { id: "changes", label: "Changes" },
  ];

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-5">
      {confirmElement}
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <p className="text-[12.5px] text-muted-foreground"><Link href="/deployments" className="text-secondary-foreground hover:text-foreground">Deployments</Link> / Slurm</p>
          <h1 className="truncate font-mono text-[24px] font-medium">{model.model_name}</h1>
          <div className="flex flex-wrap items-center gap-2">
            {row && <StateMark state={row.view.state} />}
            {row?.view.why && <span className="text-[12.5px] text-secondary-foreground">{row.view.why}</span>}
            <Pill>Slurm · launched by obleth</Pill>
            {engine && <Pill>from {engine}</Pill>}
            <span className="text-[12.5px] text-muted-foreground">{row?.where}{spec.account || spec.qos ? ` · ${[spec.account, spec.qos].filter(Boolean).join(" / ")}` : ""}</span>
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <div className="inline-flex h-9 items-center rounded-lg border border-border">
            <button type="button" aria-label="One fewer replica" disabled={pending || !spec.enabled || spec.target_replicas <= 1} onClick={() => scale(spec.target_replicas - 1)} className="inline-flex h-9 w-9 items-center justify-center text-secondary-foreground hover:text-foreground disabled:opacity-40"><Minus className="h-3.5 w-3.5" /></button>
            <span className="px-1 text-[13px] tabular-nums">{spec.target_replicas} replica{spec.target_replicas === 1 ? "" : "s"}</span>
            <button type="button" aria-label="One more replica" disabled={pending || !spec.enabled} onClick={() => scale(spec.target_replicas + 1)} className="inline-flex h-9 w-9 items-center justify-center text-secondary-foreground hover:text-foreground disabled:opacity-40"><Plus className="h-3.5 w-3.5" /></button>
          </div>
          <Button type="button" variant="outline" size="sm" className="h-9" disabled={pending} onClick={pause}>{spec.enabled ? "Pause" : "Resume"}</Button>
          <Button asChild variant="outline" size="sm" className="h-9"><Link href={logsHref({ model: model.model_name, window: "24h" })}>See its requests</Link></Button>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button type="button" variant="outline" size="icon" className="h-9 w-9" aria-label="More actions"><MoreHorizontal className="h-4 w-4" /></Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => router.push(modelHref(model.model_name))}>Model settings</DropdownMenuItem>
              {model.enabled && <DropdownMenuItem onSelect={() => router.push(`/playground?model=${encodeURIComponent(model.model_name)}`)}>Try in Playground</DropdownMenuItem>}
              <DropdownMenuItem disabled={current.length === 0} onSelect={restartAll}>Restart all replicas…</DropdownMenuItem>
              {fromRecipe && <DropdownMenuItem onSelect={() => setSaveName(`${engine ?? model.model_name} · ${spec.partition}${spec.nodes > 1 ? ` · ${spec.nodes} nodes` : ""}`)}>Save as recipe…</DropdownMenuItem>}
              <DropdownMenuSeparator />
              <DropdownMenuItem onSelect={() => setRemoving(true)}>Remove deployment…</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      {fromRecipe && counts.healthy > 0 && saveName === null && (
        <p className="text-[12.5px] text-muted-foreground">It serves. <button type="button" onClick={() => setSaveName(`${engine ?? model.model_name} · ${spec.partition}${spec.nodes > 1 ? ` · ${spec.nodes} nodes` : ""}`)} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Save these settings as a recipe</button> to launch it the same way again.</p>
      )}
      {saveName !== null && (
        <form onSubmit={(e) => { e.preventDefault(); run(async () => { const res = await saveRecipeFromDeploymentAction(modelId, saveName); if (res.ok) setSaveName(null); return res; }, "Saved. It's under Recipes › Saved, and first in New deployment."); }} aria-label="Save as recipe" className="flex flex-wrap items-center gap-2 rounded-xl border border-border bg-card px-4 py-3">
          <span className="text-[13px] font-medium">Save as recipe</span>
          <input aria-label="Recipe name" value={saveName} onChange={(e) => setSaveName(e.target.value)} className="h-9 min-w-[260px] flex-1 rounded-lg border border-input bg-background px-3 text-[13px]" />
          <Button type="submit" size="sm" className="h-9" disabled={pending || !saveName.trim()}>Save</Button>
          <Button type="button" variant="ghost" size="sm" className="h-9" onClick={() => setSaveName(null)}>Cancel</Button>
          <span className="basis-full text-[12px] text-muted-foreground">Keeps the partition, account, QoS, walltime, replica counts and the recipe&apos;s values this deployment runs with.</span>
        </form>
      )}

      {!provisionerOk && (
        <Notice strong>
          {!data.slurm?.enabled ? <>Slurm is turned off in its <Link href="/deployments?slurm=1" className="underline underline-offset-2">connection settings</Link>, so nothing is launched or replaced.</> : <>The provisioner hasn&apos;t checked in{data.slurm?.provisioner_last_seen_secs != null ? ` for ${duration(data.slurm.provisioner_last_seen_secs * 1000)}` : ""}. What you see may be out of date, and nothing is launched or replaced until it&apos;s back.</>}
        </Notice>
      )}
      {notice && <Notice onDismiss={() => setNotice(null)} strong={notice.strong}>{notice.text}</Notice>}

      {stopped && (
        <section aria-label="Why it stopped" className="flex flex-col gap-3 rounded-xl border-[1.5px] border-foreground bg-card px-5 py-4">
          <p className="text-[15px] font-semibold">obleth stopped launching {model.model_name}</p>
          {spec.last_provision_error && counts.lost < spec.max_job_failures ? (
            <div className="flex max-w-3xl flex-col gap-1.5 text-[13px] text-secondary-foreground">
              <p>Slurm refused the job when it was submitted: <b className="font-semibold text-foreground">{slurmErrorText(spec.last_provision_error)}</b></p>
              {/invalid account|account\/partition/i.test(spec.last_provision_error) && <p>The account {spec.account ? <span className="font-mono text-[12px]">{spec.account}</span> : "(your default)"} can&apos;t run jobs on <span className="font-mono text-[12px]">{spec.partition}</span>. Change the account or the partition under Placement, then try once more.</p>}
              <details className="text-[12px]"><summary className="cursor-pointer text-muted-foreground">Slurm&apos;s full answer</summary><pre className="mt-1.5 whitespace-pre-wrap break-all font-mono text-[11.5px] text-muted-foreground">{spec.last_provision_error}</pre></details>
            </div>
          ) : (
            <p className="max-w-3xl text-[13px] text-secondary-foreground">
              The last {counts.lost} job{counts.lost === 1 ? "" : "s"} ended without becoming healthy, so it stopped submitting new ones (the limit is {spec.max_job_failures} failed jobs on record; each is forgotten 15 minutes after it ends).
              {counts.healthy ? ` ${counts.healthy} replica${counts.healthy === 1 ? " is" : "s are"} still serving.` : " Nothing is serving; requests get 503 until a replica is healthy."}
            </p>
          )}
          {lost[0] && (
            <p className="max-w-3xl text-[13px]"><b className="font-semibold">What Slurm last said:</b> <span className="font-mono text-[12px]">{lost[0].last_message || "the job was gone"}</span>, after {duration(new Date(lost[0].updated_at).getTime() - new Date(lost[0].created_at).getTime())}.</p>
          )}
          <div className="flex flex-wrap gap-2">
            <Button type="button" size="sm" disabled={pending} onClick={() => run(() => clearLostReplicasAction(modelId), "Cleared. obleth will try again on its next pass.")}>Try once more</Button>
            <Button type="button" size="sm" variant="outline" onClick={() => document.getElementById("script")?.scrollIntoView({ behavior: "smooth" })}>Edit the script</Button>
          </div>
        </section>
      )}

      <div className="grid items-start gap-6 lg:grid-cols-[200px_minmax(0,1fr)]">
        <nav aria-label="Deployment sections" className="flex flex-col gap-0.5 lg:sticky lg:top-0">
          {nav.map((n) => (
            <a
              key={n.id}
              href={`#${n.id}`}
              onClick={(e) => { e.preventDefault(); document.getElementById(n.id)?.scrollIntoView({ behavior: "smooth", block: "start" }); }}
              aria-current={active === n.id ? "true" : undefined}
              className={cn("flex h-8 items-center justify-between rounded-lg px-2.5 text-[13px] transition-colors", active === n.id ? "bg-secondary text-foreground" : "text-muted-foreground hover:bg-muted/40 hover:text-foreground")}
            >
              {n.label}
              <span className="flex items-center gap-1.5 text-[11.5px] text-muted-foreground">{n.count}{dirty.includes(n.id) && <span aria-label="unsaved changes" className="h-1.5 w-1.5 rounded-full bg-foreground" />}</span>
            </a>
          ))}
          <div className="mx-1 my-2 h-px bg-border" />
          <Link href={modelHref(model.model_name)} className="flex h-8 items-center rounded-lg px-2.5 text-[13px] text-muted-foreground hover:bg-muted/40 hover:text-foreground">Model settings ›</Link>
        </nav>

        <div className="flex min-w-0 flex-col gap-4">
          <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
            <Tile label="Serving" value={`${counts.healthy} of ${spec.enabled ? spec.target_replicas : 0}`} detail={[counts.queued ? `${counts.queued} queued` : null, counts.running ? `${counts.running} loading` : null, `serves from ${spec.min_replicas}`].filter(Boolean).join(" · ")} />
            <div className="flex min-w-0 flex-col gap-1 rounded-xl border border-border bg-card px-4 py-3">
              <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Pool now</span>
              <span className="text-[24px] font-semibold leading-tight tabular-nums">{inFlight} <span className="text-[13px] font-normal text-muted-foreground">{cap ? `of ${cap}` : "in flight"}</span></span>
              <Meter value={inFlight} max={cap} strong={cap > 0 && inFlight >= cap} />
              <span className="text-xs text-muted-foreground">{queued ? `${queued} waiting` : "none waiting"}</span>
            </div>
            <Tile label="Requests · 24h" value={day.data ? compact(day.data.requests) : "—"} href={day.data?.requests ? logsHref({ model: model.model_name, window: "24h" }) : undefined} detail={day.data?.requests ? "see them" : null} />
            <Tile label="Failed · 24h" value={day.data ? compact(day.data.errors) : "—"} href={day.data?.errors ? logsHref({ model: model.model_name, window: "24h", status: "error" }) : undefined} detail={day.data?.errors && day.data.requests ? `${((day.data.errors / day.data.requests) * 100).toFixed(1)}% of requests` : day.data ? "none" : null} />
          </div>

          <section id="replicas" aria-label="Replicas" className="flex scroll-mt-24 flex-col gap-3">
            {current.length === 0 ? (
              <div className="rounded-xl border border-dashed border-border px-5 py-6 text-center text-[13px] text-muted-foreground">
                {spec.enabled ? "No jobs yet. The provisioner submits one on its next pass (every 15 seconds)." : "Paused: no jobs are running. Resume to launch again."}
              </div>
            ) : (
              <div className="grid gap-3 xl:grid-cols-2">
                {current.map((r, i) => (
                  <ReplicaCard key={r.id} r={r} letter={String.fromCharCode(65 + (i % 26))} pending={pending} onRestart={() => run(() => restartReplicaAction(r.id), "Restarting that replica: its job is cancelled and a new one submitted.")} />
                ))}
              </div>
            )}
            {lost.length > 0 && (
              <details className="rounded-xl border border-border bg-card px-[18px] py-3" open={stopped}>
                <summary className="cursor-pointer text-[13px] font-medium">Jobs that ended · {lost.length}</summary>
                <div className="mt-2">
                  <div className="grid grid-cols-[100px_110px_minmax(0,1fr)_80px_90px] gap-3 py-2 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground"><span>Job</span><span>Node</span><span>Last from Slurm</span><span>Ran</span><span>Ended</span></div>
                  {lost.slice(0, 10).map((r) => (
                    <div key={r.id} className="grid grid-cols-[100px_110px_minmax(0,1fr)_80px_90px] gap-3 border-t border-border py-2 text-[12.5px]">
                      <span className="font-mono text-[12px]">{r.slurm_job_id || "—"}</span>
                      <span className="truncate font-mono text-[12px]">{r.nodes || "—"}</span>
                      <span className="truncate">{r.last_message || "job gone"}</span>
                      <span className="font-mono text-[12px]">{duration(new Date(r.updated_at).getTime() - new Date(r.created_at).getTime())}</span>
                      <span className="font-mono text-[12px] text-muted-foreground">{new Date(r.updated_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</span>
                    </div>
                  ))}
                  <p className="border-t border-border pt-2 text-xs text-muted-foreground">Job output is in {spec.log_output_dir ? <span className="font-mono">{spec.log_output_dir}</span> : "the job's working directory"} on the cluster.</p>
                </div>
              </details>
            )}
          </section>

          <InFlightChart model={model.model_name} cap={cap} />

          <SettingsForm
            id={modelId}
            sectionOf={sectionOf}
            labelOf={labelOf}
            save={saveDeploymentSettingsAction}
            onSaved={() => { setNotice({ text: "Saved. Replicas started from now use it; restart the running ones to pick it up now." }); void refresh(); }}
            onDirtyChange={onDirty}
            className="flex flex-col gap-4"
            ariaLabel={`${model.model_name} launch settings`}
          >
            <input type="hidden" name="slurm_target_replicas" value={String(spec.target_replicas)} />
            <SettingsCard id="script" title="Script" description="What each replica's job runs. obleth adds the port binding before it; the server must listen on $OBLETH_SERVING_PORT.">
              <Setting id="set-script" label="Job script" hint="A change applies to jobs started after you save." fields={["slurm_script_body"]}>
                <TextArea name="slurm_script_body" label="Job script" rows={Math.min(24, Math.max(8, (spec.script_body || "").split("\n").length + 1))} defaultValue={spec.script_body} />
              </Setting>
            </SettingsCard>
            <SettingsCard id="placement" title="Placement" description="Where Slurm runs each replica. Choices come from the cluster.">
              <PlacementFields spec={spec} />
            </SettingsCard>
            <SettingsCard id="service" title="Service" description="How obleth checks a replica, and when it gives up.">
              <Setting id="set-port" label="Port · health path" hint="The port the server listens on in the job, and the path obleth checks." fields={["slurm_serving_port", "slurm_health_path"]}>
                <div className="flex gap-2">
                  <TextField name="slurm_serving_port" label="Port" inputMode="numeric" required defaultValue={spec.serving_port} mono className="w-24" />
                  <TextField name="slurm_health_path" label="Health path" defaultValue={spec.health_path} mono className="w-40" />
                </div>
              </Setting>
              <Setting id="set-min" label="Serve from" hint={`Healthy replicas needed before it counts as up. The replica count (${spec.target_replicas}) is set at the top.`} fields={["slurm_min_replicas"]} was={{ field: "slurm_min_replicas" }}>
                <TextField name="slurm_min_replicas" label="Serve from" inputMode="numeric" required defaultValue={spec.min_replicas} mono className="w-20" />
              </Setting>
              <Setting id="set-failures" label="Stop after failed launches" hint="Failed jobs on record (each is forgotten 15 minutes after it ends); then it waits for you. 0 never stops." fields={["slurm_max_job_failures"]} was={{ field: "slurm_max_job_failures" }}>
                <TextField name="slurm_max_job_failures" label="Stop after failed launches" inputMode="numeric" required defaultValue={spec.max_job_failures} mono className="w-20" />
              </Setting>
            </SettingsCard>
          </SettingsForm>

          <ChangesList changes={changes} />
        </div>
      </div>

      <RemoveDialog open={removing} onClose={() => setRemoving(false)} name={model.model_name} onRemove={remove} pending={pending} />
    </div>
  );
}
