// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { KeysList } from "./keys-list";
import type { ApiKey, Tenant } from "@/lib/obleth";

const mocks = vi.hoisted(() => ({
  createKey: vi.fn(),
  moveKeys: vi.fn(),
}));
vi.mock("@/app/actions", () => ({
  createKeyAction: mocks.createKey,
  deleteKeyAction: vi.fn(),
  deleteKeysAction: vi.fn(),
  moveKeysAction: mocks.moveKeys,
  replaceKeyAction: vi.fn(),
  saveKeySettingsAction: vi.fn(async () => ({ ok: true })),
  setKeysBudgetAction: vi.fn(),
  setKeysDisabledAction: vi.fn(),
}));
vi.mock("next/navigation", () => ({
  useRouter: () => ({ refresh: vi.fn(), push: vi.fn() }),
  usePathname: () => "/keys",
}));

const tenant = (id: string, name: string): Tenant => ({
  id, name, fairshare_group: "default", weight: 100, tokens_per_minute: 0, max_in_flight: null, description: "",
  organization: "", contact_email: "", status: "active", timezone: "UTC", active_from: null, active_until: null,
  weekly_windows: null, budget_tokens: null, budget_cost_usd: null, budget_period: null, budget_started_at: null,
  allowed_models: null, guardrails_policy: null, compression_policy: null, tracing_enabled: false, synthetic: false,
  created_at: "", updated_at: "",
});

const key: ApiKey = {
  id: "k1", tenant_id: "t1", name: "alice", description: "", key_prefix: "sk-abc", kind: "secret",
  identity_issuer: null, identity_subject: null, identity_claims: null, weight: 250, max_in_flight: 3,
  budget_tokens: null, budget_cost_usd: null, budget_period: null, budget_started_at: null,
  disabled: false, tracing_enabled: false, end_user_fairshare: false, created_at: "", updated_at: "",
};

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  mocks.createKey.mockReset();
  mocks.moveKeys.mockReset();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

async function render() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, enabled: false } } });
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <KeysList tenants={[tenant("t1", "Research"), tenant("t2", "Teaching")]} keys={[key]} usage={[]} budgets={[]} initial={{}} />
      </QueryClientProvider>,
    ),
  );
}
// Sheets and dialogs render over the page, so look across the whole document.
function button(text: string) {
  const result = [...document.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === text || b.getAttribute("aria-label") === text);
  if (!result) throw new Error(`Missing button: ${text}`);
  return result;
}
async function click(text: string) {
  await act(async () => button(text).click());
}

describe("keys", () => {
  it("offers a new key's fairshare weight and per-model cap", async () => {
    await render();
    await click("New key");
    expect(document.querySelector<HTMLInputElement>('input[name="weight"]')!.value).toBe("100");
    expect(document.querySelector('input[name="max_in_flight"]')).toBeTruthy();
  });

  it("shows a key's weight and per-model cap in its panel", async () => {
    await render();
    await click("Open alice");
    expect(document.querySelector<HTMLInputElement>('input[name="weight"]')!.value).toBe("250");
    expect(document.querySelector<HTMLInputElement>('input[name="max_in_flight"]')!.value).toBe("3");
  });

  it("moves a key into a new tenant with its old tenant's settings", async () => {
    mocks.moveKeys.mockResolvedValue({ done: 1, failed: [], tenantId: "t9" });
    await render();
    await click("Open alice");
    await click("Move…");
    await click("A new tenant");
    const name = document.querySelector<HTMLInputElement>('input[name="new_tenant_name"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(name, "alice-own");
      name.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await click("Make it and move");
    expect(mocks.moveKeys).toHaveBeenCalledWith(["k1"], { newTenant: { name: "alice-own", copyFrom: "t1" } });
    expect(document.body.textContent).toContain("alice-own is made and the keys are in it.");
  });
});
