import type { CapacityDiscoveryModelView, ClusterResources, ManagedModelSpec, ModelReplica, ModelRoute } from "@/lib/obleth";

/**
 * The Deployments pages' pure logic: which models are deployments, what state
 * each is in and why, and the Slurm arithmetic behind "does it fit".
 *
 * Two kinds: models obleth launches on Slurm (a managed spec plus replica
 * rows), and models a Kubernetes cluster runs that obleth only watches
 * (capacity discovery reads their Service).
 */

export type DeployKind = "slurm" | "kubernetes";

export type DeployState = "serving" | "starting" | "paused" | "stopped" | "not-running" | "no-replicas" | "stale";

export const STATE_LABEL: Record<DeployState, string> = {
  serving: "Serving",
  starting: "Starting",
  paused: "Paused",
  stopped: "Stopped",
  "not-running": "Not running",
  "no-replicas": "No replicas ready",
  stale: "Not seen lately",
};

/** Filled dot, half dot, dashed ring, or the inverted "needs you" pill. */
export type Glyph = "on" | "half" | "off" | "attention";

export function glyphOf(state: DeployState): Glyph {
  if (state === "serving") return "on";
  if (state === "starting") return "half";
  if (state === "stopped" || state === "no-replicas") return "attention";
  return "off";
}

export interface ReplicaCounts {
  healthy: number;
  queued: number;
  running: number;
  draining: number;
  lost: number;
}

/** Slurm's job state and reason from a replica's message, e.g. "PENDING — Resources". */
export function slurmStatus(message: string | null | undefined): { state: string; reason: string | null } {
  const m = (message ?? "").trim();
  if (!m) return { state: "", reason: null };
  const [state, ...rest] = m.split(" — ");
  return { state: state.trim().toUpperCase(), reason: rest.join(" — ").trim() || null };
}

/** Where a replica is in its start: waiting for Slurm, running but not yet healthy, or serving. */
export type ReplicaPhase = "queued" | "running" | "serving" | "draining" | "lost";

export function replicaPhase(r: Pick<ModelReplica, "state" | "last_message">): ReplicaPhase {
  if (r.state === "healthy") return "serving";
  if (r.state === "draining") return "draining";
  if (r.state === "lost") return "lost";
  const s = slurmStatus(r.last_message).state;
  return s === "RUNNING" || s === "COMPLETING" || r.state === "starting" ? "running" : "queued";
}

export function countReplicas(replicas: Pick<ModelReplica, "state" | "last_message">[]): ReplicaCounts {
  const out: ReplicaCounts = { healthy: 0, queued: 0, running: 0, draining: 0, lost: 0 };
  for (const r of replicas) {
    const p = replicaPhase(r);
    if (p === "serving") out.healthy += 1;
    else if (p === "queued") out.queued += 1;
    else if (p === "running") out.running += 1;
    else if (p === "draining") out.draining += 1;
    else out.lost += 1;
  }
  return out;
}

export interface StateView {
  state: DeployState;
  /** A short line under the status: "1 queued: Resources", "3 failed launches". */
  why: string | null;
  attention: boolean;
}

/**
 * A Slurm deployment's state. The provisioner stops submitting once the lost
 * replicas reach `max_job_failures`, so that is "stopped" even with some
 * replicas still serving.
 */
export function managedState(spec: Pick<ManagedModelSpec, "enabled" | "target_replicas" | "max_job_failures" | "last_provision_error">, replicas: Pick<ModelReplica, "state" | "last_message">[]): StateView {
  const c = countReplicas(replicas);
  if (!spec.enabled) return { state: "paused", why: c.healthy + c.queued + c.running + c.draining > 0 ? "stopping its jobs" : null, attention: false };
  if (spec.max_job_failures > 0 && c.lost >= spec.max_job_failures) {
    return { state: "stopped", why: `${c.lost} failed launch${c.lost === 1 ? "" : "es"}${c.healthy ? ` · ${c.healthy} still serving` : ""}`, attention: true };
  }
  if (spec.last_provision_error && c.healthy === 0) return { state: "stopped", why: "Slurm refused the job", attention: true };
  if (c.healthy >= spec.target_replicas) return { state: "serving", why: null, attention: false };
  if (c.queued + c.running > 0) {
    const waiting = replicas.find((r) => replicaPhase(r) === "queued");
    const reason = waiting ? slurmStatus(waiting.last_message).reason : null;
    const parts = [c.queued ? `${c.queued} queued${reason ? `: ${reason}` : ""}` : null, c.running ? `${c.running} loading` : null].filter(Boolean);
    return { state: "starting", why: parts.join(" · "), attention: false };
  }
  return { state: c.healthy ? "starting" : "not-running", why: "waiting for the provisioner", attention: false };
}

/** A watched Kubernetes deployment's state, from what discovery last saw. */
export function watchedState(d: CapacityDiscoveryModelView | undefined, model: Pick<ModelRoute, "enabled">): StateView {
  if (!model.enabled) return { state: "paused", why: "turned off in Models", attention: false };
  if (!d?.status) return { state: "stale", why: "discovery hasn't reported it", attention: false };
  if (d.status.state === "stale" || d.status.state === "fallback") return { state: "stale", why: d.status.reason ?? "the Service couldn't be read", attention: false };
  if (!d.status.ready_replicas) return { state: "no-replicas", why: "requests wait, then fail", attention: true };
  return { state: "serving", why: null, attention: false };
}

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

export interface DeploymentRow {
  model: ModelRoute;
  kind: DeployKind;
  view: StateView;
  ready: number;
  /** Replicas obleth keeps (Slurm); null when the cluster decides. */
  wanted: number | null;
  /** Kubernetes: namespace / Service. Slurm: partition · gres · mem · walltime. */
  where: string;
  perReplica: number | null;
  poolCap: number;
  requests24h: number;
  spec?: ManagedModelSpec;
  replicas: ModelReplica[];
  discovery?: CapacityDiscoveryModelView;
  engine: string | null;
}

export function slurmPlacement(s: Pick<ManagedModelSpec, "partition" | "gres" | "mem" | "cpus_per_task" | "time_limit">): string {
  const mem = s.mem === "0" ? "all memory" : s.mem || null;
  return [s.partition, s.gres || null, s.cpus_per_task ? `${s.cpus_per_task} CPU` : null, mem, s.time_limit ? walltimeLabel(parseTimeLimit(s.time_limit)) : null].filter(Boolean).join(" · ");
}

/**
 * The accounts a partition takes from this Slurm user: their associations for
 * that partition (or for every partition), less what the partition's
 * AllowAccounts/DenyAccounts rule out. Null when the cluster doesn't report
 * enough to tell, so callers fall back to every account.
 */
export function accountsFor(resources: ClusterResources, partition: string): string[] | null {
  const assoc = resources.associations;
  const p = resources.partitions.find((x) => x.name === partition);
  if (!assoc?.length && !p?.allowed_accounts?.length && !p?.denied_accounts?.length) return null;
  const base = assoc?.length ? assoc.filter((a) => !a.partition || a.partition === partition).map((a) => a.account) : resources.accounts;
  const allowed = p?.allowed_accounts ?? [];
  const denied = new Set(p?.denied_accounts ?? []);
  return [...new Set(base)].filter((a) => (!allowed.length || allowed.includes(a)) && !denied.has(a)).sort();
}

/** Slurm's own words from a slurmrestd error body, e.g. "Invalid account or
 *  account/partition combination specified", or the text unchanged. */
export function slurmErrorText(raw: string): string {
  const errors = [...raw.matchAll(/"error"\s*:\s*"((?:[^"\\]|\\.)*)"/g)].map((m) => m[1]).filter(Boolean);
  const descs = [...raw.matchAll(/"description"\s*:\s*"((?:[^"\\]|\\.)*)"/g)].map((m) => m[1]).filter((d) => d && !/^Expected OpenAPI/.test(d));
  const picked = errors.length ? errors : descs;
  return picked.length ? [...new Set(picked)].join("; ") : raw;
}

export function buildDeploymentRows(
  models: ModelRoute[],
  specs: ManagedModelSpec[],
  replicas: ModelReplica[],
  discovery: CapacityDiscoveryModelView[],
  requests: Record<string, number>,
): DeploymentRow[] {
  const spec = new Map(specs.map((s) => [s.model_id, s]));
  const disc = new Map(discovery.map((d) => [d.model_id, d]));
  const rows: DeploymentRow[] = [];
  for (const m of models) {
    const s = spec.get(m.id);
    if (s) {
      const mine = replicas.filter((r) => r.model_id === m.id);
      const engine = typeof s.launcher_spec?.engine === "string" ? (s.launcher_spec.engine as string) : null;
      rows.push({
        model: m,
        kind: "slurm",
        view: managedState(s, mine),
        ready: countReplicas(mine).healthy,
        wanted: s.enabled ? s.target_replicas : 0,
        where: slurmPlacement(s),
        perReplica: null,
        poolCap: m.max_in_flight ?? 0,
        requests24h: requests[m.model_name] ?? 0,
        spec: s,
        replicas: mine,
        engine,
      });
      continue;
    }
    if (m.capacity_source === "kubernetes") {
      const d = disc.get(m.id);
      rows.push({
        model: m,
        kind: "kubernetes",
        view: watchedState(d, m),
        ready: d?.status?.ready_replicas ?? 0,
        wanted: null,
        where: d?.status?.namespace && d.status.service ? `${d.status.namespace} / ${d.status.service}` : m.capacity_service || "—",
        perReplica: d?.status?.per_replica_max_in_flight ?? m.per_replica_max_in_flight ?? null,
        poolCap: d?.status?.effective_max_in_flight ?? d?.enforced_max_in_flight ?? m.max_in_flight ?? 0,
        requests24h: requests[m.model_name] ?? 0,
        replicas: [],
        discovery: d,
        engine: null,
      });
    }
  }
  return rows;
}

export type RunsFilter = "all" | "kubernetes" | "slurm" | "attention";
export type DeploySort = "requests" | "name" | "replicas" | "attention";

export function filterDeployments(rows: DeploymentRow[], f: { query: string; runs: RunsFilter }): DeploymentRow[] {
  const q = f.query.trim().toLowerCase();
  return rows.filter((r) => {
    if (f.runs === "attention" ? !r.view.attention : f.runs !== "all" && r.kind !== f.runs) return false;
    if (!q) return true;
    const jobs = r.replicas.map((x) => `${x.slurm_job_id} ${x.nodes ?? ""}`).join(" ");
    return `${r.model.model_name} ${r.where} ${r.engine ?? ""} ${jobs}`.toLowerCase().includes(q);
  });
}

export function sortDeployments(rows: DeploymentRow[], sort: DeploySort): DeploymentRow[] {
  const byName = (a: DeploymentRow, b: DeploymentRow) => a.model.model_name.localeCompare(b.model.model_name);
  const out = [...rows];
  if (sort === "name") return out.sort(byName);
  if (sort === "replicas") return out.sort((a, b) => b.ready - a.ready || byName(a, b));
  // Problems first, then Slurm before the watched fleet, then busiest.
  const rank = (r: DeploymentRow) => (r.view.attention ? 0 : r.view.state === "starting" ? 1 : 2);
  if (sort === "attention") return out.sort((a, b) => rank(a) - rank(b) || b.requests24h - a.requests24h || byName(a, b));
  return out.sort((a, b) => Number(b.view.attention) - Number(a.view.attention) || b.requests24h - a.requests24h || byName(a, b));
}

/** "49 models on Kubernetes, 277 replicas ready · 2 on Slurm, 1 starting · 1 needs you" */
export function deploymentsLine(rows: DeploymentRow[]): string {
  const k8s = rows.filter((r) => r.kind === "kubernetes");
  const slurm = rows.filter((r) => r.kind === "slurm");
  const starting = slurm.filter((r) => r.view.state === "starting").length;
  const attention = rows.filter((r) => r.view.attention).length;
  return [
    k8s.length ? `${k8s.length} on Kubernetes, ${k8s.reduce((n, r) => n + r.ready, 0)} replicas ready` : null,
    slurm.length ? `${slurm.length} on Slurm${starting ? `, ${starting} starting` : ""}` : null,
    attention ? `${attention} need${attention === 1 ? "s" : ""} you` : null,
  ].filter(Boolean).join(" · ") || "Nothing deployed yet";
}

// ---------------------------------------------------------------------------
// Slurm arithmetic
// ---------------------------------------------------------------------------

/** GPUs a gres string asks for: "gpu:1" → 1, "gpu:h100:2" → 2, "" → 0. */
export function gresCount(gres: string | null | undefined): number {
  const g = (gres ?? "").trim();
  if (!g) return 0;
  return g.split(",").reduce((n, part) => {
    const bits = part.split(":");
    if (bits[0] !== "gpu") return n;
    const last = Number(bits[bits.length - 1]);
    return n + (Number.isFinite(last) && bits.length > 1 ? last : 1);
  }, 0);
}

/** "560G" → 573440 MB, "4096M" → 4096, "1T" → 1048576, "" → null. */
export function memToMb(mem: string | null | undefined): number | null {
  const m = /^(\d+(?:\.\d+)?)\s*([KMGT])?B?$/i.exec((mem ?? "").trim());
  if (!m) return null;
  const n = Number(m[1]);
  const unit = (m[2] ?? "M").toUpperCase();
  const mb = unit === "K" ? n / 1024 : unit === "M" ? n : unit === "G" ? n * 1024 : n * 1024 * 1024;
  return Math.round(mb);
}

export function mbLabel(mb: number | null | undefined): string {
  if (!mb) return "—";
  return mb >= 1024 ? `${Math.round(mb / 1024)}G` : `${mb}M`;
}

/** Slurm walltime to minutes: minutes, MM:SS, HH:MM:SS, D-HH, D-HH:MM, D-HH:MM:SS. */
export function parseTimeLimit(raw: string | null | undefined): number | null {
  const s = (raw ?? "").trim();
  if (!s) return null;
  let days = 0;
  let rest = s;
  const dash = s.indexOf("-");
  if (dash >= 0) {
    days = Number(s.slice(0, dash));
    rest = s.slice(dash + 1);
    const parts = rest.split(":").map(Number);
    if (!Number.isFinite(days) || parts.some((p) => !Number.isFinite(p))) return null;
    const [h = 0, m = 0, sec = 0] = parts;
    return days * 1440 + h * 60 + m + Math.ceil(sec / 60);
  }
  const parts = rest.split(":").map(Number);
  if (parts.some((p) => !Number.isFinite(p))) return null;
  if (parts.length === 1) return parts[0];
  if (parts.length === 2) return parts[0] + Math.ceil(parts[1] / 60);
  return parts[0] * 60 + parts[1] + Math.ceil(parts[2] / 60);
}

/** Minutes as Slurm's D-HH:MM:SS. */
export function formatTimeLimit(minutes: number): string {
  const d = Math.floor(minutes / 1440);
  const h = Math.floor((minutes % 1440) / 60);
  const m = minutes % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d}-${pad(h)}:${pad(m)}:00`;
}

/** "4 hours", "1 day", "2 days 12 hours". */
export function walltimeLabel(minutes: number | null | undefined): string {
  if (!minutes) return "no limit";
  const d = Math.floor(minutes / 1440);
  const h = Math.floor((minutes % 1440) / 60);
  const m = minutes % 60;
  const parts = [d ? `${d} day${d === 1 ? "" : "s"}` : null, h ? `${h} hour${h === 1 ? "" : "s"}` : null, m && !d ? `${m} min` : null].filter(Boolean);
  return parts.join(" ") || `${minutes} min`;
}

/** Presets up to a partition's limit, with the limit itself last. A max of 0 or none means no limit. */
export function walltimeChoices(maxMinutes: number | null): number[] {
  const presets = [240, 720, 1440, 2880];
  if (!maxMinutes) return presets;
  const under = presets.filter((p) => p < maxMinutes);
  return [...under, maxMinutes];
}

export type NodeState = "idle" | "busy" | "down";

/** Slurm's state flags as idle, busy (allocated or mixed), or down (down, drain, fail, maintenance). */
export function nodeState(flags: string[] | undefined): NodeState {
  const f = (flags ?? []).map((x) => x.toUpperCase());
  if (f.some((x) => ["DOWN", "DRAIN", "DRAINING", "DRAINED", "FAIL", "FAILING", "MAINTENANCE", "NOT_RESPONDING", "POWERED_DOWN", "RESERVED"].includes(x))) return "down";
  if (f.some((x) => ["ALLOCATED", "MIXED", "COMPLETING"].includes(x))) return "busy";
  return "idle";
}

type Node = ClusterResources["nodes"][number];

export interface ReplicaNeed {
  gpus: number;
  cpus: number | null;
  memMb: number | null;
}

export interface PartitionFit {
  name: string;
  fits: boolean;
  /** Why not, in one line: "needs 560G, nodes have 512G". */
  reason: string | null;
  nodes: { name: string; state: NodeState; fits: boolean }[];
  /** Nodes that fit and are idle right now (state known), or null when states aren't reported. */
  idleFitting: number | null;
  /** "1 GPU · 72 CPU · 574G per node", from the largest node. */
  shape: string;
  maxMinutes: number | null;
}

function nodeGpus(n: Node): number {
  return gresCount(n.gres.replace(/\(.*?\)/g, ""));
}

/** Whether one replica fits a node of each partition, and how many such nodes are idle. */
export function partitionFits(resources: ClusterResources, need: ReplicaNeed): PartitionFit[] {
  const nodes = resources.nodes as Node[];
  return resources.partitions.map((p) => {
    const members = nodes.filter((n) => n.partitions.includes(p.name));
    const fitsNode = (n: Node) => nodeGpus(n) >= need.gpus && (need.cpus == null || n.cpus == null || n.cpus >= need.cpus) && (need.memMb == null || n.real_memory_mb == null || n.real_memory_mb >= need.memMb);
    const fitting = members.filter(fitsNode);
    const reported = members.some((n) => (n.state ?? []).length > 0);
    const biggest = [...members].sort((a, b) => nodeGpus(b) - nodeGpus(a) || (b.real_memory_mb ?? 0) - (a.real_memory_mb ?? 0))[0];
    let reason: string | null = null;
    if (members.length === 0) reason = "no nodes listed";
    else if (fitting.length === 0 && biggest) {
      if (nodeGpus(biggest) < need.gpus) reason = need.gpus && !nodeGpus(biggest) ? "no GPUs" : `needs ${need.gpus} GPU, nodes have ${nodeGpus(biggest)}`;
      else if (need.memMb != null && biggest.real_memory_mb != null && biggest.real_memory_mb < need.memMb) reason = `needs ${mbLabel(need.memMb)}, nodes have ${mbLabel(biggest.real_memory_mb)}`;
      else if (need.cpus != null && biggest.cpus != null && biggest.cpus < need.cpus) reason = `needs ${need.cpus} CPU, nodes have ${biggest.cpus}`;
      else reason = "no node matches";
    }
    const max = p.max_time ? Number(p.max_time) : null;
    return {
      name: p.name,
      fits: fitting.length > 0,
      reason,
      nodes: members.map((n) => ({ name: n.name, state: nodeState(n.state), fits: fitsNode(n) })),
      idleFitting: reported ? fitting.filter((n) => nodeState(n.state) === "idle").length : null,
      shape: biggest ? [`${nodeGpus(biggest)} GPU`, biggest.cpus ? `${biggest.cpus} CPU` : null, biggest.real_memory_mb ? mbLabel(biggest.real_memory_mb) : null].filter(Boolean).join(" · ") + " per node" : "",
      maxMinutes: max && Number.isFinite(max) && max > 0 ? max : null,
    };
  });
}

export interface Check {
  ok: boolean;
  text: string;
}

/** What can be checked before submitting, from the cluster's own data. */
export function preflightChecks(input: {
  fit: PartitionFit | undefined;
  minutes: number | null;
  nameFree: boolean;
  name: string;
  account: string;
  accounts: string[];
  qos: string;
  qosList: string[];
}): Check[] {
  const out: Check[] = [];
  const { fit } = input;
  if (fit) {
    out.push(fit.fits ? { ok: true, text: `One replica fits a ${fit.name} node` } : { ok: false, text: `Doesn't fit ${fit.name}: ${fit.reason}` });
    if (input.minutes && fit.maxMinutes) out.push(input.minutes <= fit.maxMinutes ? { ok: true, text: `${walltimeLabel(input.minutes)} is inside ${fit.name}'s ${walltimeLabel(fit.maxMinutes)} limit` } : { ok: false, text: `${walltimeLabel(input.minutes)} is over ${fit.name}'s ${walltimeLabel(fit.maxMinutes)} limit` });
  } else {
    out.push({ ok: false, text: "Pick a partition" });
  }
  if (input.account && input.accounts.length) out.push(input.accounts.includes(input.account) ? { ok: true, text: `Your Slurm user can charge ${input.account}` } : { ok: false, text: `Your Slurm user has no association with ${input.account}` });
  if (input.qos && input.qosList.length) out.push(input.qosList.includes(input.qos) ? { ok: true, text: `QoS ${input.qos} exists` } : { ok: false, text: `QoS ${input.qos} isn't one your user has` });
  out.push(input.nameFree ? { ok: true, text: `The name ${input.name || "…"} is free` } : { ok: false, text: `A model called ${input.name} already exists` });
  return out;
}

/** "18 h 04 m", "6 m 24 s". */
export function duration(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const d = Math.floor(s / 86_400);
  const h = Math.floor((s % 86_400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (d) return `${d} d ${h} h`;
  if (h) return `${h} h ${String(m).padStart(2, "0")} m`;
  if (m) return `${m} m ${String(s % 60).padStart(2, "0")} s`;
  return `${s} s`;
}

/** A deployment's page address. */
export function deploymentHref(name: string, section?: string): string {
  return `/deployments/${encodeURIComponent(name)}${section ? `#${section}` : ""}`;
}
