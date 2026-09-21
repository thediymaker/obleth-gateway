// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { KeyManager } from "./key-manager";
import type { ApiKey, Tenant } from "@/lib/obleth";

const mocks = vi.hoisted(() => ({
  createKey: vi.fn(),
  updateKey: vi.fn(),
  deleteKey: vi.fn(),
  deleteKeys: vi.fn(),
  deleteFilteredKeys: vi.fn(),
  toggleKey: vi.fn(),
  toggleKeyTracing: vi.fn(),
}));
vi.mock("@/app/actions", () => ({
  createKeyAction: mocks.createKey,
  updateKeyAction: mocks.updateKey,
  deleteKeyAction: mocks.deleteKey,
  deleteKeysAction: mocks.deleteKeys,
  deleteFilteredKeysAction: mocks.deleteFilteredKeys,
  toggleKeyAction: mocks.toggleKey,
  toggleKeyTracingAction: mocks.toggleKeyTracing,
}));
vi.mock("next/navigation", () => ({
  useRouter: () => ({ refresh: vi.fn() }),
}));

const tenant: Tenant = {
  id: "t1",
  name: "Research",
  fairshare_group: "research",
  weight: 100,
  tokens_per_minute: 0,
  max_in_flight: null,
  description: "",
  organization: "",
  contact_email: "",
  status: "active",
  timezone: "UTC",
  active_from: null,
  active_until: null,
  weekly_windows: null,
  budget_tokens: null,
  budget_cost_usd: null,
  budget_period: null,
  budget_started_at: null,
  allowed_models: null,
  guardrails_policy: null,
  compression_policy: null,
  tracing_enabled: false,
  synthetic: false,
  created_at: "",
  updated_at: "",
};

const key: ApiKey = {
  id: "k1",
  tenant_id: "t1",
  name: "alice",
  description: "",
  key_prefix: "sk-abc",
  kind: "secret",
  identity_issuer: null,
  identity_subject: null,
  identity_claims: null,
  weight: 250,
  max_in_flight: 3,
  budget_tokens: null,
  budget_cost_usd: null,
  budget_period: null,
  budget_started_at: null,
  disabled: false,
  tracing_enabled: false,
  created_at: "",
  updated_at: "",
};

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  mocks.createKey.mockReset();
  mocks.updateKey.mockReset();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

async function render() {
  await act(async () => root.render(<KeyManager tenants={[tenant]} keys={[key]} keyUsage={[]} />));
}
// Dialog content portals to document.body, outside `host`, so buttons and
// fields opened from a dialog are looked up against the whole document.
function button(text: string) {
  const result = [...document.querySelectorAll<HTMLButtonElement>("button")].find(
    (b) => b.textContent?.includes(text) || b.getAttribute("aria-label") === text,
  );
  if (!result) throw new Error(`Missing button: ${text}`);
  return result;
}
async function click(text: string) {
  await act(async () => button(text).click());
}

describe("key manager fairshare fields", () => {
  it("exposes fairshare weight and per-model cap on create and edit forms", async () => {
    await render();
    await click("New key");
    expect(document.querySelector<HTMLInputElement>('input[name="weight"]')!.value).toBe("100");
    expect(document.querySelector('input[name="max_in_flight"]')).toBeTruthy();
  });

  it("exposes fairshare weight and per-model cap on the key edit form", async () => {
    await render();
    await click("Expand alice");
    expect(document.querySelector<HTMLInputElement>('input[name="weight"]')!.value).toBe("250");
    expect(document.querySelector<HTMLInputElement>('input[name="max_in_flight"]')!.value).toBe("3");
  });
});
