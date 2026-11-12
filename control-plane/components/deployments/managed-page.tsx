"use client";

import { useCallback, useEffect, useMemo, useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Minus, MoreHorizontal, Plus } from "lucide-react";
import {
  removeDeploymentAction,
  restartReplicaAction,
  retryDeploymentAction,
  rollDeploymentAction,
  setModelStatusAction,
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
  accountsFor,
  buildDeploymentRows,
  clockLabel,
  countReplicas,
  duration,
  jobEndLine,
  pauseOf,
  pendingReason,
  provisionerHealth,
  refusalAdvice,
  scaleDownVictim,
  START_PROVISIONER,
  wasRequeued,
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
  replicaAsk,
  type ReplicaPhase,
} from "@/lib/deployments-model";
import { logsHref } from "@/lib/log-links";
import { modelHref } from "@/lib/models-model";
import type { AuditEntry, ManagedModelSpec, ModelReplica } from "@/lib/obleth";
import { PORT_WINDOW } from "@/lib/managed-model-schema";
import { compact } from "@/lib/overview-model";
import { sbatchLines, stripSbatchDirectives, type PlacementKey, type SbatchLine } from "@/lib/sbatch-directives";
import { useClusterResources } from "@/lib/use-cluster-resources";
import { cn, getJson } from "@/lib/utils";

type Card = "script" | "placement" | "service";

function cardOf(name: string): Card | null {
  if (name === "slurm_script_body" || name === "slurm_log_output_dir") return "script";
  if (["slurm_partition", "slurm_gres", "slurm_cpus_per_task", "slurm_mem", "slurm_nodes", "slurm_account", "slurm_qos", "slurm_time_limit", "slurm_constraints", "slurm_exclude"].includes(name)) return "placement";
  if (["slurm_serving_port", "slurm_health_path", "slurm_min_replicas", "slurm_max_job_failures", "slurm_startup_timeout_mins", "slurm_handoff_mins"].includes(name)) return "service";
  return null;
}

const LABELS: Record<string, string> = {
  slurm_script_body: "Job script",
  slurm_log_output_dir: "Job logs",
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
  slurm_min_replicas: "Healthy when",
  slurm_max_job_failures: "Stop after failed launches",
  slurm_startup_timeout_mins: "Give up loading after",
  slurm_handoff_mins: "Start a replacement",
};

const STEPS: { phase: ReplicaPhase; label: string }[] = [
  { phase: "queued", label: "Queued" },
  { phase: "running", label: "Loading" },
  { phase: "serving", label: "Serving" },
];

/** ", 2 failed before serving, 1 ended after serving" when the gateway says which served. */
function endedSplit(lost: ModelReplica[]): string {
  if (!lost.some((r) => r.job_started_at !== undefined)) return "";
  const served = lost.filter((r) => !!r.job_started_at).length;
  const failed = lost.length - served;
  return [failed ? `, ${failed} failed before serving` : "", served ? `, ${served} ended after serving (walltime, preempted or cancelled)` : ""].join("");
}

/** Minutes to wait for a loading replica before pointing at its log. */
const SLOW_LOAD_MINS = 30;

function ReplicaCard({ r, letter, spec, onRestart, pending, stale }: { r: ModelReplica; letter: string; spec: ManagedModelSpec; onRestart: () => void; pending: boolean; stale: boolean }) {
  const phase = replicaPhase(r);
  const at = STEPS.findIndex((s) => s.phase === phase);
  const slurm = slurmStatus(r.last_message);
  const now = Date.now();
  const requeued = wasRequeued(r);
  // Loading counts from when Slurm started the job, not from the submit.
  const loadingFrom = r.job_started_at ?? null;
  const since = now - new Date(phase === "serving" ? r.job_started_at ?? r.updated_at : phase === "running" ? loadingFrom ?? r.created_at : r.created_at).getTime();
  const why = phase === "queued" && !requeued ? pendingReason(slurm.reason) : null;
  const blocked = !!why?.blocks;
  const giveUp = spec.startup_timeout_mins;
  const slow = phase === "running" && since > (giveUp && giveUp > 0 ? Math.min(giveUp, SLOW_LOAD_MINS) : SLOW_LOAD_MINS) * 60_000;
  const end = phase === "serving" ? jobEndLine(r.job_ends_at, spec.handoff_mins, now) : null;
  const label = phase === "serving" ? "Serving" : phase === "draining" ? "Stopping" : phase === "queued" ? (blocked ? "Can't start" : "Queued") : "Loading";
  return (
    <section aria-label={`Replica ${letter}`} className={cn("flex flex-col gap-2.5 rounded-xl border bg-card px-4 py-3.5", blocked ? "border-[1.5px] border-foreground" : phase === "queued" ? "border-muted-foreground/60" : "border-border")}>
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="text-sm font-semibold">Replica {letter} <span className="font-mono text-[12px] font-normal text-muted-foreground">job {r.slurm_job_id || "—"} · {r.nodes || "no node yet"}{r.port ? ` · port ${r.port}` : ""}</span></p>
          <p className="mt-0.5 text-[12.5px] text-secondary-foreground">
            {phase === "serving" ? `Serving for ${duration(since)}` : phase === "draining" ? "Stopping. It is out of rotation, and its job is cancelled once its requests finish." : phase === "queued" ? `Waiting in the queue for ${duration(since)}` : loadingFrom ? `Loading for ${duration(since)}` : `Submitted ${duration(since)} ago, now loading`}
            {stale ? ` (last confirmed ${duration(now - new Date(r.updated_at).getTime())} ago)` : ""}
          </p>
        </div>
        {blocked ? (
          <span className="inline-flex shrink-0 items-center rounded-full bg-foreground px-2.5 py-0.5 text-[11.5px] font-semibold text-background">{label}</span>
        ) : (
          <Pill><Glyph glyph={phase} className="h-[7px] w-[7px]" />{label}</Pill>
        )}
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
      {r.state_detail ? (
        <p className="text-[12.5px] leading-relaxed">{r.state_detail}</p>
      ) : requeued ? (
        <p className="text-[12.5px] leading-relaxed">Slurm put the job back in the queue (preempted or its node failed). It will start again when resources free up.</p>
      ) : phase === "queued" && slurm.reason ? (
        <p className={cn("text-[12.5px] leading-relaxed", blocked && "font-medium")}>Slurm says <span className="font-mono text-[12px]">{slurm.reason}</span>. {why?.text ?? "It starts when Slurm schedules it."}</p>
      ) : null}
      {phase === "running" && !r.state_detail && <p className="text-[12.5px] text-secondary-foreground">The job is running. obleth checks {spec.health_path || "/health"} until the model answers, then warms it up.</p>}
      {slow && <p className="text-[12.5px] font-medium">Still not answering on {spec.health_path || "/health"} after {duration(since)}. Check the job&apos;s output below. The server must listen on $OBLETH_SERVING_PORT.{giveUp ? ` obleth gives up after ${giveUp} minutes and counts it as a failed launch.` : ""}</p>}
      {end && <p className="text-[12.5px] text-secondary-foreground">{end}</p>}
      {r.replace_requested && <p className="text-[12.5px] text-secondary-foreground">A replacement is starting. This one stops once the new one serves.</p>}
      {r.slurm_job_id && <p className="text-[12px] text-muted-foreground"><JobOutput dir={spec.log_output_dir} path={r.log_path} job={r.slurm_job_id} node={r.nodes} live /></p>}
      <div className="flex justify-end">
        <button type="button" disabled={pending || r.replace_requested || r.cancel_requested} onClick={onRestart} className="text-xs text-secondary-foreground underline underline-offset-[3px] hover:text-foreground disabled:opacity-50">{r.replace_requested ? "Being replaced" : "Restart"}</button>
      </div>
    </section>
  );
}

type Placement = Record<PlacementKey, string>;

const SETTING_NAME: Record<PlacementKey, string> = {
  partition: "partition",
  gres: "GPUs",
  cpus_per_task: "CPUs",
  mem: "memory",
  nodes: "nodes",
  account: "account",
  qos: "QoS",
  time_limit: "walltime",
  constraints: "constraints",
  exclude: "excluded nodes",
  log_output_dir: "Job logs",
};

function placementOf(spec: ManagedModelSpec): Placement {
  return {
    partition: spec.partition,
    gres: spec.gres,
    cpus_per_task: spec.cpus_per_task ? String(spec.cpus_per_task) : "",
    mem: spec.mem ?? "",
    nodes: String(spec.nodes),
    account: spec.account ?? "",
    qos: spec.qos ?? "",
    time_limit: spec.time_limit ?? "",
    constraints: spec.constraints ?? "",
    exclude: spec.exclude ?? "",
    log_output_dir: spec.log_output_dir ?? "",
  };
}

/**
 * The job script and where it runs, edited together: a script pasted with
 * `#SBATCH` lines can hand their values to Placement, which is what Slurm
 * actually gets.
 */
function ScriptAndPlacement({ spec }: { spec: ManagedModelSpec }) {
  const [script, setScript] = useState(spec.script_body ?? "");
  const [place, setPlace] = useState<Placement>(() => placementOf(spec));
  const set = useCallback((key: PlacementKey, v: string) => setPlace((p) => ({ ...p, [key]: v })), []);
  const lines = useMemo(() => sbatchLines(script), [script]);

  function moveToPlacement() {
    setPlace((p) => {
      const next = { ...p };
      for (const l of lines) if (l.setting && l.value) next[l.setting] = l.value;
      return next;
    });
    setScript(stripSbatchDirectives(script));
  }

  return (
    <>
      <SettingsCard id="script" title="Script" description="What each replica's job runs. obleth adds a few lines before it that pick a free port, and the server must listen on $OBLETH_SERVING_PORT. GPUs, CPUs and memory are set under Placement.">
        <Setting id="set-script" label="Job script" hint="A change applies to jobs started after you save." fields={["slurm_script_body"]}>
          <TextArea name="slurm_script_body" label="Job script" rows={Math.min(24, Math.max(8, script.split("\n").length + 1))} value={script} onChange={setScript} />
          {lines.length > 0 && <SbatchNotice lines={lines} place={place} onMove={moveToPlacement} />}
          {fixedPort(script) && <span className="text-xs font-medium">The script starts the server on a fixed port. Every replica after the first would then fail its health check. Use --port $OBLETH_SERVING_PORT instead.</span>}
        </Setting>
        <Setting id="set-logs" label="Job logs" hint="The folder where Slurm writes each job's output. Use a full path you can read from a login node. Slurm doesn't create this folder. Create it first and make sure the Slurm user can write to it, or jobs fail before they start." fields={["slurm_log_output_dir"]} was={{ field: "slurm_log_output_dir" }}>
          <TextField name="slurm_log_output_dir" label="Job logs" value={place.log_output_dir} onChange={(e) => set("log_output_dir", e.target.value)} placeholder="/shared/logs" mono className="w-full max-w-[520px]" />
          <span className="text-xs text-muted-foreground">Each job writes its output there as &lt;job name&gt;-&lt;job id&gt;.out. Left blank, the output goes to the job&apos;s working folder, /tmp on the node it ran on, which you can only read from that node.</span>
        </Setting>
      </SettingsCard>
      <SettingsCard id="placement" title="Placement" description="Where Slurm runs each replica and what each job asks for. Choices come from the cluster.">
        <PlacementFields spec={spec} value={place} set={set} />
      </SettingsCard>
    </>
  );
}

/** Why a script's #SBATCH lines do nothing, and a way to move them where they count. */
function SbatchNotice({ lines, place, onMove }: { lines: SbatchLine[]; place: Placement; onMove: () => void }) {
  const n = lines.length;
  return (
    <div role="status" className="flex flex-col gap-2 rounded-lg border border-foreground px-3.5 py-3 text-[12.5px]">
      <p className="font-medium">Slurm doesn&apos;t read {n === 1 ? "this #SBATCH line" : `these ${n} #SBATCH lines`}</p>
      <p className="leading-relaxed text-secondary-foreground">obleth submits each job through Slurm&apos;s REST API, which ignores #SBATCH lines. Jobs get their GPUs, CPUs, memory and the rest from Placement below, and their output folder from Job logs, so editing {n === 1 ? "this line" : "these lines"} changes nothing.</p>
      <ul className="flex flex-col gap-1">
        {lines.map((l, i) => {
          const now = l.setting ? place[l.setting].trim() : "";
          return (
            <li key={i} className="grid gap-x-3 sm:grid-cols-[minmax(0,220px)_minmax(0,1fr)]">
              <span className="truncate font-mono text-[12px]">{l.text}</span>
              <span className="text-secondary-foreground">
                {!l.setting ? "obleth has no setting for this, so it has no effect. It comes out with the others." : l.setting === "log_output_dir" && now === l.value ? "Job logs is the same folder." : l.setting === "log_output_dir" ? <>Job logs is {now ? <span className="font-mono text-[12px]">{now}</span> : "blank"}; this would make it <span className="font-mono text-[12px]">{l.value}</span>.</> : now === l.value ? `Placement's ${SETTING_NAME[l.setting]} is the same.` : <>Placement&apos;s {SETTING_NAME[l.setting]} is {now ? <span className="font-mono text-[12px]">{now}</span> : "blank"}; this would make it <span className="font-mono text-[12px]">{l.value}</span>.</>}
              </span>
            </li>
          );
        })}
      </ul>
      <div className="flex flex-wrap items-center gap-3 pt-0.5">
        <Button type="button" size="sm" variant="outline" onClick={onMove}>Move them to Placement</Button>
        <span className="text-[12px] text-muted-foreground">Placement and Job logs take their values and the lines come out of the script. Nothing changes until you save.</span>
      </div>
    </div>
  );
}

function PlacementFields({ spec, value, set }: { spec: ManagedModelSpec; value: Placement; set: (key: PlacementKey, v: string) => void }) {
  const resources = useClusterResources();
  const { partition, gres, cpus_per_task: cpus, mem, time_limit: time, account, qos } = value;
  const nodes = Number(value.nodes) || 1;
  const fits = useMemo(() => partitionFits(resources, { gpus: gresCount(gres), cpus: cpus ? Number(cpus) : null, memMb: memToMb(mem) }), [resources, gres, cpus, mem]);
  const fit = fits.find((f) => f.name === partition);
  const minutes = parseTimeLimit(time);
  const partitions = [...new Set([partition, spec.partition, ...resources.partitions.map((p) => p.name)])].filter(Boolean);
  const pick = (list: string[], current: string) => [...new Set([current, ...list])].filter(Boolean);
  const box = "flex flex-col gap-1 text-[12px] text-muted-foreground";
  const accountList = (partition ? accountsFor(resources, partition) : null) ?? resources.accounts;

  return (
    <>
      <Setting id="set-partition" label="Partition" hint="Where Slurm runs each replica." fields={["slurm_partition"]} was={{ field: "slurm_partition" }}>
        <div className="flex flex-wrap items-center gap-2.5">
          {resources.partitions.length ? (
            <div className="w-56"><Select name="slurm_partition" aria-label="Partition" value={partition} onValueChange={(v) => set("partition", v)} className="h-9 text-[13px]" options={partitions.map((p) => ({ value: p, label: p }))} /></div>
          ) : (
            <TextField name="slurm_partition" label="Partition" value={partition} onChange={(e) => set("partition", e.target.value)} mono className="w-56" />
          )}
          {fit && <span className={cn("text-xs", fit.fits ? "text-muted-foreground" : "font-medium text-foreground")}>{!fit.known ? "obleth couldn't read this partition's nodes, so it can't check that a replica fits." : fit.fits ? `Nodes have ${fit.shape.replace(/ · /g, ", ").replace(/ per node$/, "")}. ${fit.nodes.length} nodes${fit.idleFitting != null ? `, ${fit.idleFitting} of them free or asleep and big enough` : ""}.${fit.maxMinutes ? ` The longest walltime is ${walltimeLabel(fit.maxMinutes)}.` : ""}` : `One replica won't fit: ${fit.reason}.`}</span>}
          {!partition && <span className="text-xs text-muted-foreground">Blank uses the cluster&apos;s default partition.</span>}
        </div>
      </Setting>
      <Setting id="set-nodes" label="Nodes per replica" hint="Each replica is one Slurm job. Use more than 1 only when the script spreads the model across nodes." fields={["slurm_nodes"]} was={{ field: "slurm_nodes" }}>
        <TextField name="slurm_nodes" label="Nodes per replica" inputMode="numeric" value={value.nodes} onChange={(e) => set("nodes", e.target.value)} mono className="w-20" />
      </Setting>
      <Setting id="set-resources" label={nodes > 1 ? "On each node" : "On its node"} hint="What the job asks Slurm for. Leave a box empty to take the default." fields={["slurm_gres", "slurm_cpus_per_task", "slurm_mem"]}>
        <div className="grid max-w-[520px] gap-2.5 sm:grid-cols-3">
          <label className={box}>GPUs<TextField name="slurm_gres" label="GPUs" value={gres} onChange={(e) => set("gres", e.target.value)} placeholder="gpu:1" mono className="text-foreground" /><span className="text-[11.5px]">As Slurm gres, like gpu:1 or gpu:h100:4</span></label>
          <label className={box}>CPUs<TextField name="slurm_cpus_per_task" label="CPUs" inputMode="numeric" value={cpus} onChange={(e) => set("cpus_per_task", e.target.value)} placeholder="Slurm default" mono className="text-foreground" /><span className="text-[11.5px]">A whole number, like 72</span></label>
          <label className={box}>Memory<TextField name="slurm_mem" label="Memory" value={mem} onChange={(e) => set("mem", e.target.value)} placeholder="Partition default" mono className="text-foreground" /><span className="text-[11.5px]">Like 500G, or 0 for all of it</span></label>
        </div>
        <p className="text-[12.5px] text-secondary-foreground">{replicaAsk({ nodes, gres, cpus, mem })}{fit && fit.known && !fit.fits ? ` That's more than a ${fit.name} node has: ${fit.reason}.` : ""}</p>
      </Setting>
      <Setting id="set-account" label="Account and QoS" hint="The Slurm account each job is charged to and its quality of service. Leave them blank to use the Slurm user's defaults." fields={["slurm_account", "slurm_qos"]}>
        <div className="flex flex-wrap gap-2">
          {accountList.length ? (
            <div className="w-48"><Select name="slurm_account" aria-label="Account" value={account} onValueChange={(v) => set("account", v)} className="h-9 text-[13px]" options={[{ value: "", label: "Default account" }, ...pick(accountList, account).map((a) => ({ value: a, label: a }))]} /></div>
          ) : (
            <TextField name="slurm_account" label="Account" value={account} onChange={(e) => set("account", e.target.value)} placeholder="Default account" mono className="w-48" />
          )}
          {resources.qos.length ? (
            <div className="w-40"><Select name="slurm_qos" aria-label="QoS" value={qos} onValueChange={(v) => set("qos", v)} className="h-9 text-[13px]" options={[{ value: "", label: "Default QoS" }, ...pick(resources.qos, qos).map((a) => ({ value: a, label: a }))]} /></div>
          ) : (
            <TextField name="slurm_qos" label="QoS" value={qos} onChange={(e) => set("qos", e.target.value)} placeholder="Default QoS" mono className="w-40" />
          )}
        </div>
      </Setting>
      <Setting id="set-walltime" label="Walltime per job" hint={spec.handoff_mins ? `Slurm ends each job at this limit. obleth starts a replacement ${spec.handoff_mins} minutes before, and stops the old job once the new one serves.` : "Slurm ends each job at this limit, and obleth then submits a new one. With one replica, the model is down from when the job ends until the new one is healthy. Keep two replicas, or pick the longest walltime."} fields={["slurm_time_limit"]} was={{ field: "slurm_time_limit" }}>
        <div className="flex flex-wrap items-center gap-1.5">
          {walltimeChoices(fit?.maxMinutes ?? null).map((m, i, all) => (
            <button key={m} type="button" aria-pressed={minutes === m} onClick={() => set("time_limit", formatTimeLimit(m))} className={cn("inline-flex h-8 items-center rounded-full border px-3 text-[12.5px]", minutes === m ? "border-foreground bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground")}>
              {walltimeLabel(m)}{fit?.maxMinutes && i === all.length - 1 ? ", the longest" : ""}
            </button>
          ))}
          <TextField name="slurm_time_limit" label="Walltime" value={time} onChange={(e) => set("time_limit", e.target.value)} placeholder="D-HH:MM:SS" mono className="ml-1 w-36" />
        </div>
        {fit?.maxMinutes && minutes && minutes > fit.maxMinutes ? <span className="text-xs font-medium">This is over {partition}&apos;s {walltimeLabel(fit.maxMinutes)} limit, so Slurm will hold the job in the queue and never start it. Pick {walltimeLabel(fit.maxMinutes)} or less.</span> : null}
      </Setting>
      <Setting id="set-constraints" label="Constraints and excluded nodes" hint="Only run on nodes that have all of these Slurm features, and never on the nodes listed here." fields={["slurm_constraints", "slurm_exclude"]}>
        <div className="flex flex-wrap gap-2">
          <TextField name="slurm_constraints" label="Constraints" value={value.constraints} onChange={(e) => set("constraints", e.target.value)} placeholder="e.g. h200&nvlink" mono className="w-56" />
          <TextField name="slurm_exclude" label="Excluded nodes" value={value.exclude} onChange={(e) => set("exclude", e.target.value)} placeholder="e.g. node[01-04]" mono className="w-56" />
        </div>
      </Setting>
    </>
  );
}

/** Where a job's output goes, in a sentence: the path the gateway reports, the Job logs folder, or /tmp on its node. */
function JobOutput({ dir, path, job, node, live = false }: { dir: string | null | undefined; path?: string | null; job?: string; node?: string | null; live?: boolean }) {
  const verb = live ? "is going to" : "is in";
  if (path) return <>{job ? `Job ${job}'s output ${verb} ` : "Output goes to "}<span className="break-all font-mono text-[12px]">{path}</span>{!path.startsWith("/") || path.startsWith("/tmp/") ? <> on {node ? <span className="font-mono text-[12px]">{node}</span> : "its node"}. Set Job logs under Script to read it from a login node.</> : "."}</>;
  if (dir) return <>{job ? `Job ${job}'s output ${verb} ` : "Each job's output is in "}<span className="font-mono text-[12px]">{dir}</span>{job ? <>, in the file ending <span className="font-mono text-[12px]">-{job}.out</span>.</> : ", as <job name>-<job id>.out."}</>;
  return <>Job logs isn&apos;t set, so {job ? `job ${job}'s` : "each job's"} output {live ? "goes" : "went"} to /tmp on {node ? <span className="font-mono text-[12px]">{node}</span> : "the node it ran on"}. Set Job logs under Script to keep it in a folder you can read.</>;
}

/** A server started on a fixed port rather than the one obleth hands the job. */
export function fixedPort(script: string): boolean {
  return /--port[=\s]+\d+/.test(script) && !script.includes("OBLETH_SERVING_PORT");
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
  const prov = provisionerHealth(data.slurm);
  const provisionerOk = prov.kind === "ok";
  const replicasUnread = !!data.failed?.includes("replicas");
  // A gateway that reports a startup timeout replaces a replica before
  // stopping it, and can replace them all one at a time.
  const surge = spec.startup_timeout_mins !== undefined;
  const pause_ = pauseOf(spec);
  const refusal = spec.last_provision_error?.trim() ? spec.last_provision_error.trim() : null;
  const staged = (model.effective_status ?? model.lifecycle?.status) === "staged";

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
    start(async () => {
      if (to < spec.target_replicas) {
        const victim = scaleDownVictim(current);
        const phase = victim ? replicaPhase(victim) : null;
        if (victim && (phase === "serving" || phase === "running")) {
          const letter = String.fromCharCode(65 + (current.indexOf(victim) % 26));
          const ok = await confirm({
            title: `Cancel replica ${letter}?`,
            description: phase === "serving"
              ? `Going down to ${to} cancels replica ${letter}'s job, which is serving now. ${counts.healthy > 1 ? "Requests move to the others." : "Nothing else serves, so requests fail until another replica is healthy."}`
              : `Going down to ${to} cancels replica ${letter}'s job, which is loading now.`,
            confirmLabel: "Cancel the job",
          });
          if (!ok) return;
        }
      }
      const res = await setDeploymentReplicasAction(modelId, to);
      setNotice(res.ok ? null : { text: res.error, strong: true });
      await refresh();
    });
  }

  function restartOne(r: ModelReplica, letter: string) {
    start(async () => {
      const phase = replicaPhase(r);
      const description = surge
        ? `obleth starts a new job for replica ${letter} first, and cancels this one once the new one serves. ${phase === "queued" ? "The new job goes to the end of the queue." : ""}`
        : phase === "queued"
          ? `Replica ${letter}'s job is cancelled and a new one submitted. It goes back to the end of the queue.`
          : `Replica ${letter}'s job is cancelled now and a new one submitted. ${phase === "serving" ? (counts.healthy > 1 ? "Requests move to the others until it serves again." : "Nothing else serves, so requests fail until the new one is healthy.") : ""}`;
      const ok = await confirm({ title: `Restart replica ${letter}?`, description: description.trim(), confirmLabel: "Restart" });
      if (!ok) return;
      const res = await restartReplicaAction(r.id);
      setNotice(res.ok ? { text: surge ? `Replacing replica ${letter}: a new job starts first, and the old one stops once it serves.` : `Restarting replica ${letter}: its job is cancelled and a new one submitted.` } : { text: res.error, strong: true });
      await refresh();
    });
  }

  function roll() {
    start(async () => {
      const ok = await confirm({ title: `Apply to the ${current.length} running replica${current.length === 1 ? "" : "s"}?`, description: "obleth replaces them one at a time with the saved settings. Each new job starts before the old one stops, so the model keeps serving.", confirmLabel: "Replace one at a time" });
      if (!ok) return;
      const res = await rollDeploymentAction(modelId);
      setNotice(res.ok ? { text: "Replacing replicas one at a time. Each old job stops once its replacement serves." } : { text: res.error, strong: true });
      await refresh();
    });
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
      const ok = await confirm({ title: `Restart all ${current.length} replicas at once?`, description: "Each job is cancelled now and a new one submitted with the saved settings. Nothing serves until the new ones are healthy.", confirmLabel: "Restart all" });
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
  const resources = useClusterResources();
  const overLimit = useCallback((partition: string, time: string): string | null => {
    const max = Number(resources.partitions.find((p) => p.name === partition)?.max_time ?? 0);
    const mins = parseTimeLimit(time);
    return max > 0 && mins && mins > max ? `The walltime is over ${partition}'s ${walltimeLabel(max)} limit, so Slurm would hold the job and never start it. Pick ${walltimeLabel(max)} or less.` : null;
  }, [resources]);
  const sectionOf = useCallback((n: string) => cardOf(n), []);
  const labelOf = useCallback((n: string) => LABELS[n] ?? n, []);
  const stopped = row?.view.state === "stopped";
  const needsYou = stopped || row?.view.state === "blocked" || !!pause_ || !!refusal;
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
              {surge && <DropdownMenuItem disabled={current.length === 0} onSelect={roll}>Apply to running replicas…</DropdownMenuItem>}
              <DropdownMenuItem disabled={current.length === 0} onSelect={restartAll}>{surge ? "Restart all at once…" : "Restart all replicas…"}</DropdownMenuItem>
              {fromRecipe && <DropdownMenuItem onSelect={() => setSaveName(`${engine ?? model.model_name} · ${spec.partition}${spec.nodes > 1 ? ` · ${spec.nodes} nodes` : ""}`)}>Save as recipe…</DropdownMenuItem>}
              <DropdownMenuSeparator />
              <DropdownMenuItem onSelect={() => setRemoving(true)}>Remove deployment…</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      {fromRecipe && counts.healthy > 0 && saveName === null && (
        <p className="text-[12.5px] text-muted-foreground">It serves. <button type="button" onClick={() => setSaveName(`${engine ?? model.model_name} · ${spec.partition}${spec.nodes > 1 ? ` · ${spec.nodes} nodes` : ""}`)} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Save these settings as a recipe</button> to launch it with the same placement and values again.</p>
      )}
      {saveName !== null && (
        <form onSubmit={(e) => { e.preventDefault(); run(async () => { const res = await saveRecipeFromDeploymentAction(modelId, saveName); if (res.ok) setSaveName(null); return res; }, "Saved. It's under Recipes › Saved, and first in New deployment."); }} aria-label="Save as recipe" className="flex flex-wrap items-center gap-2 rounded-xl border border-border bg-card px-4 py-3">
          <span className="text-[13px] font-medium">Save as recipe</span>
          <input aria-label="Recipe name" value={saveName} onChange={(e) => setSaveName(e.target.value)} className="h-9 min-w-[260px] flex-1 rounded-lg border border-input bg-background px-3 text-[13px]" />
          <Button type="submit" size="sm" className="h-9" disabled={pending || !saveName.trim()}>Save</Button>
          <Button type="button" variant="ghost" size="sm" className="h-9" onClick={() => setSaveName(null)}>Cancel</Button>
          <span className="basis-full text-[12px] text-muted-foreground">Keeps the partition, account, QoS, walltime, replica counts and the recipe&apos;s values this deployment runs with. Edits to the job script made here aren&apos;t kept: the new recipe uses the original recipe&apos;s script. To keep script edits, copy the recipe under Recipes and change its script.</span>
        </form>
      )}

      {!provisionerOk && (
        <Notice strong>
          {prov.kind === "unread" ? <>obleth couldn&apos;t read the Slurm settings just now, so it can&apos;t tell whether the provisioner is running. It tries again every 10 seconds.</>
            : prov.kind === "off" ? <>Slurm is turned off in its <Link href="/deployments?slurm=1" className="underline underline-offset-2">connection settings</Link>, so nothing is launched or replaced.</>
            : prov.kind === "never" ? <>No provisioner has run yet, so nothing will be launched. {START_PROVISIONER}</>
            : prov.kind === "stopped" ? <>The provisioner hasn&apos;t checked in{prov.lastSeenSecs != null ? ` for ${duration(prov.lastSeenSecs * 1000)}` : ""}. What you see may be out of date, and nothing is launched or replaced until it&apos;s back. {START_PROVISIONER}</>
            : <>The provisioner&apos;s passes are failing{prov.lastOkSecs != null ? `, and its last good pass was ${duration(prov.lastOkSecs * 1000)} ago` : ""}, so the replica states below are frozen and nothing is replaced.{prov.detail ? <> It says: <span className="font-mono text-[12px]">{prov.detail}</span>.</> : null} <Link href="/deployments?slurm=1" className="underline underline-offset-2">Test the Slurm connection</Link> to see why.</>}
        </Notice>
      )}
      {staged && counts.healthy > 0 && (
        <div role="status" className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border bg-card px-4 py-3 text-[13px]">
          <span>Serving, but not listed yet. Callers can use {model.model_name} by name, but it isn&apos;t in /v1/models and auto never picks it until you make it live.</span>
          <Button type="button" size="sm" variant="outline" disabled={pending} onClick={() => run(() => setModelStatusAction(model.id, { status: "active" }), `${model.model_name} is live: it's listed in /v1/models.`)}>Make it live</Button>
        </div>
      )}
      {notice && <Notice onDismiss={() => setNotice(null)} strong={notice.strong}>{notice.text}</Notice>}

      {needsYou && (pause_ || refusal || stopped) && (
        <section aria-label="Why it stopped" className="flex flex-col gap-3 rounded-xl border-[1.5px] border-foreground bg-card px-5 py-4">
          <p className="text-[15px] font-semibold">{pause_ || stopped ? `obleth stopped launching ${model.model_name}` : `Slurm refuses ${model.model_name}'s job`}</p>
          {pause_ ? (
            <p className="max-w-3xl text-[13px] text-secondary-foreground">
              {pause_.reason.replace(/\.?$/, ".")} {pause_.until ? `obleth tries again at ${clockLabel(pause_.until)}, or now if you say so.` : "It waits for you."}
              {counts.healthy ? ` ${counts.healthy} replica${counts.healthy === 1 ? " is" : "s are"} still serving.` : " Nothing is serving; requests get 503 until a replica is healthy."}
            </p>
          ) : stopped && !refusal ? (
            <p className="max-w-3xl text-[13px] text-secondary-foreground">
              {counts.lost} job{counts.lost === 1 ? "" : "s"} ended in the last 15 minutes, which reached the limit of {spec.max_job_failures}, so it stopped submitting new ones. Each ended job is forgotten 15 minutes after it ends.
              {counts.healthy ? ` ${counts.healthy} replica${counts.healthy === 1 ? " is" : "s are"} still serving.` : " Nothing is serving; requests get 503 until a replica is healthy."}
            </p>
          ) : null}
          {refusal && (
            <div className="flex max-w-3xl flex-col gap-1.5 text-[13px] text-secondary-foreground">
              <p>Slurm refused the job when it was submitted: <b className="font-semibold text-foreground">{slurmErrorText(refusal)}</b></p>
              {refusalAdvice(refusal) && <p>{refusalAdvice(refusal)}</p>}
              <p>{pause_ || surge ? "obleth waits longer between tries each time Slurm refuses it." : "obleth submits it again on every pass, every 15 seconds, until something changes."}</p>
              {refusal.includes("{") && <details className="text-[12px]"><summary className="cursor-pointer text-muted-foreground">Slurm&apos;s full answer</summary><pre className="mt-1.5 whitespace-pre-wrap break-all font-mono text-[11.5px] text-muted-foreground">{refusal}</pre></details>}
            </div>
          )}
          {lost[0] && !refusal && (
            <p className="max-w-3xl text-[13px]"><b className="font-semibold">What Slurm last said:</b> <span className="font-mono text-[12px]">{lost[0].state_detail || lost[0].last_message || "the job was gone"}</span>, after {duration(new Date(lost[0].updated_at).getTime() - new Date(lost[0].job_started_at ?? lost[0].created_at).getTime())}.</p>
          )}
          {lost[0]?.slurm_job_id && !refusal && <p className="max-w-3xl text-[13px] text-secondary-foreground"><JobOutput dir={spec.log_output_dir} path={lost[0].log_path} job={lost[0].slurm_job_id} node={lost[0].nodes} /></p>}
          {lost[0] && !refusal && /FAILED/i.test(lost[0].last_message ?? "") && new Date(lost[0].updated_at).getTime() - new Date(lost[0].created_at).getTime() < 30_000 && (
            <p className="max-w-3xl text-[13px] text-secondary-foreground">It failed within seconds. That usually means Slurm couldn&apos;t open its output file: check that the Job logs folder exists and the Slurm user can write to it.</p>
          )}
          <div className="flex flex-wrap gap-2">
            <Button type="button" size="sm" disabled={pending} onClick={() => run(() => retryDeploymentAction(modelId), "obleth will submit again on its next pass.")}>Try again</Button>
            <Button type="button" size="sm" variant="outline" onClick={() => document.getElementById(refusal ? "placement" : "script")?.scrollIntoView({ behavior: "smooth" })}>{refusal ? "Edit Placement" : "Edit the script"}</Button>
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
            <Tile label="Serving" value={`${counts.healthy} of ${spec.enabled ? spec.target_replicas : 0}`} detail={[counts.queued ? `${counts.queued} queued` : null, counts.running ? `${counts.running} loading` : null, `healthy at ${Math.max(1, spec.min_replicas)}`].filter(Boolean).join(", ")} />
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
                {replicasUnread ? "obleth couldn't read the replicas just now. It tries again every 10 seconds." : !spec.enabled ? "Paused: no jobs are running. Resume to launch again." : needsYou ? "No jobs are running." : provisionerOk ? "No jobs yet. The provisioner submits one on its next pass." : "No jobs yet, and none are submitted until the provisioner runs."}
              </div>
            ) : (
              <div className="grid gap-3 xl:grid-cols-2">
                {current.map((r, i) => (
                  <ReplicaCard key={r.id} r={r} letter={String.fromCharCode(65 + (i % 26))} spec={spec} stale={!provisionerOk} pending={pending} onRestart={() => restartOne(r, String.fromCharCode(65 + (i % 26)))} />
                ))}
              </div>
            )}
            {lost.length > 0 && (
              <details className="rounded-xl border border-border bg-card px-[18px] py-3" open={stopped}>
                <summary className="cursor-pointer text-[13px] font-medium">Jobs that ended · {lost.length}{endedSplit(lost)}</summary>
                <div className="mt-2">
                  <div className="grid grid-cols-[100px_110px_minmax(0,1fr)_80px_90px] gap-3 py-2 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground"><span>Job</span><span>Node</span><span>Last from Slurm</span><span>Ran</span><span>Ended</span></div>
                  {lost.slice(0, 10).map((r) => (
                    <div key={r.id} className="grid grid-cols-[100px_110px_minmax(0,1fr)_80px_90px] gap-3 border-t border-border py-2 text-[12.5px]">
                      <span className="font-mono text-[12px]">{r.slurm_job_id || "—"}</span>
                      <span className="truncate font-mono text-[12px]">{r.nodes || "—"}</span>
                      <span className="truncate" title={r.state_detail ?? r.last_message ?? undefined}>{r.state_detail || r.last_message || "job gone"}</span>
                      <span className="font-mono text-[12px]">{r.job_started_at ? duration(new Date(r.updated_at).getTime() - new Date(r.job_started_at).getTime()) : "—"}</span>
                      <span className="font-mono text-[12px] text-muted-foreground">{new Date(r.updated_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</span>
                    </div>
                  ))}
                  <p className="border-t border-border pt-2 text-xs text-muted-foreground"><JobOutput dir={spec.log_output_dir} /></p>
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
            validate={(d) => overLimit(String(d.get("slurm_partition") ?? ""), String(d.get("slurm_time_limit") ?? ""))}
            onSaved={() => { setNotice({ text: surge && current.length ? "Saved. New jobs use it. Use Apply to running replicas under More actions to replace the running ones one at a time, without an outage." : "Saved. Replicas started from now use it; restart the running ones to pick it up now." }); void refresh(); }}
            onDirtyChange={onDirty}
            className="flex flex-col gap-4"
            ariaLabel={`${model.model_name} launch settings`}
          >
            <input type="hidden" name="slurm_target_replicas" value={String(spec.target_replicas)} />
            <ScriptAndPlacement spec={spec} />
            <SettingsCard id="service" title="Service" description="How obleth checks a replica, and when it gives up.">
              <Setting id="set-port" label="Port and health path" hint={`obleth gives each replica ${PORT_WINDOW} ports starting near this one and passes a free one to the job as $OBLETH_SERVING_PORT. The server must listen on $OBLETH_SERVING_PORT, not on a fixed port. obleth checks the health path on that port until it answers.`} fields={["slurm_serving_port", "slurm_health_path"]}>
                <div className="flex gap-2">
                  <TextField name="slurm_serving_port" label="Port" inputMode="numeric" required defaultValue={spec.serving_port} mono className="w-24" />
                  <TextField name="slurm_health_path" label="Health path" defaultValue={spec.health_path} mono className="w-40" />
                </div>
              </Setting>
              <Setting id="set-min" label="Healthy when" hint={`Models calls it healthy once this many replicas answer, and degraded below that. Requests go to every healthy replica either way. The replica count (${spec.target_replicas}) is set at the top.`} fields={["slurm_min_replicas"]} was={{ field: "slurm_min_replicas" }}>
                <TextField name="slurm_min_replicas" label="Healthy when" inputMode="numeric" required defaultValue={Math.max(1, spec.min_replicas)} mono className="w-20" />
              </Setting>
              <Setting id="set-failures" label="Stop after failed launches" hint={surge ? "After this many launches in a row fail (a job that ends, or doesn't answer, before it ever serves), obleth waits longer and longer before trying again, and shows it here. A job that served and then hit its walltime or was preempted doesn't count. Set 0 to never stop." : "obleth stops submitting once this many jobs have ended in the last 15 minutes, and waits for you. Set 0 to never stop."} fields={["slurm_max_job_failures"]} was={{ field: "slurm_max_job_failures" }}>
                <TextField name="slurm_max_job_failures" label="Stop after failed launches" inputMode="numeric" required defaultValue={spec.max_job_failures} mono className="w-20" />
              </Setting>
              {spec.startup_timeout_mins !== undefined && (
                <Setting id="set-startup" label="Give up loading after" hint="Minutes a running job may take to answer on its health path before obleth cancels it and counts a failed launch. Set 0 to wait forever." fields={["slurm_startup_timeout_mins"]} was={{ field: "slurm_startup_timeout_mins" }}>
                  <TextField name="slurm_startup_timeout_mins" label="Give up loading after" inputMode="numeric" defaultValue={spec.startup_timeout_mins} mono className="w-20" />
                </Setting>
              )}
              {spec.handoff_mins !== undefined && (
                <Setting id="set-handoff" label="Start a replacement" hint="Minutes before a serving job's time limit that obleth starts its replacement; the old job stops once the new one serves. Set 0 to turn this off." fields={["slurm_handoff_mins"]} was={{ field: "slurm_handoff_mins" }}>
                  <TextField name="slurm_handoff_mins" label="Start a replacement" inputMode="numeric" defaultValue={spec.handoff_mins} mono className="w-20" />
                </Setting>
              )}
            </SettingsCard>
          </SettingsForm>

          <ChangesList changes={changes} />
        </div>
      </div>

      <RemoveDialog open={removing} onClose={() => setRemoving(false)} name={model.model_name} onRemove={remove} pending={pending} />
    </div>
  );
}
