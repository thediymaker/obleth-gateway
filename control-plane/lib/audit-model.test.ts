import { describe, expect, it } from "vitest";
import { auditHref, buildNames, burstSummary, changeLine, fieldChanges, filtersFromParams, groupBursts, paramsFor, previousOf, queryFor, tailOf, thingHref, verbOf } from "./audit-model";
import type { AuditEntry } from "./obleth";

let id = 1000;
const ev = (over: Partial<AuditEntry>): AuditEntry => ({ id: id--, ts: "2026-09-25T16:06:00Z", actor: "admin", action: "update_model", entity_type: "model", entity_id: "m1", detail: {}, ...over });

describe("sentences", () => {
  it("says what happened in words", () => {
    expect(verbOf(ev({ action: "create_key", entity_type: "api_key" }))).toBe("made key");
    expect(verbOf(ev({ action: "set_tenant_allowlist", entity_type: "tenant" }))).toBe("changed the allowlist of tenant");
    expect(verbOf(ev({ action: "disable_key", entity_type: "api_key" }))).toBe("turned off key");
    expect(verbOf(ev({ action: "enable_key_tracing", entity_type: "api_key" }))).toBe("turned on tracing for key");
    expect(verbOf(ev({ action: "set_auto_router_settings", entity_type: "settings" }))).toBe("changed");
  });

  it("names deleted things from the log's own create events", () => {
    const created = ev({ action: "create_tenant", entity_type: "tenant", entity_id: "t9", detail: { name: "e2e-scope-ebead1" } });
    const deleted = ev({ action: "delete_tenant", entity_type: "tenant", entity_id: "t9", detail: { keys_removed: 1 } });
    const names = buildNames({ t1: "service-accounts" }, [deleted, created]);
    expect(names.t9).toBe("e2e-scope-ebead1");
    expect(tailOf(deleted, names)).toBe("and its 1 key");
    expect(thingHref(deleted, names, new Set(["t1"]))).toBeNull();
    expect(tailOf(ev({ action: "create_key", entity_type: "api_key", detail: { tenant_id: "t1" } }), names)).toBe("in service-accounts");
  });

  it("links a thing to its page", () => {
    const names = { m1: "glm-5-3", t1: "rc users" };
    expect(thingHref(ev({}), names, new Set(["m1"]))).toBe("/models/glm-5-3");
    expect(thingHref(ev({ entity_type: "tenant", entity_id: "t1" }), names, new Set(["t1"]))).toBe("/tenants/rc%20users");
    expect(thingHref(ev({ entity_type: "settings", entity_id: "auto_router", action: "set_auto_router_settings" }), names, new Set())).toBe("/settings?tab=routing");
    expect(auditHref("model", "m1")).toBe("/audit?entity=model%3Am1");
  });
});

describe("bursts", () => {
  it("folds the same change by the same person within a minute", () => {
    const burst = ["a", "b", "c", "d"].map((m, i) => ev({ entity_id: m, ts: `2026-09-25T16:06:${String(50 - i * 10).padStart(2, "0")}Z` }));
    const other = ev({ actor: "jlee379@asu.edu", action: "create_key", entity_type: "api_key", entity_id: "k1", ts: "2026-09-25T16:05:00Z" });
    const later = ev({ entity_id: "e", ts: "2026-09-25T15:00:00Z" });
    const groups = groupBursts([...burst, other, later]);
    expect(groups.map((g) => g.events.length)).toEqual([4, 1, 1]);
    const s = burstSummary(groups[0], { a: "qwen38-27b", b: "qwen36-27b", c: "flux-2", d: "wan-2-2" });
    expect(s).toMatchObject({ verb: "changed 4 models", things: ["qwen38-27b", "qwen36-27b", "flux-2"], more: 1, sameThing: false });
  });

  it("counts repeats on one thing", () => {
    const twice = [ev({ entity_type: "settings", entity_id: "auto_router", action: "set_auto_router_settings", ts: "2026-09-25T22:35:30Z" }), ev({ entity_type: "settings", entity_id: "auto_router", action: "set_auto_router_settings", ts: "2026-09-25T22:35:00Z" })];
    const [g] = groupBursts(twice);
    expect(burstSummary(g, {})).toMatchObject({ sameThing: true, times: 2 });
  });
});

describe("what changed", () => {
  it("compares with the same thing's previous change", () => {
    // Newer events have higher ids.
    const after = ev({ entity_type: "settings", entity_id: "auto_router", action: "set_auto_router_settings", detail: { classifier_model: "router-classifier-v2", cost_weight: 0.4, updated_at: "x" } });
    const before = ev({ entity_type: "settings", entity_id: "auto_router", action: "set_auto_router_settings", detail: { classifier_model: "router-classifier-v1", cost_weight: 0.4 } });
    const all = [after, before];
    expect(previousOf(after, all)).toBe(before);
    expect(fieldChanges(after, before)).toEqual([
      { field: "classifier_model", before: "router-classifier-v1", after: "router-classifier-v2", changed: true },
      { field: "cost_weight", before: "0.4", after: "0.4", changed: false },
    ]);
    expect(changeLine(after, before)).toBe("classifier model router-classifier-v1 → router-classifier-v2");
    expect(changeLine(before, undefined)).toBeNull();
  });
});

describe("filters", () => {
  it("round-trip through the address and become a gateway query", () => {
    const f = filtersFromParams({ range: "30d", who: "jlee379@asu.edu", kind: "keys", q: " comfy " });
    expect(paramsFor(f)).toBe("?range=30d&who=jlee379%40asu.edu&kind=keys&q=comfy");
    const q = queryFor(f, Date.parse("2026-09-28T00:00:00Z"));
    expect(q).toMatchObject({ actor: "jlee379@asu.edu", entityType: "api_key", q: "comfy", since: "2026-08-29T00:00:00.000Z" });
    expect(filtersFromParams({ entity: "model:m1" })).toMatchObject({ range: "all", entity: "model:m1" });
    expect(queryFor(filtersFromParams({ entity: "model:m1" }))).toMatchObject({ entityType: "model", entityId: "m1", since: undefined });
  });
});
