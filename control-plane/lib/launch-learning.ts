// Suggestions from obleth's own launch history. Nothing here is applied on its
// own: the form shows each suggestion beside its field with the evidence, and
// the person launching picks it or not. Pure, so it is easy to test.
import type { DeployForm, FieldPath } from "@/lib/deploy-form";
import { formatTimeLimit, parseTimeLimit, walltimeLabel } from "@/lib/deployments-model";

/** The launch-history fields this module reads (a subset of the API row). */
export interface LaunchRecord {
  id: string;
  model_name: string;
  recipe_id: string | null;
  partition: string | null;
  account: string | null;
  qos: string | null;
  time_limit: string | null;
  nodes_requested: number | null;
  nodes: string | null;
  submitted_at: string;
  started_at: string | null;
  healthy_at: string | null;
  ended_at: string | null;
  end_state: string | null;
  queued_secs: number | null;
  load_secs: number | null;
}

export interface Suggestion {
  path: FieldPath;
  /** The value to apply, or null when it's advice without a one-click fix. */
  value: string | number | null;
  /** Short, for the side panel: "Avoid gh-017". */
  title: string;
  /** The evidence, for beside the field. */
  evidence: string;
}

export interface Learned {
  /** Launches of this recipe, and how many became healthy. */
  runs: number;
  served: number;
  /** Values to start with, where every recent launch agreed. */
  prefill: Partial<Record<FieldPath, string>>;
  /** One line of evidence per prefilled field. */
  prefillWhy: Partial<Record<FieldPath, string>>;
  suggestions: Suggestion[];
  facts: { label: string; value: string }[];
}

const DAY = 86_400_000;

function median(xs: number[]): number | null {
  if (!xs.length) return null;
  const s = [...xs].sort((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 ? s[mid] : Math.round((s[mid - 1] + s[mid]) / 2);
}

/** "~40 min", "~3 h". */
export function roughly(secs: number): string {
  if (secs < 90) return `~${Math.max(1, Math.round(secs))} s`;
  if (secs < 5400) return `~${Math.round(secs / 60)} min`;
  if (secs < 2 * 86_400) return `~${Math.round(secs / 3600)} h`;
  return `~${Math.round(secs / 86_400)} days`;
}

function firstNode(nodes: string | null): string | null {
  const n = (nodes ?? "").split(/[,\s]+/).filter(Boolean)[0];
  return n || null;
}

/** A value every one of the last few launches used, if they agree. */
function agreed(launches: LaunchRecord[], pick: (l: LaunchRecord) => string | null): { value: string; count: number } | null {
  const recent = launches.slice(0, 6).map(pick);
  if (recent.length < 2 || recent.some((v) => !v)) return null;
  return recent.every((v) => v === recent[0]) ? { value: recent[0] as string, count: recent.length } : null;
}

/**
 * What the history says about launching `form.recipe` on `form.slurm.partition`.
 * `all` is every recent launch (newest first) for cluster-wide facts such as
 * queue waits; the recipe's own launches are picked out of it.
 */
export function learnFrom(all: LaunchRecord[], form: DeployForm, opts: { now?: number; partitionMaxMinutes?: number | null } = {}): Learned {
  const now = opts.now ?? Date.now();
  const mine = all.filter((l) => l.recipe_id === form.recipe);
  const here = mine.filter((l) => !form.slurm.partition || l.partition === form.slurm.partition);
  const out: Learned = { runs: mine.length, served: mine.filter((l) => l.healthy_at).length, prefill: {}, prefillWhy: {}, suggestions: [], facts: [] };

  // Account and QoS: start from what every recent launch of this recipe used.
  for (const [path, pick, label] of [["slurm.account", (l: LaunchRecord) => l.account, "account"], ["slurm.qos", (l: LaunchRecord) => l.qos, "QoS"]] as const) {
    const current = path === "slurm.account" ? form.slurm.account : form.slurm.qos;
    const a = agreed(mine, pick);
    if (a && (!current || current === a.value)) {
      if (!current) out.prefill[path] = a.value;
      out.prefillWhy[path] = `The last ${a.count} launches of this recipe used ${a.value}.`;
    } else if (a && current !== a.value) {
      out.suggestions.push({ path, value: a.value, title: `Use ${label} ${a.value}`, evidence: `The last ${a.count} launches of this recipe used ${a.value}.` });
    }
  }

  // Walltime: runs that were cut off at their limit and relaunched.
  const minutes = parseTimeLimit(form.slurm.time_limit);
  const timedOut = here.filter((l) => l.end_state === "TIMEOUT");
  const ended = here.filter((l) => l.end_state);
  if (timedOut.length) {
    const limit = Math.max(...timedOut.map((l) => parseTimeLimit(l.time_limit) ?? 0));
    if (limit && (minutes == null || minutes <= limit)) {
      const load = median(here.map((l) => l.load_secs).filter((x): x is number => x != null));
      const max = opts.partitionMaxMinutes ?? null;
      const better = max && max > limit ? max : null;
      out.suggestions.push({
        path: "slurm.time_limit",
        value: better ? formatTimeLimit(better) : null,
        title: better ? `Walltime ${walltimeLabel(better)}` : "A longer walltime or QoS",
        evidence: `${timedOut.length} of ${ended.length} runs hit the ${walltimeLabel(limit)} walltime and were relaunched${load ? `, reloading for ${roughly(load)} each time` : ""}.`,
      });
    }
  }

  // Nodes where launches keep failing while others work.
  const byNode = new Map<string, { bad: number; total: number }>();
  for (const l of all.filter((x) => !form.slurm.partition || x.partition === form.slurm.partition)) {
    const n = firstNode(l.nodes);
    if (!n || !l.end_state) continue;
    const e = byNode.get(n) ?? { bad: 0, total: 0 };
    e.total += 1;
    if (l.end_state === "NODE_FAIL" || (!l.healthy_at && ["FAILED", "gone", "BOOT_FAIL"].includes(l.end_state))) e.bad += 1;
    byNode.set(n, e);
  }
  const excluded = new Set(form.slurm.exclude.split(",").map((x) => x.trim()).filter(Boolean));
  const healthyElsewhere = [...byNode.values()].some((e) => e.total > e.bad);
  for (const [node, e] of byNode) {
    if (e.bad >= 2 && e.bad / e.total >= 0.5 && healthyElsewhere && !excluded.has(node)) {
      const others = [...byNode.entries()].filter(([n]) => n !== node).reduce((a, [, x]) => ({ bad: a.bad + x.bad, total: a.total + x.total }), { bad: 0, total: 0 });
      out.suggestions.push({ path: "slurm.exclude", value: [...excluded, node].join(","), title: `Avoid ${node}`, evidence: `${e.bad} of ${e.total} launches on ${node} failed; on other nodes, ${others.bad} of ${others.total}.` });
    }
  }

  // Out of memory at this node count.
  const oom = here.filter((l) => l.end_state === "OUT_OF_MEMORY" && (l.nodes_requested ?? 1) === form.slurm.nodes);
  if (oom.length) {
    const worked = here.filter((l) => l.healthy_at && l.end_state !== "OUT_OF_MEMORY" && (l.nodes_requested ?? 1) > form.slurm.nodes).map((l) => l.nodes_requested ?? 1);
    const more = worked.length ? Math.min(...worked) : null;
    out.suggestions.push({
      path: "slurm.nodes",
      value: more,
      title: more ? `Run on ${more} nodes` : "More memory or nodes",
      evidence: `${oom.length} of ${here.filter((l) => (l.nodes_requested ?? 1) === form.slurm.nodes).length} runs on ${form.slurm.nodes} node${form.slurm.nodes === 1 ? "" : "s"} ran out of memory${more ? `; on ${more} they served` : ""}.`,
    });
  }

  // Facts for the side panel.
  const week = all.filter((l) => l.partition === form.slurm.partition && now - Date.parse(l.submitted_at) < 7 * DAY);
  const wait = median(week.map((l) => l.queued_secs).filter((x): x is number => x != null));
  if (wait != null) out.facts.push({ label: `Queue wait on ${form.slurm.partition}`, value: `${roughly(wait)} (median, 7 days)` });
  const loads = mine.map((l) => l.load_secs).filter((x): x is number => x != null);
  const load = median(loads);
  if (load != null) out.facts.push({ label: "Start to healthy", value: roughly(Math.min(...loads)) === roughly(Math.max(...loads)) ? roughly(load) : `${roughly(Math.min(...loads))} to ${roughly(Math.max(...loads))}` });
  const last = mine[0];
  if (last) out.facts.push({ label: "Last launched", value: `${last.model_name}, ${roughly((now - Date.parse(last.submitted_at)) / 1000)} ago` });
  return out;
}
