import type {
  AlertSettingsView,
  AutoRouterSettingsView,
  BoonSettingsView,
  CompressorStatusView,
  KnowledgeSettingsView,
  ModelRoute,
  RouterReadinessView,
  SlurmSettingsView,
  SpeculationCategoryGate,
} from "@/lib/obleth";
import { boonsWaitingOnFunctionCalling } from "@/lib/boon-availability";

/**
 * The Settings page's pure logic: its sections, the presets they offer, and
 * the "Needs you" list worked out from what's actually set.
 */

/** Sections that save, in the order the save runs them. */
export const SAVE_SECTIONS = ["alerts", "routing", "boons", "energy", "assistant", "retention"] as const;
export type SaveSection = (typeof SAVE_SECTIONS)[number];

export const SECTION_LABEL: Record<SaveSection, string> = {
  alerts: "Alerts",
  routing: "Routing",
  boons: "Boons",
  energy: "Energy",
  assistant: "Assistant",
  retention: "Data",
};

/** Every form field is `section.field`, so a change says which save it belongs to. */
export function sectionOfField(name: string): SaveSection | null {
  const s = name.split(".")[0];
  return (SAVE_SECTIONS as readonly string[]).includes(s) ? (s as SaveSection) : null;
}

/** Old `?tab=` links land on the matching section. */
export const TAB_ANCHOR: Record<string, string> = {
  alerts: "alerts",
  routing: "routing",
  boons: "boons",
  compression: "boon-compression",
  energy: "energy",
  data: "data",
  assistant: "assistant",
  about: "about",
};

// ---------------------------------------------------------------------------
// Routing presets
// ---------------------------------------------------------------------------

export interface RouterValues {
  capacity_weight: number;
  cost_weight: number;
  tag_weight: number;
  temperature: number;
  soft_cap: number;
  difficulty: boolean;
  tier_source: "hybrid" | "derived" | "declared";
}

export const ROUTER_PROFILES: { key: string; label: string; blurb: string; values: RouterValues }[] = [
  { key: "balanced", label: "Balanced", blurb: "Free capacity first, then price; task fit breaks ties.", values: { capacity_weight: 0.6, cost_weight: 0.4, tag_weight: 0.5, temperature: 0, soft_cap: 8, difficulty: false, tier_source: "hybrid" } },
  { key: "best_answer", label: "Best answer", blurb: "The model that fits the task best; harder questions go to stronger models.", values: { capacity_weight: 0.4, cost_weight: 0.05, tag_weight: 0.9, temperature: 0, soft_cap: 8, difficulty: true, tier_source: "hybrid" } },
  { key: "cost_saver", label: "Cost saver", blurb: "The cheapest model that can take it; capacity steers around busy ones.", values: { capacity_weight: 0.5, cost_weight: 0.9, tag_weight: 0.4, temperature: 0, soft_cap: 8, difficulty: false, tier_source: "hybrid" } },
];

export function routerProfileOf(v: RouterValues): string {
  return ROUTER_PROFILES.find((p) => (Object.keys(p.values) as (keyof RouterValues)[]).every((k) => p.values[k] === v[k]))?.key ?? "custom";
}

export function routerValues(s: AutoRouterSettingsView): RouterValues {
  return { capacity_weight: s.capacity_weight, cost_weight: s.cost_weight, tag_weight: s.tag_weight, temperature: s.temperature, soft_cap: s.default_soft_cap, difficulty: s.difficulty_enabled, tier_source: s.tier_source };
}

// ---------------------------------------------------------------------------
// Speculation presets
// ---------------------------------------------------------------------------

export interface SpecValues {
  agree_min: number;
  lp_min: number;
  abort_agree: number;
  abort_lp: number;
  first_chunk_tokens: number;
  chunk_tokens: number;
  decide_by_tokens: number;
  max_draft_tokens: number;
  pace_ms: number;
  timeout_ms: number;
  unlisted: boolean;
  gates: SpeculationCategoryGate[];
}

const SPEC_BASE = { agree_min: 0.5, lp_min: -1.0, abort_agree: 0.45, abort_lp: -1.6, first_chunk_tokens: 80, chunk_tokens: 250, decide_by_tokens: 450, max_draft_tokens: 2048, pace_ms: 9, timeout_ms: 45000 };

export const SPEC_PROFILES: { key: string; label: string; blurb: string; values: SpecValues }[] = [
  {
    key: "calibrated",
    label: "Calibrated",
    blurb: "Drafts only where they verify well (coding, math, summaries, writing), each with its own bar. Needs a classifier.",
    values: {
      ...SPEC_BASE,
      unlisted: false,
      gates: [
        { tag: "infrastructure", speculate: false },
        { tag: "planning", speculate: false },
        { tag: "database", speculate: false },
        { tag: "regex", speculate: false },
        { tag: "explanation", speculate: false },
        { tag: "debugging", speculate: false },
        { tag: "coding", speculate: true, agree_min: 0.4, lp_min: -0.8 },
        { tag: "math", speculate: true, agree_min: 0.4, lp_min: -0.9 },
        { tag: "summarization", speculate: true, agree_min: 0.4, lp_min: -1.0 },
        { tag: "writing", speculate: true, agree_min: 0.4, lp_min: -1.9 },
      ],
    },
  },
  { key: "cautious", label: "Cautious", blurb: "Every request drafted and held to one strict bar. Works without a classifier.", values: { ...SPEC_BASE, unlisted: true, gates: [] } },
];

function sameGates(a: SpeculationCategoryGate[], b: SpeculationCategoryGate[]): boolean {
  const key = (g: SpeculationCategoryGate) => `${g.tag}|${!!g.speculate}|${g.agree_min ?? ""}|${g.lp_min ?? ""}`;
  const as = a.map(key).sort();
  const bs = b.map(key).sort();
  return as.length === bs.length && as.every((x, i) => x === bs[i]);
}

export function specProfileOf(v: SpecValues): string {
  return SPEC_PROFILES.find((p) => (Object.keys(SPEC_BASE) as (keyof typeof SPEC_BASE)[]).every((k) => p.values[k] === v[k]) && p.values.unlisted === v.unlisted && sameGates(p.values.gates, v.gates))?.key ?? "custom";
}

export function specValues(b: BoonSettingsView): SpecValues {
  return {
    agree_min: b.speculation_agree_min,
    lp_min: b.speculation_lp_min,
    abort_agree: b.speculation_abort_agree,
    abort_lp: b.speculation_abort_lp,
    first_chunk_tokens: b.speculation_first_chunk_tokens,
    chunk_tokens: b.speculation_chunk_tokens,
    decide_by_tokens: b.speculation_decide_by_tokens,
    max_draft_tokens: b.speculation_max_draft_tokens,
    pace_ms: b.speculation_pace_ms,
    timeout_ms: b.speculation_timeout_ms,
    unlisted: b.speculation_unlisted_categories_speculate,
    gates: b.speculation_category_gates ?? [],
  };
}

// ---------------------------------------------------------------------------
// Boons
// ---------------------------------------------------------------------------

export type BoonKey = "vision" | "structured_output" | "tool_loop" | "image_generation" | "web_search" | "speculation" | "compression";

export const BOONS: { key: BoonKey; label: string; blurb: string; enabled: keyof BoonSettingsView; modelBoon: string | null }[] = [
  { key: "vision", label: "Vision", blurb: "Describes images for text-only models", enabled: "vision_enabled", modelBoon: "vision" },
  { key: "structured_output", label: "Structured output", blurb: "Holds replies to a JSON schema", enabled: "structured_output_enabled", modelBoon: "structured_output" },
  { key: "tool_loop", label: "Tool loop", blurb: "Runs MCP tools inside the gateway", enabled: "tool_loop_enabled", modelBoon: null },
  { key: "image_generation", label: "Image generation", blurb: "Lets a chat model draw", enabled: "image_generation_enabled", modelBoon: "image_generation" },
  { key: "web_search", label: "Web search", blurb: "Lets a chat model look things up", enabled: "web_search_enabled", modelBoon: "web_search" },
  { key: "speculation", label: "Speculation", blurb: "Answers from a fast drafter when the model agrees", enabled: "speculation_enabled", modelBoon: "speculation" },
  { key: "compression", label: "Compression", blurb: "Shrinks long inputs before the model reads them", enabled: "compression_enabled", modelBoon: "compression" },
];

/** Models that ask for a boon: those that list it, or for the tool loop, those granted an MCP server. */
export function modelsAsking(key: BoonKey, models: ModelRoute[]): string[] {
  const boon = BOONS.find((b) => b.key === key)!;
  return models
    .filter((m) => (boon.modelBoon ? (m.boons ?? []).includes(boon.modelBoon) : (m.tool_servers ?? []).length > 0))
    .map((m) => m.model_name)
    .sort();
}

/** "No describer model picked", "flux-2 · up to 2 a call · 512 or 1024 square". */
export function boonSummary(key: BoonKey, b: BoonSettingsView): string {
  const s = (ms: number) => (ms >= 60000 && ms % 60000 === 0 ? `${ms / 60000} min` : `${Math.round(ms / 1000)} s`);
  switch (key) {
    case "vision":
      return b.vision_fallback_model ? `${b.vision_fallback_model} describes up to ${b.vision_max_images} images` : "No describer model picked";
    case "structured_output":
      return `${b.structured_output_fixer_model ? `Fixed by ${b.structured_output_fixer_model}` : "Fixed by the same model"} · ${b.structured_output_max_repair_attempts} ${b.structured_output_max_repair_attempts === 1 ? "retry" : "retries"}`;
    case "tool_loop":
      return `${b.tool_loop_max_turns} turns · ${s(b.tool_loop_tool_timeout_ms)} a tool · ${s(b.tool_loop_deadline_secs * 1000)} in all`;
    case "image_generation":
      return b.image_generation_model ? `${b.image_generation_model} · up to ${b.image_generation_max_images_per_request} a call · ${(b.image_generation_allowed_sizes ?? []).join(", ")}` : "No image model picked";
    case "web_search":
      return b.web_search_tool ? `${b.web_search_tool} · ${b.web_search_max_results} results a search · up to ${b.web_search_max_searches_per_request} searches a request` : "No search tool picked";
    case "speculation":
      return b.speculation_draft_model ? `drafter ${b.speculation_draft_model} · ${(b.speculation_category_gates ?? []).length} category rules` : "No drafter picked";
    case "compression": {
      const parts = [b.compression_code_compaction && "code", b.compression_dedup && "dedup", b.compression_compact_logs && "logs", b.compression_allow_lossy && "lossy"].filter(Boolean);
      return parts.length ? `Lossless JSON plus ${parts.join(", ")}` : "Lossless JSON only";
    }
  }
}

// ---------------------------------------------------------------------------
// Needs you
// ---------------------------------------------------------------------------

export interface Finding {
  key: string;
  title: string;
  detail: string;
  /** A section anchor on this page, or a page elsewhere. */
  href: string;
  action: string;
}

export function needsYou(input: {
  alerts: AlertSettingsView | null;
  boons: BoonSettingsView | null;
  knowledge: KnowledgeSettingsView | null;
  models: ModelRoute[];
  readiness: RouterReadinessView | null;
  slurm: SlurmSettingsView | null;
  compressor: CompressorStatusView | null;
  router: AutoRouterSettingsView | null;
}): Finding[] {
  const out: Finding[] = [];
  const { alerts, boons, models } = input;
  if (alerts && !alerts.slack_webhook_set && !alerts.email) {
    out.push({ key: "alerts", title: "Nobody is alerted", detail: "No Slack webhook or email is set up, so a model going down or a stalled pool reaches no one.", href: "#alerts", action: "Set up alerts" });
  }
  if (boons) {
    for (const b of BOONS) {
      const on = !!boons[b.enabled];
      const asking = modelsAsking(b.key, models);
      if (!on && asking.length) {
        const who = asking.length === 1 ? `${asking[0]} asks` : `${asking.length} models ask`;
        const detail = b.key === "tool_loop" ? "They're granted MCP servers, but the tools don't run while the tool loop is off." : b.key === "vision" ? "Images sent to them aren't described, so they can't answer about them." : "It does nothing for them while it's off.";
        out.push({ key: `boon-${b.key}`, title: b.key === "tool_loop" ? `${asking.length === 1 ? `${asking[0]} has` : `${asking.length} models have`} MCP tools, but the tool loop is off` : `${who} for ${b.label}, which is off`, detail, href: `#boon-${b.key}`, action: `Turn on ${b.label}` });
      }
    }
    if (boons.vision_enabled && !boons.vision_fallback_model) out.push({ key: "vision-model", title: "Vision is on with no describer model", detail: "Pick the model that describes images.", href: "#boon-vision", action: "Pick one" });
    if (boons.image_generation_enabled && !boons.image_generation_model) out.push({ key: "image-model", title: "Image generation is on with no image model", detail: "Pick the model that draws.", href: "#boon-image_generation", action: "Pick one" });
    if (boons.web_search_enabled && !boons.web_search_tool) out.push({ key: "search-tool", title: "Web search is on with no search tool", detail: "Pick the search tool the models search with.", href: "#boon-web_search", action: "Pick one" });
    if (boons.speculation_enabled && !boons.speculation_draft_model) out.push({ key: "spec-model", title: "Speculation is on with no drafter", detail: "Pick the fast model that drafts.", href: "#boon-speculation", action: "Pick one" });
  }
  const waiting = models.filter((m) => m.model_type === "chat" && boonsWaitingOnFunctionCalling(m).length > 0);
  if (waiting.length) {
    const one = waiting.length === 1 ? waiting[0] : null;
    const labels = one ? boonsWaitingOnFunctionCalling(one).map((b) => BOONS.find((x) => x.modelBoon === b)?.label ?? b) : [];
    out.push({
      key: "boons-need-function-calling",
      title: one ? `${one.model_name} has ${labels.join(" and ")}, but Function calling is off` : `${waiting.length} models have tool boons, but Function calling is off`,
      detail: `${one ? "" : `${waiting.map((m) => m.model_name).join(", ")}. `}These boons give the model a tool to call, so they do nothing until Function calling is on.`,
      href: one ? `/models/${encodeURIComponent(one.model_name)}#set-native` : "/models",
      action: one ? "Open the model" : "Models",
    });
  }
  if (input.knowledge && !input.knowledge.enabled) {
    const asking = models.filter((m) => (m.boons ?? []).includes("knowledge")).map((m) => m.model_name);
    if (asking.length) out.push({ key: "knowledge", title: `${asking.length === 1 ? `${asking[0]} asks` : `${asking.length} models ask`} for Knowledge, but retrieval is off`, detail: "Their collections aren't searched.", href: "/knowledge?tab=retrieval", action: "Retrieval settings" });
  }
  for (const f of input.readiness?.findings ?? []) {
    if (f.severity === "warn") out.push({ key: `router-${f.code}`, title: f.title, detail: f.detail, href: "#routing", action: "Routing" });
  }
  if (input.slurm?.enabled && !input.slurm.provisioner_running) {
    out.push({ key: "provisioner", title: "Slurm is on, but the provisioner isn't running", detail: "Nothing is launched or replaced until it checks in.", href: "/deployments?slurm=1", action: "Slurm connection" });
  }
  if (input.compressor?.configured && !input.compressor.reachable) {
    out.push({ key: "compressor", title: "The compressor service doesn't answer", detail: input.compressor.error ?? "Lossy compression scores sentences with the built-in heuristic instead.", href: "#about", action: "About" });
  }
  return out;
}

function plural(n: number, noun: string): string {
  return `${n} ${noun}${n === 1 ? "" : "s"}`;
}

/** What a cache reconcile did, in a sentence. */
export function describeResync(report: { keys: number; models: number; mcp_servers: number; keys_pruned: number; model_names_pruned: number; mcp_servers_pruned: number }): string {
  const republished = `Republished ${plural(report.keys, "key")}, ${plural(report.models, "model")}, and ${plural(report.mcp_servers, "MCP server")}.`;
  const evicted = report.keys_pruned + report.model_names_pruned + report.mcp_servers_pruned;
  if (evicted === 0) return `${republished} No stale entries found.`;
  return `${republished} Evicted ${plural(report.keys_pruned, "stale key")}, ${plural(report.model_names_pruned, "stale model name")}, and ${plural(report.mcp_servers_pruned, "stale MCP server")}.`;
}
