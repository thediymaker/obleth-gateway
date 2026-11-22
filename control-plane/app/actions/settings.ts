"use server";

/** The Settings page: every section's save, and the settings saved from other pages. */

import { revalidatePath } from "next/cache";
import { z } from "zod";
import { obleth } from "@/lib/obleth";
import type { CharoSettingsView, EnergyTestQuery, EnergyTestResult, ResyncReport, UpdateAlertSettings, UpdateAutoRouterSettings, UpdateBoonSettings, SpeculationCategoryGate, UpdateEnergySettings, UpdateKnowledgeSettings, UpdateSlurmSettings, SlurmHealthView, NamedNetwork } from "@/lib/obleth";
import { requireAdmin } from "@/lib/auth/roles";
import { keyPrefixProblem } from "@/lib/key-prefix";
import { cleanNamedNetworks, namedNetworksProblem, parseNamedNetworksField } from "@/lib/named-networks";
import { actionError, type ActionResult } from "@/lib/action-result";
import { formNumber, trimmed } from "@/lib/form-helpers";
import { firstIssue, loadModel, modelUpdateKeys, SettingsSaveResult } from "./shared";

type SettingsPageSection = "alerts" | "routing" | "boons" | "energy" | "assistant" | "keys" | "networks" | "retention";

const SETTINGS_PAGE_ORDER: SettingsPageSection[] = ["alerts", "routing", "boons", "energy", "assistant", "keys", "networks", "retention"];

const SETTINGS_PAGE_LABEL: Record<SettingsPageSection, string> = { alerts: "Alerts", routing: "Routing", boons: "Boons", energy: "Energy", assistant: "Assistant", keys: "Keys", networks: "Networks", retention: "Data" };

const BOON_BOOLS = [
  "vision_enabled", "structured_output_enabled", "tool_loop_enabled", "image_generation_enabled", "web_search_enabled", "speculation_enabled",
  "compression_enabled", "compression_code_compaction", "compression_dedup", "compression_compact_logs", "compression_allow_lossy",
] as const;

const BOON_NUMBERS = [
  "vision_max_images", "vision_timeout_ms", "vision_cache_ttl_secs", "structured_output_max_repair_attempts", "structured_output_timeout_ms",
  "tool_loop_max_turns", "tool_loop_tool_timeout_ms", "tool_loop_deadline_secs",
  "image_generation_max_images_per_request", "image_generation_timeout_ms",
  "web_search_max_results", "web_search_max_searches_per_request", "web_search_timeout_ms",
  "speculation_agree_min", "speculation_lp_min", "speculation_abort_agree", "speculation_abort_lp", "speculation_first_chunk_tokens",
  "speculation_chunk_tokens", "speculation_decide_by_tokens", "speculation_max_draft_tokens", "speculation_pace_ms", "speculation_timeout_ms",
  "speculation_verify_timeout_ms", "speculation_verify_reply_max_bytes",
  "compression_min_tokens", "compression_max_segments", "compression_max_lossy_segments", "compression_original_ttl_secs", "compression_neural_keep_ratio",
] as const;

/** Model pickers: blank means none. */
const BOON_MODELS = ["vision_fallback_model", "structured_output_fixer_model", "image_generation_model", "web_search_tool", "speculation_draft_model", "speculation_classify_model"] as const;

const BOON_TEXTS = ["vision_describe_prompt", "tool_loop_nudge", "image_generation_tool_description", "web_search_tool_description", "speculation_verify_url_template"] as const;

/** The boons section of the Settings form as the gateway's update, or why not. */
function boonSettingsFromForm(formData: FormData): { ok: true; body: UpdateBoonSettings } | { ok: false; error: string } {
  const get = (k: string) => formData.get(`boons.${k}`);
  const body: Record<string, unknown> = {};
  for (const k of BOON_BOOLS) body[k] = get(k) === "on";
  for (const k of BOON_NUMBERS) {
    const raw = trimmed(get(k));
    if (raw === "") continue;
    const n = Number(raw);
    if (!Number.isFinite(n)) return { ok: false, error: `${k.replace(/_/g, " ")} must be a number.` };
    body[k] = n;
  }
  for (const k of BOON_MODELS) body[k] = trimmed(get(k)) || null;
  for (const k of BOON_TEXTS) if (formData.has(`boons.${k}`)) body[k] = String(get(k) ?? "");
  body.image_generation_allowed_sizes = trimmed(get("image_generation_allowed_sizes")).split(/[\s,]+/).filter(Boolean);
  body.speculation_unlisted_categories_speculate = get("speculation_unlisted") === "on";
  const gates = trimmed(get("speculation_gates"));
  try {
    body.speculation_category_gates = gates ? (JSON.parse(gates) as SpeculationCategoryGate[]) : [];
  } catch {
    return { ok: false, error: "The category rules couldn't be read." };
  }
  const kwargs = trimmed(get("speculation_draft_chat_template_kwargs"));
  if (kwargs) {
    try {
      const parsed = JSON.parse(kwargs);
      if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) return { ok: false, error: "The drafter's template arguments must be a JSON object." };
      body.speculation_draft_chat_template_kwargs = parsed;
    } catch {
      return { ok: false, error: "The drafter's template arguments aren't valid JSON." };
    }
  } else {
    body.speculation_draft_chat_template_kwargs = {};
  }
  const attempts = body.structured_output_max_repair_attempts as number | undefined;
  if (attempts !== undefined && (attempts < 0 || attempts > 3)) return { ok: false, error: "Structured output: 0 to 3 retries." };
  const turns = body.tool_loop_max_turns as number | undefined;
  if (turns !== undefined && (turns < 1 || turns > 8)) return { ok: false, error: "Tool loop: 1 to 8 turns." };
  const images = body.image_generation_max_images_per_request as number | undefined;
  if (images !== undefined && (images < 1 || images > 4)) return { ok: false, error: "Image generation: 1 to 4 images a call." };
  const results = body.web_search_max_results as number | undefined;
  if (results !== undefined && (results < 1 || results > 10)) return { ok: false, error: "Web search: 1 to 10 results a search." };
  const searches = body.web_search_max_searches_per_request as number | undefined;
  if (searches !== undefined && (searches < 1 || searches > 8)) return { ok: false, error: "Web search: 1 to 8 searches a request." };
  const keep = body.compression_neural_keep_ratio as number | undefined;
  if (keep !== undefined && (keep < 0.05 || keep > 1)) return { ok: false, error: "Compression: keep between 5% and 100% of prose." };
  return { ok: true, body: body as UpdateBoonSettings };
}

export async function saveSettingsAction(formData: FormData): Promise<SettingsSaveResult<SettingsPageSection>> {
  await requireAdmin();
  const wanted = new Set(trimmed(formData.get("sections")).split(",").map((x) => x.trim()));
  const saved: SettingsPageSection[] = [];
  const g = (section: string, k: string) => formData.get(`${section}.${k}`);
  const num = (section: string, k: string) => formNumber(g(section, k));
  const fail = (section: SettingsPageSection, error: string) => ({ ok: false as const, error: `${SETTINGS_PAGE_LABEL[section]}: ${error}`, saved });

  for (const section of SETTINGS_PAGE_ORDER.filter((x) => wanted.has(x))) {
    let res: ActionResult;
    if (section === "alerts") {
      const minutes = num("alerts", "quiet_minutes");
      if (minutes !== undefined && (!Number.isFinite(minutes) || minutes < 0)) return fail(section, "The quiet period is minutes, 0 or more.");
      const body: UpdateAlertSettings = { min_interval_secs: Math.round((minutes ?? 5) * 60) };
      const webhook = trimmed(g("alerts", "slack_webhook_url"));
      if (webhook) body.slack_webhook_url = webhook;
      else if (g("alerts", "clear_slack") === "on") body.clear_slack_webhook = true;
      if (g("alerts", "email_enabled") === "on") {
        const host = trimmed(g("alerts", "smtp_host"));
        const from = trimmed(g("alerts", "from_address"));
        const recipients = trimmed(g("alerts", "recipients")).split(/[\n,]/).map((r) => r.trim()).filter(Boolean);
        if (!host || !from || recipients.length === 0) return fail(section, "Email needs a server, a from address and at least one recipient.");
        body.email = {
          smtp_host: host,
          smtp_port: num("alerts", "smtp_port") || 587,
          username: trimmed(g("alerts", "smtp_username")) || null,
          from_address: from,
          recipients,
          starttls: g("alerts", "starttls") === "on",
        };
        const password = trimmed(g("alerts", "smtp_password"));
        if (password) body.email.smtp_password = password;
        else if (g("alerts", "clear_password") === "on") body.email.clear_smtp_password = true;
      } else {
        body.email = null;
      }
      res = await setAlertSettingsAction(body);
    } else if (section === "routing") {
      res = await setAutoRouterSettingsAction({
        classifier_enabled: g("routing", "classifier_enabled") === "on",
        classifier_model: trimmed(g("routing", "classifier_model")),
        classifier_timeout_ms: num("routing", "classifier_timeout_ms"),
        capacity_weight: num("routing", "capacity_weight"),
        cost_weight: num("routing", "cost_weight"),
        tag_weight: num("routing", "tag_weight"),
        temperature: num("routing", "temperature"),
        default_soft_cap: num("routing", "soft_cap"),
        difficulty_enabled: g("routing", "difficulty") === "on",
        right_size_enabled: g("routing", "right_size") === "on",
        tier_source: (trimmed(g("routing", "tier_source")) || "hybrid") as "hybrid" | "derived" | "declared",
        sticky_enabled: g("routing", "sticky") === "on",
        messages_default_model: trimmed(g("routing", "messages_default_model")),
        verdicts_default_model: trimmed(g("routing", "verdicts_default_model")),
      });
    } else if (section === "boons") {
      const parsed = boonSettingsFromForm(formData);
      if (!parsed.ok) return fail(section, parsed.error);
      res = await setBoonSettingsAction(parsed.body);
    } else if (section === "energy") {
      const body: UpdateEnergySettings = {
        enabled: g("energy", "enabled") === "on",
        prometheus_url: trimmed(g("energy", "prometheus_url")),
        power_query: String(g("energy", "power_query") ?? "").trim(),
        model_power_query: String(g("energy", "model_power_query") ?? "").trim(),
        model_power_label: trimmed(g("energy", "model_power_label")),
        model_requests_query: String(g("energy", "model_requests_query") ?? "").trim(),
        poll_interval_secs: num("energy", "poll_interval_secs"),
        energy_cost_per_kwh: num("energy", "energy_cost_per_kwh"),
        carbon_g_per_kwh: num("energy", "carbon_g_per_kwh"),
        pue: num("energy", "pue"),
      };
      if (body.poll_interval_secs !== undefined && body.poll_interval_secs < 10) return fail(section, "Poll at most every 10 seconds.");
      if (body.pue !== undefined && body.pue < 1) return fail(section, "PUE is 1 or more.");
      if (body.enabled && !body.prometheus_url) return fail(section, "Energy needs a Prometheus URL.");
      if (body.enabled && !body.power_query && !body.model_power_query) return fail(section, "Energy needs a power query, per model or per node.");
      res = await setEnergySettingsAction(body);
    } else if (section === "assistant") {
      const limits = ["bench_max_concurrency", "bench_max_duration_s", "bench_max_requests"].map((k) => num("assistant", k));
      if (limits.some((v) => v === undefined || !Number.isInteger(v) || v < 1)) return fail(section, "Benchmark limits are whole numbers of 1 or more.");
      res = await setCharoSettingsAction({
        enabled: g("assistant", "enabled") === "on",
        brain_model: trimmed(g("assistant", "brain_model")) || null,
        tools_enabled: { run_benchmark: g("assistant", "tool_run_benchmark") === "on" },
        bench_max_concurrency: limits[0]!,
        bench_max_duration_s: limits[1]!,
        bench_max_requests: limits[2]!,
      });
    } else if (section === "keys") {
      res = await setKeyPrefixAction(trimmed(g("keys", "prefix")));
    } else if (section === "networks") {
      const rows = parseNamedNetworksField(g("networks", "list"));
      if (!rows) return fail(section, "The list could not be read. Reload the page and try again.");
      res = await setNamedNetworksAction(rows);
    } else {
      res = await setUsageRetentionAction(num("retention", "days") ?? 0);
    }
    if (!res.ok) return fail(section, res.error);
    saved.push(section);
  }
  revalidatePath("/settings");
  return { ok: true };
}

async function setAlertSettingsAction(
  body: UpdateAlertSettings,
): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.setAlertSettings(body, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

const autoRouterUpdateSchema = z.object({
  classifier_enabled: z.boolean().optional(),
  classifier_model: z.string().optional(),
  classifier_timeout_ms: z.number().optional(),
  capacity_weight: z.number().min(0).max(1).optional(),
  cost_weight: z.number().min(0).max(1).optional(),
  tag_weight: z.number().min(0).max(1).optional(),
  default_soft_cap: z.number().int().min(1).optional(),
  temperature: z.number().min(0).max(2).optional(),
  difficulty_enabled: z.boolean().optional(),
  right_size_enabled: z.boolean().optional(),
  tier_source: z.enum(["hybrid", "derived", "declared"]).optional(),
  sticky_enabled: z.boolean().optional(),
  messages_default_model: z.string().optional(),
  verdicts_default_model: z.string().optional(),
});

export async function setAutoRouterSettingsAction(
  body: UpdateAutoRouterSettings,
): Promise<ActionResult> {
  const session = await requireAdmin();
  const parsed = autoRouterUpdateSchema.safeParse(body);
  if (!parsed.success) return { ok: false, error: firstIssue(parsed.error) };
  try {
    await obleth.setAutoRouterSettings(parsed.data, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

export async function setBoonSettingsAction(
  body: UpdateBoonSettings,
): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.setBoonSettings(body, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

/** Turn knowledge retrieval on or off for every model that has the boon and a collection. */
export async function setKnowledgeEnabledAction(enabled: boolean): Promise<ActionResult> {
  return setKnowledgeSettingsAction({ enabled });
}

/** The Knowledge page's retrieval settings form, saved as one. */
export async function saveRetrievalSettingsAction(formData: FormData): Promise<SettingsSaveResult<"retrieval">> {
  const num = (k: string) => formNumber(formData.get(k));
  const body: UpdateKnowledgeSettings = {
    enabled: formData.get("enabled") === "on",
    top_k: num("top_k"),
    min_score: num("min_score"),
    max_context_tokens: num("max_context_tokens"),
    query_turns: num("query_turns"),
    embed_timeout_ms: num("embed_timeout_ms"),
    query_cache_ttl_s: num("query_cache_ttl_s"),
    max_upload_bytes: num("max_upload_mb") === undefined ? undefined : Math.round(num("max_upload_mb")! * 1024 * 1024),
    max_chunks_per_collection: num("max_chunks_per_collection"),
    index_batch_size: num("index_batch_size"),
    index_timeout_ms: num("index_timeout_ms"),
    index_stale_after_secs: num("index_stale_after_secs"),
    debug_snapshot: formData.get("debug_snapshot") === "on",
  };
  const bad = Object.entries(body).find(([, v]) => typeof v === "number" && (!Number.isFinite(v) || v < 0));
  if (bad) return { ok: false, error: `${bad[0].replace(/_/g, " ")} must be a number of 0 or more.`, saved: [] };
  if (body.min_score !== undefined && body.min_score > 1) return { ok: false, error: "Minimum score is between 0 and 1.", saved: [] };
  const res = await setKnowledgeSettingsAction(body);
  if (!res.ok) return { ok: false, error: res.error, saved: [] };
  revalidatePath("/knowledge");
  return { ok: true };
}

/**
 * Attach a collection to a model, or detach it. Attaching also grants the
 * model the Knowledge boon, which retrieval needs; detaching the last
 * collection leaves the boon for the model page to manage.
 */
export async function attachCollectionAction(modelId: string, collectionId: string, attach: boolean): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    const model = await loadModel(modelId);
    if (!model) return { ok: false, error: "That model no longer exists." };
    const { collection_ids } = await obleth.getModelCollections(modelId);
    const next = attach ? [...new Set([...collection_ids, collectionId])] : collection_ids.filter((c) => c !== collectionId);
    await obleth.setModelCollections(modelId, next, { auditActor: session.email });
    if (attach && !(model.boons ?? []).includes("knowledge")) {
      await obleth.updateModel(modelId, { ...modelUpdateKeys(model), boons: [...(model.boons ?? []), "knowledge"] }, { auditActor: session.email });
    }
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/knowledge");
  revalidatePath("/models");
  return { ok: true };
}

async function setKnowledgeSettingsAction(
  body: UpdateKnowledgeSettings,
): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.updateKnowledgeSettings(body, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  revalidatePath("/knowledge");
  return { ok: true };
}

async function setEnergySettingsAction(
  body: UpdateEnergySettings,
): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.setEnergySettings(body, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

export async function testEnergyQueryAction(
  query: EnergyTestQuery,
): Promise<ActionResult & { data?: EnergyTestResult }> {
  await requireAdmin();
  try {
    const data = await obleth.testEnergyQuery(query);
    return { ok: true, data };
  } catch (e) {
    return actionError(e);
  }
}

async function setCharoSettingsAction(
  next: CharoSettingsView,
): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.setCharoSettings(next, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  revalidatePath("/", "layout");
  return { ok: true };
}

export async function setSlurmSettingsAction(
  body: UpdateSlurmSettings,
): Promise<ActionResult> {
  const session = await requireAdmin();
  let warnings: string[] = [];
  try {
    const saved = await obleth.setSlurmSettings(body, { auditActor: session.email });
    // Plain sentences about what was saved, such as a token that expires soon.
    // Older gateways don't send them.
    warnings = Array.isArray(saved?.warnings) ? saved.warnings.filter((w) => typeof w === "string" && w.trim()) : [];
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/deployments");
  revalidatePath("/settings");
  // The model-create dialog gates the Slurm option on these settings.
  revalidatePath("/models");
  return warnings.length ? { ok: true, warnings } : { ok: true };
}

export async function testSlurmConnectionAction(): Promise<
  (ActionResult & { health?: SlurmHealthView })
> {
  await requireAdmin();
  try {
    const health = await obleth.testSlurmConnection();
    return { ok: true, health };
  } catch (e) {
    return actionError(e);
  }
}

async function setNamedNetworksAction(rows: NamedNetwork[]): Promise<ActionResult> {
  const session = await requireAdmin();
  const problem = namedNetworksProblem(rows);
  if (problem) return { ok: false, error: problem };
  try {
    await obleth.setNamedNetworks(cleanNamedNetworks(rows), { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  revalidatePath("/reports");
  return { ok: true };
}

async function setKeyPrefixAction(prefix: string): Promise<ActionResult> {
  const session = await requireAdmin();
  const problem = keyPrefixProblem(prefix);
  if (problem) return { ok: false, error: problem };
  try {
    await obleth.setKeyPrefix(prefix, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  revalidatePath("/keys");
  return { ok: true };
}

async function setUsageRetentionAction(
  days: number,
): Promise<ActionResult> {
  const session = await requireAdmin();
  if (!Number.isFinite(days) || days < 1) {
    return { ok: false, error: "Retention must be at least 1 day" };
  }
  try {
    await obleth.setUsageRetention(Math.floor(days), { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

export async function compactUsageAction(): Promise<
  ActionResult & { partitionsDropped?: number; retentionDays?: number }
> {
  const session = await requireAdmin();
  try {
    const res = await obleth.compactUsage({ auditActor: session.email });
    return {
      ok: true,
      partitionsDropped: res.partitions_dropped,
      retentionDays: res.retention_days,
    };
  } catch (e) {
    return actionError(e);
  }
}

/**
 * Rebuild the data plane's resolver cache (keys, models, MCP servers) from
 * Postgres. This is the retry a failed delete-eviction error points operators at.
 */
export async function resyncCacheAction(): Promise<ActionResult & { report?: ResyncReport }> {
  const session = await requireAdmin();
  try {
    const report = await obleth.resync({ auditActor: session.email });
    return { ok: true, report };
  } catch (e) {
    return actionError(e);
  }
}

export async function testAlertAction(): Promise<
  ActionResult & {
    results?: { channel: string; ok: boolean; detail: string }[];
  }
> {
  await requireAdmin();
  try {
    const res = await obleth.testAlert();
    return { ok: true, results: res.results };
  } catch (e) {
    return actionError(e);
  }
}
