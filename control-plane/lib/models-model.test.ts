import { describe, expect, it } from "vitest";
import {
  acceptsImages,
  awaitingFirstPass,
  buildModelRows,
  contextLabel,
  EMPTY_FILTERS,
  fieldLabel,
  fieldSection,
  filterRows,
  modelStatus,
  priceLabel,
  runsOn,
  searchSettings,
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
  familyGroups,
  familyMemberOptions,
  familyNameProblem,
  familyPreview,
  familyProblem,
  familyWrite,
  levelAction,
  levelDraft,
  reasoningDirty,
  reasoningDraft,
  reasoningFromDraft,
  reasoningNavCount,
  reasoningPreview,
  reasoningProblem,
  sendWords,
  takenModelNames,
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
    expect(priceLabel(model({ input_cost_per_token: 0.0000006, output_cost_per_token: 0.0000025 }))).toBe("$0.60 · $2.50");
    expect(priceLabel(model({ model_type: "image", cost_per_image: 0.04 }))).toBe("$0.04 / image");
    expect(priceLabel(model({ model_type: "video", cost_per_video: 2 }))).toBe("$2.00 / video");
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

  it("finds the reasoning section on chat models only", () => {
    expect(searchSettings("thinking", "chat")[0]).toMatchObject({ id: "reasoning", label: "Reasoning effort", section: "reasoning" });
    expect(searchSettings("reasoning_effort", "embedding")).toEqual([]);
  });
});

describe("tracking changes on the settings form", () => {
  it("sends each change to the call that owns it", () => {
    expect(fieldSection("max_retries")).toBe("reliability");
    expect(fieldSection("capacity_headroom")).toBe("capacity");
    expect(fieldSection("cache_ttl_secs")).toBe("cache");
    expect(fieldSection("listed")).toBe("listing");
    expect(fieldSection("maintenance_note")).toBe("health");
    expect(fieldSection("tag_level_coding")).toBe("model");
    expect(fieldSection("has_tags")).toBeNull();
  });

  it("names a change once per setting", () => {
    expect(["route_bias", "tag_level_math", "tag_math", "description"].map(fieldLabel)).toEqual(["Routing bias", "Router tags", "Router tags", "Description"]);
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

  it("labels a staged model as not listed yet", () => {
    expect(lifecycleStatus({ lifecycle: { status: "staged" } })).toBe("staged");
    expect(lifecycleLabel({ lifecycle: { status: "staged" } })).toBe("Staged · not listed yet");
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

describe("reasoning effort", () => {
  const glmLike = {
    mode: "mapped" as const,
    levels: {
      none: { use: "low" as const },
      low: { send: { reasoning_effort: "low" } },
      medium: { send: {} },
      high: { send: { reasoning_effort: "max" } },
    },
  };

  it("reads a missing map as pass-through", () => {
    expect(reasoningDraft(model()).mode).toBe("passthrough");
    expect(reasoningNavCount(model())).toBeUndefined();
    expect(reasoningNavCount(model({ reasoning: glmLike }))).toBe("translated");
    expect(reasoningNavCount(model({ reasoning: { mode: "unsupported" } }))).toBe("removed");
  });

  it("shows a stored action as the preset it matches exactly, else as custom JSON", () => {
    expect(levelDraft("medium", { send: {} }).preset).toBe("default");
    expect(levelDraft("high", { send: { reasoning_effort: "max" } })).toMatchObject({ preset: "effort", effort: "max" });
    expect(levelDraft("none", { send: { chat_template_kwargs: { enable_thinking: false } } }).preset).toBe("thinking_off");
    expect(levelDraft("high", { send: { chat_template_kwargs: { enable_thinking: true } } }).preset).toBe("thinking_on");
    expect(levelDraft("none", { use: "low" })).toMatchObject({ preset: "use", use: "low" });
    const mixed = levelDraft("high", { send: { reasoning_effort: "high", chat_template_kwargs: { enable_thinking: true } } });
    expect(mixed.preset).toBe("custom");
    expect(JSON.parse(mixed.json)).toEqual({ reasoning_effort: "high", chat_template_kwargs: { enable_thinking: true } });
    expect(levelDraft("low", { send: { chat_template_kwargs: { enable_thinking: true, other: 1 } } }).preset).toBe("custom");
  });

  it("turns each preset back into the stored action", () => {
    for (const [level, action] of Object.entries(glmLike.levels)) {
      expect(levelAction(levelDraft(level as "none", action))).toEqual({ ok: true, action });
    }
    expect(levelAction({ ...levelDraft("low"), preset: "thinking_off" })).toEqual({ ok: true, action: { send: { chat_template_kwargs: { enable_thinking: false } } } });
    expect(levelAction({ ...levelDraft("low"), effort: "  xhigh " })).toEqual({ ok: true, action: { send: { reasoning_effort: "xhigh" } } });
  });

  it("says plainly when a row can't be saved", () => {
    expect(levelAction({ ...levelDraft("low"), effort: " " })).toMatchObject({ ok: false, error: expect.stringContaining("Type the value") });
    expect(levelAction({ ...levelDraft("low"), preset: "custom", json: "{nope" })).toMatchObject({ ok: false, error: expect.stringContaining("not valid JSON") });
    expect(levelAction({ ...levelDraft("low"), preset: "custom", json: "[1]" })).toMatchObject({ ok: false, error: expect.stringContaining("must be a JSON object") });
  });

  it("refuses what the gateway refuses", () => {
    expect(reasoningProblem(glmLike, "chat")).toBeNull();
    expect(reasoningProblem({ mode: "mapped", levels: { low: { send: {} } } })).toBe("Set all four levels to translate them. Not set: none, medium, high.");
    expect(reasoningProblem({ ...glmLike, levels: { ...glmLike.levels, low: { use: "medium" } } })).toContain("which is itself the same as another level");
    expect(reasoningProblem({ ...glmLike, levels: { ...glmLike.levels, high: { use: "high" } } })).toContain("can't be the same as itself");
    expect(reasoningProblem({ ...glmLike, levels: { ...glmLike.levels, high: { send: { stream: true } } } })).toBe("High may not set stream. A level can only send fields about effort.");
    expect(reasoningProblem({ ...glmLike, levels: { ...glmLike.levels, high: { send: { x: "y".repeat(5000) } } } })).toContain("at most 4096");
    expect(reasoningProblem({ mode: "unsupported" }, "embedding")).toContain("Only chat models");
    // Outside mapped, a use may point anywhere but at itself: the levels wait there for later.
    expect(reasoningProblem({ mode: "passthrough", levels: { none: { use: "low" } } })).toBeNull();
  });

  it("keeps the stored levels when switching away from translating", () => {
    const m = model({ reasoning: glmLike });
    const draft = { ...reasoningDraft(m), mode: "passthrough" as const };
    expect(reasoningFromDraft(draft, m)).toEqual({ ok: true, reasoning: { mode: "passthrough", levels: glmLike.levels } });
    expect(reasoningDirty(draft, m)).toBe(true);
    expect(reasoningDirty(reasoningDraft(m), m)).toBe(false);
  });

  it("never invents levels for a model that has none", () => {
    const m = model();
    expect(reasoningFromDraft({ ...reasoningDraft(m), mode: "unsupported" }, m)).toEqual({ ok: true, reasoning: { mode: "unsupported" } });
    expect(reasoningDirty(reasoningDraft(m), m)).toBe(false);
    // Translating starts every level at reasoning_effort = its own name.
    const mapped = reasoningFromDraft({ ...reasoningDraft(m), mode: "mapped", defaultLevel: "medium" }, m);
    expect(mapped).toEqual({
      ok: true,
      reasoning: {
        mode: "mapped",
        default_level: "medium",
        levels: { none: { send: { reasoning_effort: "none" } }, low: { send: { reasoning_effort: "low" } }, medium: { send: { reasoning_effort: "medium" } }, high: { send: { reasoning_effort: "high" } } },
      },
    });
  });

  it("names the level whose row is unfinished", () => {
    const m = model({ reasoning: glmLike });
    const draft = reasoningDraft(m);
    draft.levels.high = { ...draft.levels.high, preset: "custom", json: "{" };
    expect(reasoningFromDraft(draft, m)).toMatchObject({ ok: false, error: expect.stringMatching(/^High: The custom fields are not valid JSON/) });
    expect(reasoningDirty(draft, m)).toBe(true);
    expect(reasoningFromDraft({ ...reasoningDraft(model({ model_type: "embedding" })), mode: "unsupported" }, model({ model_type: "embedding" }))).toMatchObject({ ok: false });
  });

  it("previews each level in plain words", () => {
    expect(sendWords({})).toBe("sends nothing, so the server's own default applies");
    expect(sendWords({ chat_template_kwargs: { enable_thinking: false }, reasoning_effort: "low" })).toBe("sends chat_template_kwargs.enable_thinking: false, reasoning_effort: low");
    expect(reasoningPreview(glmLike)).toEqual([
      "none → served as low, which sends reasoning_effort: low (this model has no off switch)",
      "low → sends reasoning_effort: low",
      "medium → sends nothing, so the server's own default applies",
      "high → sends reasoning_effort: max",
      "no effort set → sent as it came, so the server's own default applies",
    ]);
    expect(reasoningPreview({ ...glmLike, default_level: "high" }).at(-1)).toBe("no effort set → served as high");
    expect(reasoningPreview({ mode: "passthrough" })).toHaveLength(1);
    expect(reasoningPreview({ mode: "unsupported" })[0]).toContain("removes reasoning_effort and reasoning");
  });
});

describe("model families", () => {
  const members = { none: "acme-7b-instruct", low: "acme-7b-think", medium: "acme-7b-think", high: "acme-7b-think" };
  const taken = { modelNames: takenModelNames([model({ model_name: "acme-7b-think", aliases: ["acme-think"], variants: [{ name: "acme-think-spec", description: "", boons: [] }] })]), familyNames: ["other"] };

  it("checks a name the way the gateway does", () => {
    expect(taken.modelNames).toEqual(["acme-7b-think", "acme-think", "acme-think-spec"]);
    expect(familyNameProblem("acme-7b", taken)).toBeNull();
    expect(familyNameProblem(" ", taken)).toContain("Give the family a name");
    expect(familyNameProblem("acme 7b", taken)).toBe("A family name can't contain spaces.");
    expect(familyNameProblem("auto", taken)).toBe("auto is the router's own name.");
    expect(familyNameProblem("acme-think", taken)).toBe("A model already answers to that name.");
    expect(familyNameProblem("other", taken)).toBe("Another family already has that name.");
    expect(familyNameProblem("x".repeat(129), taken)).toContain("128 characters");
  });

  it("asks for a model at every level", () => {
    const draft = { name: "acme-7b", description: "", default_level: "none" as const, members: { ...members, medium: "" } };
    expect(familyProblem(draft, taken)).toBe("Pick the model that answers at medium.");
    expect(familyProblem({ ...draft, members }, taken)).toBeNull();
    expect(familyWrite({ ...draft, name: " acme-7b ", description: " d ", members })).toEqual({ name: "acme-7b", description: "d", default_level: "none", members });
  });

  it("previews where requests go, grouping the levels each model answers", () => {
    expect(familyGroups(members)).toEqual([{ levels: ["none"], model: "acme-7b-instruct" }, { levels: ["low", "medium", "high"], model: "acme-7b-think" }]);
    expect(familyPreview({ name: "acme-7b", default_level: "none", members })).toEqual({
      lead: "A request for acme-7b with no effort set goes to acme-7b-instruct.",
      levels: "none → acme-7b-instruct · low, medium, high → acme-7b-think",
    });
    expect(familyPreview({ name: "acme-7b", default_level: "none", members: { ...members, high: "" } })).toBeNull();
  });

  it("offers chat models that are still served, and keeps a chosen one", () => {
    const models = [
      model({ model_name: "b-chat" }),
      model({ model_name: "a-old", lifecycle: { status: "deprecated" } }),
      model({ model_name: "gone", lifecycle: { status: "retired" } }),
      model({ model_name: "embed", model_type: "embedding" }),
    ];
    expect(familyMemberOptions(models)).toEqual([{ value: "a-old", label: "a-old (deprecated)" }, { value: "b-chat", label: "b-chat" }]);
    expect(familyMemberOptions(models, ["gone"]).map((o) => o.value)).toEqual(["a-old", "b-chat", "gone"]);
  });
});
