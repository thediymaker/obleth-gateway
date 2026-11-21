// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { KeysList } from "./keys-list";
import type { ApiKey, KeyStats, Tenant } from "@/lib/obleth";
import type { KeyPageData } from "@/lib/key-page-data";
import { EMPTY_KEY_FILTERS } from "@/lib/access-model";

const mocks = vi.hoisted(() => ({
  createKey: vi.fn(),
  moveKeys: vi.fn(),
  setDisabled: vi.fn(),
}));
vi.mock("@/app/actions", () => ({
  createKeyAction: mocks.createKey,
  deleteKeyAction: vi.fn(),
  deleteKeysAction: vi.fn(),
  moveKeysAction: mocks.moveKeys,
  replaceKeyAction: vi.fn(),
  saveKeySettingsAction: vi.fn(async () => ({ ok: true })),
  setKeysBudgetAction: vi.fn(),
  setKeysDisabledAction: mocks.setDisabled,
}));
// A plain select, so a test can pick a sort.
vi.mock("@/components/ui/select", () => ({
  Select: ({ value, onValueChange, options, ...rest }: { value: string; onValueChange: (v: string) => void; options: { value: string; label: string }[]; "aria-label"?: string }) => (
    <select aria-label={rest["aria-label"]} value={value} onChange={(e) => onValueChange(e.target.value)}>
      {options.map((o) => <option key={o.value} value={o.value}>{o.label}</option>)}
    </select>
  ),
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
  disabled: false, tracing_enabled: false, end_user_fairshare: false, background: false, created_at: "", updated_at: "",
};

let host: HTMLDivElement;
let root: Root;
/** The dashboard routes the list asked for, in order, and what they answer. */
let requests: string[];
let answer: (url: URL) => unknown;

const WINDOW = { sinceMs: 1_000, untilMs: 2_000 };
const STATS: KeyStats = {
  keys: 1234, disabled: 4, identity: 0, tenants: 2, used: 1000, used_recent: 50, unused: 234, unused_on: 230,
  requests: 9000, busiest: null, near_budget: 0, near_budget_keys: [],
};
const page = (keys: ApiKey[], next: string | null = null): KeyPageData => ({ keys, usage: [], budgets: [], next });

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  mocks.createKey.mockReset();
  mocks.moveKeys.mockReset();
  mocks.setDisabled.mockReset();
  requests = [];
  answer = (url) => (url.pathname === "/api/live/keys" ? page([key]) : []);
  vi.stubGlobal("fetch", vi.fn(async (input: string) => {
    const url = new URL(input, "http://dash.test");
    requests.push(`${url.pathname}?${url.searchParams.toString()}`);
    return new Response(JSON.stringify(answer(url)), { status: 200, headers: { "content-type": "application/json" } });
  }));
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

async function render(keys: ApiKey[] = [key], next: string | null = null) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  await act(async () =>
    root.render(
      <QueryClientProvider client={client}>
        <KeysList
          tenants={[tenant("t1", "Research"), tenant("t2", "Teaching")]}
          first={{ filters: EMPTY_KEY_FILTERS, sort: "last-used", page: page(keys, next) }}
          usageWindow={WINDOW}
          stats={STATS}
          initial={{}}
        />
      </QueryClientProvider>,
    ),
  );
}
/** Let fetches and the renders they cause finish. */
async function settle() {
  for (let i = 0; i < 5; i++) await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
}
const listRequests = () => requests.filter((r) => r.startsWith("/api/live/keys?"));
const names = () => [...host.querySelectorAll('[aria-label="Keys"] button[aria-label^="Open "]')].map((b) => b.getAttribute("aria-label")!.slice(5));
const keyNamed = (name: string, id = name): ApiKey => ({ ...key, id, name });
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

  it("marks a background key in the list and shows its switch on", async () => {
    await render([key, { ...key, id: "k2", name: "runner", background: true }]);
    const rows = [...document.querySelectorAll<HTMLButtonElement>("button")].filter((b) => b.getAttribute("aria-label")?.startsWith("Open "));
    expect(rows.find((b) => b.getAttribute("aria-label") === "Open alice")!.textContent).not.toContain("Background");
    expect(rows.find((b) => b.getAttribute("aria-label") === "Open runner")!.textContent).toContain("Background");
    await click("Open runner");
    expect(document.body.textContent).toContain("Background priority");
    expect(document.body.textContent).toContain("Uses only spare capacity.");
    expect(document.querySelector<HTMLInputElement>('input[name="background"]')!.checked).toBe(true);
  });

  it("shows background priority off for an ordinary key", async () => {
    await render();
    await click("Open alice");
    expect(document.querySelector<HTMLInputElement>('input[name="background"]')!.checked).toBe(false);
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

  it("loads the next page from where the last one ended", async () => {
    await render([keyNamed("a")], "c1");
    expect(host.textContent).toContain("1 key so far");
    answer = () => page([keyNamed("b")]);
    await click("Load more");
    await settle();
    expect(listRequests()).toEqual(["/api/live/keys?sort=last_used&since_ms=1000&until_ms=2000&cursor=c1"]);
    expect(names()).toEqual(["a", "b"]);
    expect(host.textContent).not.toContain("Load more");
  });

  it("searches on the gateway once typing pauses, from the first page", async () => {
    await render([keyNamed("a")], "c1");
    answer = () => page([keyNamed("lab-bot")]);
    const box = host.querySelector<HTMLInputElement>('input[aria-label="Search keys"]')!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(box, "lab");
      box.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(listRequests()).toEqual([]);
    await act(async () => { await new Promise((r) => setTimeout(r, 300)); });
    await settle();
    expect(listRequests()).toEqual(["/api/live/keys?q=lab&sort=last_used&since_ms=1000&until_ms=2000"]);
    expect(names()).toEqual(["lab-bot"]);
  });

  it("starts again from the first page when the sort changes", async () => {
    await render([keyNamed("a")], "c1");
    answer = () => page([keyNamed("b")], "c2");
    await click("Load more");
    await settle();
    answer = () => page([keyNamed("z-first")], "n1");
    const sort = host.querySelector<HTMLSelectElement>('select[aria-label="Sort"]')!;
    await act(async () => {
      sort.value = "name";
      sort.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await settle();
    expect(listRequests().at(-1)).toBe("/api/live/keys?sort=name&since_ms=1000&until_ms=2000");
    expect(names()).toEqual(["z-first"]);
  });

  it("selects every matching key by filter, saying how many the gateway counts", async () => {
    mocks.setDisabled.mockResolvedValue({ done: 1234, failed: [] });
    await render([keyNamed("a")], "c1");
    answer = (url) => (url.pathname === "/api/live/keys/count" ? { keys: 1234, tenants: 2 } : page([keyNamed("a")], "c1"));
    await act(async () => host.querySelector<HTMLInputElement>('input[aria-label="Select the 1 keys shown"]')!.click());
    expect(host.querySelector('[aria-label="Selected keys"]')!.textContent).toContain("1 selected");
    await click("Select every matching key");
    await settle();
    expect(host.querySelector('[aria-label="Selected keys"]')!.textContent).toContain("All 1,234 keys selected");
    await click("Turn off");
    await settle();
    expect(requests.filter((r) => r.startsWith("/api/live/keys/count"))).toEqual([
      "/api/live/keys/count?sort=last_used&since_ms=1000&until_ms=2000",
      "/api/live/keys/count?sort=last_used&since_ms=1000&until_ms=2000",
    ]);
    expect(document.body.textContent).toContain("Turn off 1,234 keys?");
    expect(document.body.textContent).toContain("1,233 of them are not loaded on this page");
    await act(async () => [...document.querySelectorAll<HTMLButtonElement>('[role="alertdialog"] button, [role="dialog"] button')].find((b) => b.textContent?.trim() === "Turn off")!.click());
    await settle();
    expect(mocks.setDisabled).toHaveBeenCalledWith({ filter: expect.objectContaining({ sort: "last_used", sinceMs: 1000, untilMs: 2000 }), expected: 1234 }, true);
    expect(document.body.textContent).toContain("Turned off 1,234 keys.");
  });

  it("shows where a key may be used from", async () => {
    await render([{ ...key, allowed_networks: ["10.0.0.0/8"] }]);
    expect(host.querySelector('[aria-label="Keys"]')!.textContent).toContain("Only 10.0.0.0/8");
  });
});
