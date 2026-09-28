// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { saveTenantSettingsAction } from "@/app/actions";
import { TenantSettings } from "./tenant-settings";
import type { Tenant } from "@/lib/obleth";

vi.mock("@/app/actions", () => ({ saveTenantSettingsAction: vi.fn(async () => ({ ok: true })) }));

const tenant = (over: Partial<Tenant> = {}): Tenant => ({
  id: "t1", name: "rcusers", fairshare_group: "default", weight: 150, tokens_per_minute: 50000, max_in_flight: 8,
  description: "", organization: "ASU", contact_email: "", status: "active", timezone: "America/Phoenix",
  active_from: "2026-08-17T07:00:00Z", active_until: null,
  weekly_windows: [1, 2, 3, 4, 5].map((day) => ({ day, start_min: 420, end_min: 1320 })),
  budget_tokens: null, budget_cost_usd: 300, budget_period: "monthly", budget_started_at: null,
  allowed_models: null, guardrails_policy: null, compression_policy: null, tracing_enabled: false, synthetic: false,
  created_at: "", updated_at: "", ...over,
});

let root: Root;
let host: HTMLDivElement;
const frame = () => act(async () => { await new Promise((r) => requestAnimationFrame(() => r(null))); });

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.mocked(saveTenantSettingsAction).mockClear();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

async function render(t = tenant()) {
  const others = [t, tenant({ id: "t2", name: "brain-training", weight: 100 }), tenant({ id: "t3", name: "svc", weight: 100 })];
  await act(async () => root.render(<TenantSettings tenant={t} tenants={others} models={["m1", "m2"]} budget={null} onDirty={() => {}} onSaved={() => {}} />));
  await frame();
}

async function type(name: string, value: string) {
  const el = host.querySelector<HTMLInputElement>(`[name="${name}"]`)!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await frame();
}

describe("a tenant's settings", () => {
  it("opens with nothing to save, even with hours, dates and a budget set", async () => {
    await render();
    expect(document.querySelector('[aria-label="Unsaved changes"]')).toBeNull();
  });

  it("previews the weight's share and saves only what changed", async () => {
    await render();
    await type("weight", "200");
    expect(host.textContent).toContain("50%");
    const bar = document.querySelector('[aria-label="Unsaved changes"]')!;
    expect(bar.textContent).toContain("1 unsaved");
    expect(bar.textContent).toContain("Weight");
    const save = [...bar.querySelectorAll("button")].find((b) => b.textContent === "Save changes")!;
    await act(async () => save.click());
    const sent = vi.mocked(saveTenantSettingsAction).mock.calls[0][0];
    expect(sent.get("sections")).toBe("weight");
    expect(sent.get("weight")).toBe("200");
  });

  it("opens hours with the week grid's preset and sends them as windows", async () => {
    await render();
    const preset = [...host.querySelectorAll("button")].find((b) => b.textContent === "Weekdays 9–17")!;
    await act(async () => preset.click());
    await frame();
    const windows = JSON.parse(host.querySelector<HTMLInputElement>('[name="weekly_windows"]')!.value);
    expect(windows).toHaveLength(5);
    expect(windows[0]).toEqual({ day: 1, start_min: 540, end_min: 1020 });
    expect(document.querySelector('[aria-label="Unsaved changes"]')!.textContent).toContain("Weekly hours");
  });

  it("won't save a harmful-content scanner without a guard model", async () => {
    await render(tenant({ guardrails_policy: { action: "block", input_scanners: ["harm"], output_scanners: [], guard_model: null, ban_keywords: [], fail_open: true } }));
    await type("weight", "151");
    const bar = document.querySelector('[aria-label="Unsaved changes"]')!;
    await act(async () => [...bar.querySelectorAll("button")].find((b) => b.textContent === "Save changes")!.click());
    expect(saveTenantSettingsAction).not.toHaveBeenCalled();
    expect(bar.textContent).toContain("pick a guard model");
  });
});
