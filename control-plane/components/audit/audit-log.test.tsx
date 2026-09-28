// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AuditLog } from "./audit-log";
import { filtersFromParams } from "@/lib/audit-model";
import type { AuditEntry } from "@/lib/obleth";

const replace = vi.fn();
vi.mock("next/navigation", () => ({ useRouter: () => ({ replace, push: vi.fn(), refresh: vi.fn() }), usePathname: () => "/audit" }));

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  replace.mockClear();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

const entries: AuditEntry[] = [
  { id: 10, ts: "2026-09-25T22:38:00Z", actor: "admin", action: "set_auto_router_settings", entity_type: "settings", entity_id: "auto_router", detail: { classifier_model: "router-classifier-v2" } },
  { id: 9, ts: "2026-09-25T22:10:00Z", actor: "admin", action: "set_auto_router_settings", entity_type: "settings", entity_id: "auto_router", detail: { classifier_model: "router-classifier-v1" } },
  { id: 8, ts: "2026-09-25T17:29:00Z", actor: "jlee379@asu.edu", action: "create_key", entity_type: "api_key", entity_id: "k1", detail: { tenant_id: "t1", prefix: "sk_22e7" } },
  ...["m1", "m2", "m3", "m4"].map((m, i) => ({ id: 7 - i, ts: `2026-09-25T16:06:${String(40 - i * 5).padStart(2, "0")}Z`, actor: "admin", action: "update_model", entity_type: "model", entity_id: m, detail: {} })),
];
const names = { k1: "comfy-ui", t1: "service-accounts", m1: "qwen38-27b", m2: "qwen36-27b", m3: "flux-2", m4: "wan-2-2" };

describe("the audit log", () => {
  it("reads as sentences, folds bursts, and shows what a change changed", async () => {
    await act(async () => root.render(<AuditLog entries={entries} filters={filtersFromParams({ range: "all" })} names={names} alive={Object.keys(names)} total={null} />));
    const list = host.querySelector('[aria-label="Changes"]')!;
    expect(list.textContent).toContain("jlee379@asu.edu made key comfy-ui in service-accounts");
    expect(list.textContent).toContain("admin token changed 4 models: qwen38-27b, qwen36-27b, flux-2 and 1 more");
    expect(list.textContent).toContain("classifier model router-classifier-v1 → router-classifier-v2");
    expect(host.querySelector<HTMLAnchorElement>('a[href="/keys?key=k1"]')).toBeTruthy();
    const burst = [...host.querySelectorAll("button")].find((b) => b.textContent === "4 changes ›")!;
    await act(async () => burst.click());
    expect(list.textContent).toContain("wan-2-2");
  });

  it("keeps its filters in the address", async () => {
    await act(async () => root.render(<AuditLog entries={entries} filters={filtersFromParams({})} names={names} alive={[]} total={null} />));
    await act(async () => [...host.querySelectorAll("button")].find((b) => b.textContent === "30 days")!.click());
    expect(replace).toHaveBeenCalledWith("/audit?range=30d", { scroll: false });
  });
});
