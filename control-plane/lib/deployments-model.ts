import type { CapacityDiscoveryModelView, ClusterResources, ManagedModelSpec, ModelReplica, ModelRoute, SlurmSettingsView } from "@/lib/obleth";

/**
 * The Deployments pages' pure logic: which models are deployments, what state
 * each is in and why, and the Slurm arithmetic behind "does it fit".
 *
 * Two kinds: models obleth launches on Slurm (a managed spec plus replica
 * rows), and models a Kubernetes cluster runs that obleth only watches
 * (capacity discovery reads their Service).
 */

export type DeployKind = "slurm" | "kubernetes";

export type DeployState = "serving" | "starting" | "paused" | "stopped" | "blocked" | "not-running" | "no-replicas" | "stale";

export const STATE_LABEL: Record<DeployState, string> = {
  serving: "Serving",
  starting: "Starting",
  paused: "Paused",
  stopped: "Stopped",
  blocked: "Can't start",
  "not-running": "Not running",
  "no-replicas": "No replicas ready",
  stale: "Not seen lately",
};

/** Filled dot, half dot, dashed ring, or the inverted "needs you" pill. */
export type Glyph = "on" | "half" | "off" | "attention";

export function glyphOf(state: DeployState): Glyph {
  if (state === "serving") return "on";
  if (state === "starting") return "half";
  if (state === "stopped" || state === "blocked" || state === "no-replicas") return "attention";
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
  if (r.state === "draining") return "draining";
  if (r.state === "lost") return "lost";
  const s = slurmStatus(r.last_message).state;
  // Slurm requeues a preempted job, or one whose node failed, under the same
  // job id. An older gateway leaves the row healthy, but nothing serves.
  if (r.state === "healthy") return s === "PENDING" ? "queued" : "serving";
  return s === "RUNNING" || s === "COMPLETING" || r.state === "starting" ? "running" : "queued";
}

/** Whether Slurm put a job that had served back in the queue. */
export function wasRequeued(r: Pick<ModelReplica, "state" | "last_message" | "job_started_at">): boolean {
  const queued = slurmStatus(r.last_message).state === "PENDING";
  return queued && (r.state === "healthy" || (r.state === "pending" && !!r.job_started_at));
}

/** What Slurm's reason for holding a job in the queue means, and whether it
 *  will ever start without someone changing something. */
export interface PendingReason {
  text: string;
  /** True when the job won't start until a setting changes. */
  blocks: boolean;
}

const PENDING_REASONS: [RegExp, string, boolean][] = [
  [/^Resources$/i, "No node that fits is free yet. It starts when one frees up.", false],
  [/^Priority$/i, "Other jobs are ahead of it in the queue.", false],
  [/^(Dependency|BeginTime)$/i, "It is waiting for a start time or for another job.", false],
  [/^(QOS|Assoc)MaxWallDurationPerJobLimit$/i, "Its walltime is longer than the QoS or account allows, so Slurm never starts it. Shorten the walltime under Placement or pick another QoS.", true],
  [/^PartitionTimeLimit$/i, "Its walltime is longer than the partition allows, so Slurm never starts it. Shorten the walltime under Placement.", true],
  [/^PartitionNodeLimit$/i, "It asks for more nodes than the partition allows, so Slurm never starts it. Lower Nodes per replica or pick another partition.", true],
  [/^PartitionConfig$/i, "The partition doesn't allow what the job asks for, such as its account or QoS, so Slurm never starts it. Change them under Placement.", true],
  [/^(QOS|Assoc)\w*PerJob\w*$/i, "One job asks for more than the QoS or account allows per job, so Slurm never starts it. Ask for fewer GPUs, fewer CPUs or less memory under Placement, or pick another QoS.", true],
  [/^(QOS|Assoc)\w*(PerUser|PerAccount)\w*$/i, "Your Slurm user is at a QoS or account limit. It starts when some of your other jobs end.", false],
  [/^(QOS|Assoc)Grp\w*$/i, "The QoS or account is using everything it is allowed right now. It starts when other jobs under it end.", false],
  [/^BadConstraints$/i, "No node has the features or resources it asks for, so Slurm never starts it. Check Constraints and Excluded nodes under Placement.", true],
  [/^ReqNodeNotAvail/i, "The nodes it can run on are down, drained or reserved right now. It starts when one comes back.", false],
  [/^Invalid(Account|QOS)$/i, "Slurm doesn't accept its account or QoS, so it never starts. Change them under Placement.", true],
  [/^DependencyNeverSatisfied$/i, "It depends on a job that can't finish, so it never starts. Restart the replica.", true],
  [/^JobHeld(Admin|User)$/i, "The job is held, so Slurm won't start it until it is released. Restart the replica, or release it with scontrol release.", true],
  [/held/i, "Slurm couldn't start the job and held it. Restart the replica, and if it happens again, read the job's output.", true],
  [/^(PartitionDown|PartitionInactive)$/i, "The partition is down. It starts when an administrator brings it back.", false],
  [/^Reservation$/i, "It is waiting for its reservation to start.", false],
  [/^NodeDown$/i, "A node it was given is down. Slurm starts it again when it can.", false],
  [/^Licenses$/i, "It is waiting for licenses.", false],
  [/^(QOS|Assoc)/i, "It is waiting on a QoS or account limit. It starts when that limit allows.", false],
];

/** Slurm's pending reason in a sentence, or null for one this doesn't know. */
export function pendingReason(reason: string | null | undefined): PendingReason | null {
  const r = (reason ?? "").trim();
  if (!r) return null;
  for (const [re, text, blocks] of PENDING_REASONS) if (re.test(r)) return { text, blocks };
  return null;
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

type SpecForState = Pick<ManagedModelSpec, "enabled" | "target_replicas" | "max_job_failures" | "last_provision_error"> &
  Partial<Pick<ManagedModelSpec, "min_replicas" | "paused_reason" | "paused_until" | "startup_timeout_mins" | "handoff_mins">>;

/**
 * Whether the gateway pauses launches itself after failed launches (and says
 * so in `paused_reason`/`paused_until`). An older one stops once the lost rows
 * reach `max_job_failures`, counting every job that ended.
 */
export function pausesOnFailures(spec: Partial<Pick<ManagedModelSpec, "paused_reason" | "paused_until" | "startup_timeout_mins">>): boolean {
  return spec.startup_timeout_mins !== undefined || spec.paused_reason !== undefined || spec.paused_until !== undefined;
}

/** A failure pause in force: why, and when submits resume on their own (null: when someone says so). */
export function pauseOf(spec: Partial<Pick<ManagedModelSpec, "paused_reason" | "paused_until">>, now = Date.now()): { reason: string; until: Date | null } | null {
  const until = spec.paused_until ? new Date(spec.paused_until) : null;
  const valid = until && !Number.isNaN(until.getTime()) ? until : null;
  const active = valid ? valid.getTime() > now : !!spec.paused_reason?.trim();
  if (!active) return null;
  return { reason: spec.paused_reason?.trim() || "Too many launches failed.", until: valid };
}

/** "2:20 PM" today, "Nov 12, 2:20 PM" on another day. */
export function clockLabel(at: Date | string, now = Date.now()): string {
  const d = typeof at === "string" ? new Date(at) : at;
  const sameDay = new Date(now).toDateString() === d.toDateString();
  return sameDay ? d.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }) : d.toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
}

/** The healthy-replica count at which Models calls the model healthy (at least 1). */
export function healthyFloor(spec: Partial<Pick<ManagedModelSpec, "min_replicas">>): number {
  return Math.max(1, Math.floor(spec.min_replicas ?? 1) || 1);
}

/**
 * A Slurm deployment's state. It serves once `min_replicas` replicas are
 * healthy (requests go to every healthy replica either way). A pause after
 * failed launches, a refused submit, or a queued job Slurm will never start
 * needs someone.
 */
export function managedState(spec: SpecForState, replicas: Pick<ModelReplica, "state" | "last_message">[], now = Date.now()): StateView {
  const c = countReplicas(replicas);
  if (!spec.enabled) return { state: "paused", why: c.healthy + c.queued + c.running + c.draining > 0 ? "stopping its jobs" : null, attention: false };
  const pause = pauseOf(spec, now);
  if (pause) {
    const when = pause.until ? `tries again at ${clockLabel(pause.until, now)}` : "waits for you";
    return { state: "stopped", why: `launches paused, ${when}${c.healthy ? ` · ${c.healthy} still serving` : ""}`, attention: true };
  }
  if (!pausesOnFailures(spec) && spec.max_job_failures > 0 && c.lost >= spec.max_job_failures) {
    return { state: "stopped", why: `${c.lost} jobs ended${c.healthy ? ` · ${c.healthy} still serving` : ""}`, attention: true };
  }
  const queued = replicas.filter((r) => replicaPhase(r) === "queued");
  const blocked = queued.map((r) => slurmStatus(r.last_message).reason).filter((x): x is string => !!x && !!pendingReason(x)?.blocks);
  const waiting = queued.map((r) => slurmStatus(r.last_message).reason).find((x) => !!x) ?? null;
  const refused = !!spec.last_provision_error?.trim();
  const parts = (serving: boolean) => [
    serving ? `${c.healthy} of ${spec.target_replicas} serving` : c.healthy ? `${c.healthy} serving` : null,
    blocked.length ? `${blocked.length} can't start: ${blocked[0]}` : null,
    c.queued - blocked.length > 0 ? `${c.queued - blocked.length} queued${!blocked.length && waiting ? `: ${waiting}` : ""}` : null,
    c.running ? `${c.running} loading` : null,
    refused && c.healthy < spec.target_replicas ? "Slurm refused a job" : null,
  ].filter(Boolean).join(" · ");
  if (c.healthy > 0 && c.healthy >= healthyFloor(spec)) {
    if (c.healthy >= spec.target_replicas && !blocked.length) return { state: "serving", why: null, attention: false };
    return { state: "serving", why: parts(true), attention: blocked.length > 0 || (refused && c.healthy < spec.target_replicas) };
  }
  if (refused && c.running === 0 && c.queued === blocked.length) return { state: "blocked", why: c.healthy ? parts(false) : "Slurm refused the job", attention: true };
  if (blocked.length && c.running === 0 && c.queued === blocked.length) return { state: "blocked", why: parts(false), attention: true };
  if (c.queued + c.running > 0) return { state: "starting", why: parts(false), attention: blocked.length > 0 || refused };
  return { state: c.healthy ? "starting" : "not-running", why: "waiting for the provisioner", attention: false };
}

/** A Needs-you line for a deployment, e.g. "glm stopped" or "glm can't start". */
export function attentionWords(state: DeployState): string {
  if (state === "stopped") return "stopped";
  if (state === "blocked") return "can't start";
  if (state === "no-replicas") return "has no replicas";
  return "needs a look";
}

/** Which replica a scale-down by one cancels, as the provisioner picks it:
 *  the newest queued job, then a loading one, then the oldest serving one. */
export function scaleDownVictim<R extends Pick<ModelReplica, "state" | "last_message" | "created_at">>(replicas: R[]): R | null {
  const live = replicas.filter((r) => r.state !== "lost" && r.state !== "draining");
  const by = (phase: ReplicaPhase) => live.filter((r) => replicaPhase(r) === phase);
  const newest = (xs: R[]) => [...xs].sort((a, b) => b.created_at.localeCompare(a.created_at))[0];
  const oldest = (xs: R[]) => [...xs].sort((a, b) => a.created_at.localeCompare(b.created_at))[0];
  return newest(by("queued")) ?? newest(by("running")) ?? oldest(by("serving")) ?? null;
}

/** What to change when Slurm refuses a submit, from its message. Null when unknown. */
export function refusalAdvice(message: string): string | null {
  const m = message.toLowerCase();
  if (/invalid account|account\/partition/.test(m)) return "The account can't run jobs on this partition. Change the account or the partition under Placement.";
  if (/accounting\/qos policy|qos policy|association/.test(m)) return "The job breaks an account or QoS limit, such as how many jobs or GPUs the Slurm user may have. Ask for less under Placement or pick another QoS.";
  if (/invalid qos|qos.*(not|invalid)/.test(m)) return "Slurm doesn't accept the QoS for this account or partition. Pick another QoS under Placement, or leave it blank for the default.";
  if (/time limit/.test(m)) return "The walltime is longer than the partition or QoS allows. Shorten it under Placement.";
  if (/generic resource|gres/.test(m)) return "Slurm doesn't know the GPUs value. Check GPUs under Placement, such as gpu:1 or gpu:a100:1.";
  if (/invalid partition/.test(m)) return "There is no partition by that name, or the Slurm user can't use it. Pick another partition under Placement.";
  if (/node configuration is not available/.test(m)) return "No node in the partition has what one job asks for. Ask for fewer GPUs, fewer CPUs or less memory under Placement, or pick another partition.";
  if (/memory/.test(m)) return "No node has that much memory. Ask for less under Placement.";
  if (/node count|nodes? requested/.test(m)) return "The partition doesn't have that many nodes. Lower Nodes per replica under Placement.";
  if (/\b(401|403|511)\b|unauthori[sz]ed|authentication|token/.test(m)) return "slurmrestd rejected the Slurm token. Mint a new one and paste it on the Slurm connection.";
  if (/\b404\b|not found/.test(m)) return "slurmrestd doesn't serve that API version. Set API version to automatic on the Slurm connection.";
  if (/timed? ?out|connection refused|unreachable|error sending request/.test(m)) return "obleth couldn't reach slurmrestd. It tries again on each pass; test the Slurm connection to see why.";
  return null;
}

/** When a running job hits its time limit, and when its replacement starts. */
export function jobEndLine(endsAt: string | null | undefined, handoffMins: number | null | undefined, now = Date.now()): string | null {
  if (!endsAt) return null;
  const end = new Date(endsAt);
  if (Number.isNaN(end.getTime())) return null;
  const hand = handoffMins ?? 0;
  if (hand <= 0) return `Time limit at ${clockLabel(end, now)}; a new job is submitted when this one ends.`;
  const start = new Date(end.getTime() - hand * 60_000);
  return start.getTime() <= now ? `Time limit at ${clockLabel(end, now)}; a replacement has been submitted.` : `Time limit at ${clockLabel(end, now)}; a replacement starts at ${clockLabel(start, now)}.`;
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

/**
 * What one replica's job asks Slurm for, as a sentence:
 * "Each replica asks Slurm for 1 node with 1 GPU, 72 CPUs and 500 GB of memory."
 * A blank box is said as the default it falls back to.
 */
export function replicaAsk({ nodes, gres, cpus, mem }: { nodes: number; gres: string; cpus: string; mem: string }): string {
  const n = Math.max(1, Math.floor(nodes) || 1);
  const g = gres.trim();
  const gpus = gresCount(g);
  const c = cpus.trim();
  const m = mem.trim();
  const mb = memToMb(m);
  const parts = [
    !g ? "no GPUs" : gpus ? `${gpus} GPU${gpus === 1 ? "" : "s"}` : g,
    !c ? "Slurm's default CPUs" : `${c} CPU${c === "1" ? "" : "s"}`,
    !m ? "the partition's default memory" : mb === 0 ? "all of its memory" : mb ? `${mb >= 1024 ? `${Math.round(mb / 1024)} GB` : `${mb} MB`} of memory` : `${m} of memory`,
  ];
  const list = `${parts.slice(0, -1).join(", ")} and ${parts[parts.length - 1]}`;
  return n === 1 ? `Each replica asks Slurm for 1 node with ${list}.` : `Each replica asks Slurm for ${n} nodes, each with ${list}.`;
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

export type NodeState = "idle" | "busy" | "asleep" | "down";

/**
 * Slurm's state flags as idle, busy (allocated or mixed), asleep (powered
 * down by power saving, or a cloud node not started; Slurm starts it when a
 * job needs it), or down (down, drain, fail, maintenance, reserved).
 */
export function nodeState(flags: string[] | undefined): NodeState {
  const f = (flags ?? []).map((x) => x.toUpperCase());
  if (f.some((x) => ["DOWN", "DRAIN", "DRAINING", "DRAINED", "FAIL", "FAILING", "MAINTENANCE", "NOT_RESPONDING", "RESERVED"].includes(x))) return "down";
  if (f.some((x) => ["ALLOCATED", "MIXED", "COMPLETING"].includes(x))) return "busy";
  if (f.some((x) => ["POWERED_DOWN", "POWERING_UP", "POWERING_DOWN", "POWER_DOWN", "CLOUD"].includes(x))) return "asleep";
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
  /** True when one replica fits a node, or when the nodes couldn't be read (never block on what isn't known). */
  fits: boolean;
  /** False when the cluster listed no nodes for the partition, so the fit is unknown. */
  known: boolean;
  /** Why not, in one line: "needs 560G, nodes have 512G". */
  reason: string | null;
  nodes: { name: string; state: NodeState; fits: boolean }[];
  /** Nodes that fit and are idle or asleep right now (state known), or null when states aren't reported. */
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
    const known = members.length > 0;
    if (fitting.length === 0 && biggest) {
      if (nodeGpus(biggest) < need.gpus) reason = need.gpus && !nodeGpus(biggest) ? "no GPUs" : `needs ${need.gpus} GPU, nodes have ${nodeGpus(biggest)}`;
      else if (need.memMb != null && biggest.real_memory_mb != null && biggest.real_memory_mb < need.memMb) reason = `needs ${mbLabel(need.memMb)}, nodes have ${mbLabel(biggest.real_memory_mb)}`;
      else if (need.cpus != null && biggest.cpus != null && biggest.cpus < need.cpus) reason = `needs ${need.cpus} CPU, nodes have ${biggest.cpus}`;
      else reason = "no node matches";
    }
    const max = p.max_time ? Number(p.max_time) : null;
    return {
      name: p.name,
      fits: !known || fitting.length > 0,
      known,
      reason,
      nodes: members.map((n) => ({ name: n.name, state: nodeState(n.state), fits: fitsNode(n) })),
      idleFitting: reported ? fitting.filter((n) => ["idle", "asleep"].includes(nodeState(n.state))).length : null,
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
  /** True when `accounts` are the Slurm user's own (its associations), not every account on the cluster. */
  accountsAreUsers?: boolean;
  qos: string;
  qosList: string[];
}): Check[] {
  const out: Check[] = [];
  const { fit } = input;
  if (fit) {
    out.push(!fit.known ? { ok: true, text: `obleth couldn't read ${fit.name}'s nodes, so it can't check the fit` } : fit.fits ? { ok: true, text: `One replica fits a ${fit.name} node` } : { ok: false, text: `Doesn't fit ${fit.name}: ${fit.reason}` });
    if (input.minutes && fit.maxMinutes) out.push(input.minutes <= fit.maxMinutes ? { ok: true, text: `${walltimeLabel(input.minutes)} is inside ${fit.name}'s ${walltimeLabel(fit.maxMinutes)} limit` } : { ok: false, text: `${walltimeLabel(input.minutes)} is over ${fit.name}'s ${walltimeLabel(fit.maxMinutes)} limit` });
  } else {
    out.push({ ok: false, text: "Pick a partition" });
  }
  if (input.account && input.accounts.length) {
    const has = input.accounts.includes(input.account);
    out.push(input.accountsAreUsers
      ? has ? { ok: true, text: `Your Slurm user can charge ${input.account}` } : { ok: false, text: `Your Slurm user has no association with ${input.account}` }
      : has ? { ok: true, text: `${input.account} is an account on this cluster (obleth couldn't read which accounts your Slurm user has)` } : { ok: false, text: `There's no account called ${input.account} on this cluster` });
  }
  if (input.qos && input.qosList.length) out.push(input.qosList.includes(input.qos) ? { ok: true, text: `QoS ${input.qos} exists` } : { ok: false, text: `There's no QoS called ${input.qos} on this cluster` });
  out.push(input.nameFree ? { ok: true, text: `The name ${input.name || "…"} is free` } : { ok: false, text: `A model called ${input.name} already exists` });
  return out;
}

// ---------------------------------------------------------------------------
// The Slurm connection and the provisioner
// ---------------------------------------------------------------------------

/** The command that mints a Slurm token good for 30 days. */
export const MINT_TOKEN = "scontrol token lifespan=2592000";

/** "30 minutes", "2 days", "5 hours". */
export function roughDuration(secs: number): string {
  const s = Math.max(0, Math.round(secs));
  if (s < 90) return `${s} seconds`;
  if (s < 5400) return `${Math.round(s / 60)} minutes`;
  if (s < 2 * 86_400) return `${Math.round(s / 3600)} hours`;
  return `${Math.round(s / 86_400)} days`;
}

/**
 * The stored Slurm token's expiry as a sentence, and whether it needs someone
 * (expired, or expires within 3 days). Null when the gateway doesn't say.
 */
export function tokenExpiry(s: Pick<SlurmSettingsView, "jwt_set"> & Partial<Pick<SlurmSettingsView, "jwt_expires_at" | "jwt_expires_in_secs" | "jwt_user">>, now = Date.now()): { text: string; attention: boolean; expired: boolean } | null {
  if (!s.jwt_set) return null;
  const at = s.jwt_expires_at ? new Date(s.jwt_expires_at) : null;
  const secs = s.jwt_expires_in_secs ?? (at && !Number.isNaN(at.getTime()) ? (at.getTime() - now) / 1000 : null);
  if (secs == null) return s.jwt_expires_at === null || s.jwt_expires_in_secs === null ? { text: "The Slurm token doesn't expire.", attention: false, expired: false } : null;
  const who = s.jwt_user ? ` for ${s.jwt_user}` : "";
  const when = at && !Number.isNaN(at.getTime()) ? ` (${clockLabel(at, now)})` : "";
  if (secs <= 0) return { text: `The Slurm token${who} has expired${at ? ` at ${clockLabel(at, now)}` : ""}, so obleth can't launch or check jobs.`, attention: true, expired: true };
  return { text: `The Slurm token${who} expires in ${roughDuration(secs)}${when}.`, attention: secs < 3 * 86_400, expired: false };
}

export type ProvisionerHealth =
  | { kind: "off" }
  | { kind: "unread" }
  | { kind: "never" }
  | { kind: "stopped"; lastSeenSecs: number | null }
  | { kind: "failing"; detail: string | null; lastOkSecs: number | null }
  | { kind: "ok" };

/**
 * What the provisioner is doing, from the settings the gateway reports. A
 * provisioner that checks in while its passes fail leaves replica states
 * frozen, so that counts as failing once it has gone on for a minute.
 */
export function provisionerHealth(s: SlurmSettingsView | null | undefined): ProvisionerHealth {
  if (!s) return { kind: "unread" };
  if (!s.enabled) return { kind: "off" };
  if (!s.provisioner_running) return s.provisioner_last_seen_secs == null ? { kind: "never" } : { kind: "stopped", lastSeenSecs: s.provisioner_last_seen_secs };
  const failing = s.provisioner_tick_status === "error" && ((s.provisioner_held_secs ?? 0) > 60 || (s.provisioner_last_ok_secs ?? 0) > 60 || s.provisioner_last_ok_secs == null);
  if (failing) return { kind: "failing", detail: s.provisioner_tick_detail ?? null, lastOkSecs: s.provisioner_last_ok_secs ?? null };
  return { kind: "ok" };
}

/** How to start the provisioner, for each way obleth ships. */
export const START_PROVISIONER = "Start obleth-provisioner: on Kubernetes set provisioner.enabled=true; with Docker Compose add slurm to COMPOSE_PROFILES; with systemd enable obleth-provisioner.service.";

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
