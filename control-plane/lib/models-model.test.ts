import { describe, expect, it } from "vitest";
import {
  acceptsImages,
  awaitingFirstPass,
  buildModelRows,
  changedFields,
  changedSections,
  changeSummary,
  contextLabel,
  EMPTY_FILTERS,
  fieldSection,
  filterRows,
  modelStatus,
  priceLabel,
  runsOn,
  searchSettings,
  snapshotForm,
  sortRows,
  statusLine,
  touches,
  typeGroup,
  variantDrafts,
  variantNameProblem,
  variantsValue,
  type VariantDraft,
  dateInputValue,
  lifecycleLabel,
  lifecycleStatus,
  retireAtFromInput,
  retiredMessage,
} from "./models-model";
import type { ModelHealthSummary, ModelRoute } from "./obleth";

const model = (over: Partial<ModelRoute> = {}): ModelRoute => ({
  id: over.model_name ?? "m", model_name: "m", description: "", upstream_model: "up", api_base: "http://a",
  api_key_set: false, model_type: "chat", quantization: "unknown", aliases: [],
  input_cost_per_token: 0, output_cost_per_token: 0,
  cost_per_image: 0, cost_per_audio_second: 0, cost_per_character: 0, cost_per_video: 0, energy_slots_per_node: 0,
  route_bias: 1, auto_eligible: true, draft_model: "", verify_api_base: "", verify_upstream_model: "",
  context_window: 131072, admission_weight: 100, max_in_flight: null, capacity_mode: "static",
  capacity_tuned_at: null, supports_function_calling: false, supports_system_messages: true,
  supports_response_schema: false, supports_tool_choice: false, supports_vision: false,
  enabled: true, cache_enabled: false, cache_ttl_secs: 0, request_timeout_secs: null,
  max_retries: 0, retry_backoff_ms: 200, endpoint_selection_mode: "failover", debug_diagnostics: false,
  tags: [], boons: [], tool_servers: [], created_at: "2026-09-01T00:00:00Z", updated_at: "",
  ...over,
});

const health = (model_id: string, status: string, over: Partial<ModelHealthSummary> = {}): ModelHealthSummary => ({
  model_id, model_name: model_id, checks_enabled: true, alerts_enabled: true, check_interval_secs: 900,
  failure_threshold: 2, maintenance_until: null, maintenance_note: null, status, consecutive_failures: 0,
  alert_state: "ok", next_check_at: "", last_checked_at: "2026-09-27T00:00:00Z", last_latency_ms: 200,
  last_http_status: 200, last_message: null, updated_at: "", ...over,
});

describe("where a model stands", () => {
  it("reads a switched-off route as off whatever its checks say", () => {
    expect(modelStatus(model({ enabled: false }), health("m", "healthy"))).toBe("off");
    expect(modelStatus(model({ enabled: false }), health("m", "unhealthy"))).toBe("off");
  });

  it("puts maintenance ahead of the check result, and an unknown result as not checked", () => {
    const later = new Date(Date.now() + 3_600_000).toISOString();
    expect(modelStatus(model(), health("m", "unhealthy", { maintenance_until: later }))).toBe("maintenance");
    expect(modelStatus(model(), health("m", "unhealthy"))).toBe("down");
    expect(modelStatus(model(), health("m", "degraded"))).toBe("unchecked");
    expect(modelStatus(model(), undefined)).toBe("unchecked");
  });

  it("counts switched-off models apart in the header line", () => {
    expect(statusLine([{ status: "serving" }, { status: "serving" }, { status: "down" }, { status: "off" }])).toBe(
      "2 of 3 serving · 1 failing checks · 1 off",
    );
  });

  it("waits for a first pass until a probe that actually ran came back healthy", () => {
    // The scheduler records a switched-off model as `disabled`: those checks never probed.
    expect(awaitingFirstPass([{ status: "disabled" }, { status: "unhealthy", message: "404" }])).toEqual({ waiting: true, last: { status: "unhealthy", message: "404" } });
    expect(awaitingFirstPass([{ status: "disabled" }, { status: "healthy" }]).waiting).toBe(false);
    expect(awaitingFirstPass([])).toEqual({ waiting: true, last: null });
  });
});

describe("types, places and prices", () => {
  it("folds both audio types into one filter", () => {
    expect(typeGroup("audio_speech")).toBe("audio");
    expect(typeGroup("audio_transcription")).toBe("audio");
    expect(typeGroup("image")).toBe("image");
    expect(typeGroup("search")).toBe("search");
    expect(typeGroup("something-new")).toBe("chat");
  });

  it("says where replicas come from", () => {
    expect(runsOn(model(), true)).toBe("slurm");
    expect(runsOn(model({ capacity_mode: "discovered", capacity_source: "kubernetes" }), false)).toBe("kubernetes");
    expect(runsOn(model({ capacity_mode: "discovered", capacity_source: "endpoints" }), false)).toBe("endpoint");
  });

  it("prices each type in the unit it bills by", () => {
    expect(priceLabel(model({ input_cost_per_token: 0.0000006, output_cost_per_token: 0.0000025 }))).toBe("$0.6 · $2.5");
    expect(priceLabel(model({ model_type: "image", cost_per_image: 0.04 }))).toBe("$0.04 / image");
    expect(priceLabel(model({ model_type: "video", cost_per_video: 2 }))).toBe("$2 / video");
    expect(priceLabel(model({ model_type: "audio_transcription", cost_per_audio_second: 0.0001 }))).toBe("$0.006 / min");
    expect(priceLabel(model())).toBeNull();
    // A search costs nothing, whatever token prices a row carries.
    expect(priceLabel(model({ model_type: "search", input_cost_per_token: 0.000001 }))).toBeNull();
  });

  it("writes context windows the way people say them", () => {
    expect(contextLabel(131072)).toBe("128K");
    expect(contextLabel(200000)).toBe("200K");
    expect(contextLabel(1_048_576)).toBe("1M");
    expect(contextLabel(0)).toBe("—");
  });
});

describe("finding a model in the list", () => {
  const models = [
    model({ id: "a", model_name: "kimi-k2-7-code", upstream_model: "moonshot/Kimi-K2.7", aliases: ["kimi-code"], tags: ["coding:3"], variants: [{ name: "kimi-k2-7-code-spec", description: "", boons: ["speculation"] }] }),
    model({ id: "b", model_name: "flux-2", model_type: "image" }),
    model({ id: "c", model_name: "gemma4", enabled: false }),
    model({ id: "d", model_name: "benchmark-endpoint-1" }),
    model({ id: "e", model_name: "glm-5-3", capacity_mode: "discovered", capacity_source: "kubernetes" }),
  ];
  const rows = buildModelRows(models, [health("a", "healthy"), health("b", "unhealthy")], {});

  it("matches every word against the name, aliases, variants, upstream and tags", () => {
    const names = (q: string) => filterRows(rows, { ...EMPTY_FILTERS, query: q }).map((r) => r.name);
    expect(names("kimi-code")).toEqual(["kimi-k2-7-code"]);
    expect(names("code-spec")).toEqual(["kimi-k2-7-code"]);
    expect(names("moonshot coding")).toEqual(["kimi-k2-7-code"]);
    expect(names("nothing-like-it")).toEqual([]);
  });

  it("hides benchmark routes unless asked", () => {
    expect(filterRows(rows, EMPTY_FILTERS).map((r) => r.name)).not.toContain("benchmark-endpoint-1");
    expect(filterRows(rows, { ...EMPTY_FILTERS, benchmarks: true }).map((r) => r.name)).toContain("benchmark-endpoint-1");
  });

  it("filters by type, status and where it runs", () => {
    expect(filterRows(rows, { ...EMPTY_FILTERS, group: "image" }).map((r) => r.name)).toEqual(["flux-2"]);
    expect(filterRows(rows, { ...EMPTY_FILTERS, status: "attention" }).map((r) => r.name)).toEqual(["flux-2"]);
    expect(filterRows(rows, { ...EMPTY_FILTERS, status: "off" }).map((r) => r.name)).toEqual(["gemma4"]);
    expect(filterRows(rows, { ...EMPTY_FILTERS, runs: "kubernetes" }).map((r) => r.name)).toEqual(["glm-5-3"]);
  });

  it("puts failing models first, switched-off ones last, when sorting by busiest", () => {
    const sorted = sortRows(filterRows(rows, EMPTY_FILTERS), "busiest").map((r) => r.name);
    expect(sorted[0]).toBe("flux-2");
    expect(sorted.at(-1)).toBe("gemma4");
    expect(sortRows(filterRows(rows, EMPTY_FILTERS), "name").map((r) => r.name)).toEqual(["flux-2", "gemma4", "glm-5-3", "kimi-k2-7-code"]);
  });
});

describe("finding a setting", () => {
  it("finds a setting by the start of its name, then by what people call it", () => {
    expect(searchSettings("retr", "chat")[0]).toMatchObject({ label: "Max retries", section: "connection" });
    expect(searchSettings("concurrency", "chat").map((r) => r.label)).toContain("Max slots");
    expect(searchSettings("ttl", "chat").map((r) => r.label)).toContain("Response cache");
  });

  it("leaves out settings a type doesn't have", () => {
    expect(searchSettings("router tags", "chat").map((r) => r.label)).toContain("Router tags");
    expect(searchSettings("variants", "chat").map((r) => r.label)).toContain("Variants");
    expect(searchSettings("variants", "image").map((r) => r.label)).not.toContain("Variants");
    expect(searchSettings("router tags", "image").map((r) => r.label)).not.toContain("Router tags");
    expect(searchSettings("", "chat")).toEqual([]);
  });
});

describe("tracking changes on the settings form", () => {
  const snap = (entries: [string, string][]) => {
    const fd = new FormData();
    for (const [k, v] of entries) fd.append(k, v);
    return snapshotForm(fd);
  };

  it("sees a checkbox that was cleared, which submits nothing", () => {
    const before = snap([["auto_eligible", "on"], ["has_routing", "1"], ["route_bias", "1"]]);
    const after = snap([["has_routing", "1"], ["route_bias", "1"]]);
    expect(changedFields(before, after)).toEqual(["auto_eligible"]);
  });

  it("compares a repeated field as a whole and ignores the markers", () => {
    const before = snap([["knowledge_collection", "a"], ["knowledge_collection", "b"], ["has_tags", "1"]]);
    const after = snap([["knowledge_collection", "a"]]);
    expect(changedFields(before, after)).toEqual(["knowledge_collection"]);
  });

  it("sends each change to the call that owns it", () => {
    expect(fieldSection("max_retries")).toBe("reliability");
    expect(fieldSection("capacity_headroom")).toBe("capacity");
    expect(fieldSection("cache_ttl_secs")).toBe("cache");
    expect(fieldSection("maintenance_note")).toBe("health");
    expect(fieldSection("tag_level_coding")).toBe("model");
    expect(fieldSection("has_tags")).toBeNull();
    expect(changedSections(["route_bias", "tag_coding", "max_in_flight"])).toEqual(["model", "capacity"]);
  });

  it("summarises the changes once each, in page order", () => {
    expect(changeSummary(["route_bias", "tag_level_math", "tag_math", "description"])).toEqual(["Description", "Router tags", "Routing bias"]);
    expect(changeSummary(["route_bias", "variants", "aliases"])).toEqual(["Aliases", "Variants", "Routing bias"]);
    expect(fieldSection("variants")).toBe("model");
  });

  it("matches a setting's fields by name or by prefix", () => {
    expect(touches(["tag_math"], ["tag_"])).toBe(true);
    expect(touches(["route_bias"], ["tag_"])).toBe(false);
    expect(touches(["max_in_flight"], ["max_in_flight"])).toBe(true);
  });
});

describe("a model's variants", () => {
  const order = ["vision", "structured_output", "compression", "knowledge", "image_generation", "speculation"];
  const stored = model({ model_name: "glm-5-3", aliases: ["glm-latest"], variants: [{ name: "glm-5-3-spec", description: "Faster answers", boons: ["speculation", "vision"] }] });

  it("submits the stored rows in the editor's order, so opening the page is no change", () => {
    const drafts = variantDrafts(stored);
    expect(drafts).toEqual([{ name: "glm-5-3-spec", description: "Faster answers", boons: ["speculation", "vision"] }]);
    expect(JSON.parse(variantsValue(drafts, order))).toEqual([{ name: "glm-5-3-spec", description: "Faster answers", boons: ["vision", "speculation"] }]);
    expect(variantDrafts(model())).toEqual([]);
    expect(variantsValue([], order)).toBe("[]");
  });

  it("reads an edit put back as no change", () => {
    const before = variantsValue(variantDrafts(stored), order);
    const toggled: VariantDraft[] = [{ name: "glm-5-3-spec ", description: " Faster answers", boons: ["vision", "speculation", "compression"] }];
    expect(variantsValue(toggled, order)).not.toBe(before);
    toggled[0].boons = toggled[0].boons.filter((b) => b !== "compression");
    expect(variantsValue(toggled, order)).toBe(before);
  });

  it("keeps a boon the editor doesn't know, at the end", () => {
    expect(JSON.parse(variantsValue([{ name: "v", description: "", boons: ["future_boon", "speculation"] }], order))[0].boons).toEqual(["speculation", "future_boon"]);
  });

  it("says why a name can't be saved", () => {
    const own = { name: "glm-5-3", aliases: ["glm-latest"], otherModels: ["kimi-k2-7-code"] };
    const rows = (...names: string[]): VariantDraft[] => names.map((name) => ({ name, description: "", boons: [] }));
    expect(variantNameProblem(rows("glm-5-3-spec"), 0, own)).toBeNull();
    expect(variantNameProblem(rows(""), 0, own)).toBeNull();
    expect(variantNameProblem(rows("auto"), 0, own)).toMatch(/router/);
    expect(variantNameProblem(rows("glm-5-3"), 0, own)).toMatch(/own name/);
    expect(variantNameProblem(rows("glm-latest"), 0, own)).toMatch(/aliases/);
    expect(variantNameProblem(rows("kimi-k2-7-code"), 0, own)).toMatch(/Another model/);
    const twice = rows("glm-5-3-spec", " glm-5-3-spec");
    expect(variantNameProblem(twice, 0, own)).toBeNull();
    expect(variantNameProblem(twice, 1, own)).toMatch(/Another variant/);
  });
});

describe("acceptsImages", () => {
  const text = { supports_vision: false, boons: [] as string[] };
  const viaBoon = { supports_vision: false, boons: ["vision", "speculation"] };
  const native = { supports_vision: true, boons: [] as string[] };

  it("takes images a model reads itself, whatever the boon", () => {
    expect(acceptsImages(native, false)).toBe(true);
    expect(acceptsImages(native, true)).toBe(true);
  });

  it("takes images through the vision boon only while the boon is on", () => {
    expect(acceptsImages(viaBoon, true)).toBe(true);
    expect(acceptsImages(viaBoon, false)).toBe(false);
  });

  it("refuses them for a text-only model, and doesn't guess before the model is known", () => {
    expect(acceptsImages(text, true)).toBe(false);
    expect(acceptsImages(undefined, true)).toBeUndefined();
  });
});

describe("lifecycle", () => {
  it("reads an older gateway's model as active", () => {
    expect(lifecycleStatus({})).toBe("active");
    expect(lifecycleLabel({})).toBeNull();
  });

  it("prefers the status in force over the stored one", () => {
    const overdue = { lifecycle: { status: "deprecated" as const, retire_at: "2026-01-01T00:00:00Z" }, effective_status: "retired" as const };
    expect(lifecycleStatus(overdue)).toBe("retired");
    expect(lifecycleLabel(overdue)).toBe("Retired");
  });

  it("labels a deprecation with its date and a redirect with its stand-in", () => {
    expect(lifecycleLabel({ lifecycle: { status: "deprecated", retire_at: "2026-10-19T00:00:00Z" } })).toBe("Deprecated · retires Oct 19, 2026");
    expect(lifecycleLabel({ lifecycle: { status: "deprecated" } })).toBe("Deprecated");
    expect(lifecycleLabel({ lifecycle: { status: "retired", replacement: "gemma4-31b-it", redirect: true } })).toBe("Retired · answered by gemma4-31b-it");
  });

  it("words the refusal the way the gateway does", () => {
    expect(retiredMessage("glm-4-5v", { status: "retired", replacement: "gemma4-31b-it", retire_at: "2026-10-19T00:00:00Z", note: "Ask rc." }))
      .toBe("The model `glm-4-5v` was retired on 2026-10-19. Use `gemma4-31b-it` instead. Ask rc.");
    expect(retiredMessage("glm-4-5v", { status: "retired", changed_at: "2026-10-20T13:00:00Z" })).toBe("The model `glm-4-5v` was retired on 2026-10-20.");
    expect(retiredMessage("glm-4-5v", { status: "retired" })).toBe("The model `glm-4-5v` has been retired.");
  });

  it("maps a date input to midnight UTC and back", () => {
    expect(retireAtFromInput("2026-10-19")).toBe("2026-10-19T00:00:00Z");
    expect(retireAtFromInput("")).toBeNull();
    expect(dateInputValue("2026-10-19T00:00:00Z")).toBe("2026-10-19");
    expect(dateInputValue(null)).toBe("");
  });
});
