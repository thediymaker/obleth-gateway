import { describe, expect, it } from "vitest";
import { alerts, boons, model, readiness, router } from "@/components/settings/fixtures";
import { ROUTER_PROFILES, SPEC_PROFILES, boonSummary, describeResync, modelsAsking, needsYou, routerProfileOf, routerValues, sectionOfField, specProfileOf, specValues } from "./settings-model";

const base = { alerts: alerts(), boons: boons(), knowledge: null, models: [], readiness: readiness(), slurm: null, compressor: null, router: router() };

describe("the settings model", () => {
  it("files each field under the save call that owns it", () => {
    expect(sectionOfField("alerts.quiet_minutes")).toBe("alerts");
    expect(sectionOfField("retention.days")).toBe("retention");
    expect(sectionOfField("slurm.url")).toBeNull();
    expect(sectionOfField("id")).toBeNull();
  });

  it("names the routing profile the weights match, or custom", () => {
    expect(routerProfileOf(routerValues(router({ difficulty_enabled: false })))).toBe("balanced");
    expect(routerProfileOf(routerValues(router()))).toBe("custom");
    for (const p of ROUTER_PROFILES) expect(routerProfileOf(p.values)).toBe(p.key);
  });

  it("round-trips the speculation profiles", () => {
    for (const p of SPEC_PROFILES) expect(specProfileOf(p.values)).toBe(p.key);
    expect(specProfileOf(specValues(boons()))).toBe("custom");
  });

  it("counts a model as asking for the tool loop when it has MCP servers", () => {
    const models = [model("a", { tool_servers: ["search"] } as never), model("b", { boons: ["vision"] })];
    expect(modelsAsking("tool_loop", models)).toEqual(["a"]);
    expect(modelsAsking("vision", models)).toEqual(["b"]);
  });

  it("says what needs you, and nothing when all is well", () => {
    const models = [model("glm-5-3", { boons: ["vision", "image_generation", "speculation", "knowledge"], supports_function_calling: true })];
    const found = needsYou({ ...base, models, knowledge: { enabled: false } as never, slurm: { enabled: true, provisioner_running: false } as never, compressor: { configured: true, reachable: false, url: "", model: null, revision: null, error: null } });
    expect(found.map((f) => f.key)).toEqual(["alerts", "boon-vision", "knowledge", "provisioner", "compressor"]);
    expect(found[1]).toMatchObject({ title: "glm-5-3 asks for Vision, which is off", href: "#boon-vision" });
    expect(found.find((f) => f.key === "provisioner")!.href).toBe("/deployments?slurm=1");
    expect(needsYou({ ...base, alerts: alerts({ slack_webhook_set: true }) })).toEqual([]);
  });

  it("flags an on boon with no model, and router warnings", () => {
    const found = needsYou({ ...base, alerts: null, boons: boons({ speculation_draft_model: null }), readiness: readiness({ findings: [{ code: "no_tags", severity: "warn", title: "No model is tagged", detail: "Tag some." }, { code: "fyi", severity: "info", title: "FYI", detail: "" }] as never }) });
    expect(found.map((f) => f.key)).toEqual(["spec-model", "router-no_tags"]);
  });

  it("flags web search switched on with no search tool, and summarizes it once picked", () => {
    const found = needsYou({ ...base, alerts: null, boons: boons({ web_search_enabled: true, web_search_tool: null }) });
    expect(found.map((f) => f.key)).toEqual(["search-tool"]);
    expect(found[0].href).toBe("#boon-web_search");
    expect(boonSummary("web_search", boons({ web_search_tool: "searxng-search" }))).toBe("searxng-search · 5 results a search · up to 3 searches a request");
    expect(boonSummary("web_search", boons())).toBe("No search tool picked");
  });

  it("flags a model holding tool boons while Function calling is off", () => {
    const one = needsYou({ ...base, alerts: null, models: [model("nemotron-3-nano", { boons: ["image_generation"], supports_function_calling: false })] });
    expect(one).toEqual([expect.objectContaining({ key: "boons-need-function-calling", title: "nemotron-3-nano has Image generation, but Function calling is off", href: "/models/nemotron-3-nano#set-native" })]);
    const two = needsYou({ ...base, alerts: null, models: [model("a", { boons: ["web_search"] }), model("b", { boons: ["image_generation", "web_search"] }), model("c", { boons: ["web_search"], supports_function_calling: true })] }).filter((f) => f.key === "boons-need-function-calling");
    expect(two).toEqual([expect.objectContaining({ title: "2 models have tool boons, but Function calling is off", href: "/models" })]);
    expect(two[0].detail).toMatch(/^a, b\. /);
  });

  it("counts a model as asking for web search when it lists the boon", () => {
    const models = [model("glm-5-3", { boons: ["vision", "image_generation", "web_search"] }), model("b")];
    expect(modelsAsking("web_search", models)).toEqual(["glm-5-3"]);
  });

  it("describes a cache reconcile in words", () => {
    expect(describeResync({ keys: 3, models: 1, mcp_servers: 0, keys_pruned: 0, model_names_pruned: 0, mcp_servers_pruned: 0 })).toContain("3 keys");
  });
});
