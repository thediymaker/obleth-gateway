// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PortalKeys } from "./portal-keys";
import { createPortalKey, deletePortalKey, disablePortalKey } from "@/app/actions/portal";
import type { ApiKey } from "@/lib/obleth";

vi.mock("@/app/actions/portal", () => ({
  createPortalKey: vi.fn(),
  deletePortalKey: vi.fn(),
  disablePortalKey: vi.fn(),
}));

const key = (over: Partial<ApiKey>): ApiKey =>
  ({
    id: "k1",
    tenant_id: "t1",
    name: "lab-runner",
    description: "",
    key_prefix: "obl-abc",
    kind: "secret",
    identity_issuer: null,
    identity_subject: null,
    identity_claims: null,
    weight: 100,
    max_in_flight: null,
    budget_tokens: null,
    budget_cost_usd: null,
    budget_period: null,
    budget_started_at: null,
    disabled: false,
    tracing_enabled: false,
    end_user_fairshare: false,
    ...over,
  }) as ApiKey;

let root: Root;
let host: HTMLDivElement;
const button = (label: string) =>
  [...document.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === label);
const dialog = () => document.querySelector('[role="dialog"]');

async function render(keys: ApiKey[]) {
  await act(async () =>
    root.render(<PortalKeys keys={keys} keyUsage={[]} gatewayBase="https://gw.example" defaultModel="alpha" />),
  );
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.mocked(disablePortalKey).mockReset();
  vi.mocked(deletePortalKey).mockReset();
  vi.mocked(createPortalKey).mockReset();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

it("asks before turning a key off, and cancel makes no call", async () => {
  await render([key({})]);
  await act(async () => button("Turn off…")!.click());
  expect(dialog()?.textContent).toContain("Turn off lab-runner?");
  await act(async () => button("Cancel")!.click());
  expect(dialog()).toBeNull();
  expect(disablePortalKey).not.toHaveBeenCalled();
});

it("turns the key off once confirmed", async () => {
  vi.mocked(disablePortalKey).mockResolvedValue({ ok: true });
  await render([key({})]);
  await act(async () => button("Turn off…")!.click());
  await act(async () => button("Turn off")!.click());
  expect(disablePortalKey).toHaveBeenCalledTimes(1);
  expect(disablePortalKey).toHaveBeenCalledWith("k1");
});

it("shows the action's error on the key's row", async () => {
  vi.mocked(disablePortalKey).mockResolvedValue({ ok: false, error: "Key not found" });
  await render([key({})]);
  await act(async () => button("Turn off…")!.click());
  await act(async () => button("Turn off")!.click());
  expect(host.textContent).toContain("Key not found");
});

it("offers no Turn off for a key that is already off, and says it is off", async () => {
  await render([key({ disabled: true })]);
  expect(button("Turn off…")).toBeUndefined();
  expect(button("Delete…")).toBeDefined();
  expect(document.querySelector("tbody tr")!.textContent).toContain("Off");
});

it("names only the key or the new key's name: the tenant always comes from the session", async () => {
  vi.mocked(deletePortalKey).mockResolvedValue({ ok: true });
  vi.mocked(createPortalKey).mockResolvedValue({ ok: true, secret: "obl-secret" });
  await render([key({ tenant_id: "t1" })]);

  await act(async () => button("Delete…")!.click());
  expect(dialog()?.textContent).toContain("Delete lab-runner?");
  await act(async () => button("Cancel")!.click());
  expect(deletePortalKey).not.toHaveBeenCalled();
  await act(async () => button("Delete…")!.click());
  await act(async () => button("Delete")!.click());
  expect(vi.mocked(deletePortalKey).mock.calls).toEqual([["k1"]]);

  await act(async () => button("Make key")!.click());
  const name = document.querySelector<HTMLInputElement>("#new-key-name")!;
  name.value = "notebook";
  await act(async () => { name.form!.requestSubmit(); });
  expect(vi.mocked(createPortalKey).mock.calls).toEqual([["notebook"]]);
  expect(document.body.textContent).toContain("obl-secret");
});
