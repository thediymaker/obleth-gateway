// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PortalKeys } from "./portal-keys";
import { disablePortalKey } from "@/app/(portal)/portal-actions";
import type { ApiKey } from "@/lib/obleth";

vi.mock("@/app/(portal)/portal-actions", () => ({
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
  await act(async () => button("Disable")!.click());
  expect(dialog()?.textContent).toContain('Turn off "lab-runner"?');
  await act(async () => button("Cancel")!.click());
  expect(dialog()).toBeNull();
  expect(disablePortalKey).not.toHaveBeenCalled();
});

it("turns the key off once confirmed", async () => {
  vi.mocked(disablePortalKey).mockResolvedValue({ ok: true });
  await render([key({})]);
  await act(async () => button("Disable")!.click());
  await act(async () => button("Turn off")!.click());
  expect(disablePortalKey).toHaveBeenCalledTimes(1);
  expect(disablePortalKey).toHaveBeenCalledWith("k1");
});

it("shows the action's error on the key's row", async () => {
  vi.mocked(disablePortalKey).mockResolvedValue({ ok: false, error: "Key not found" });
  await render([key({})]);
  await act(async () => button("Disable")!.click());
  await act(async () => button("Turn off")!.click());
  expect(host.textContent).toContain("Key not found");
});

it("offers no Disable control for a key that is already off", async () => {
  await render([key({ disabled: true })]);
  expect(button("Disable")).toBeUndefined();
  expect(button("Delete")).toBeDefined();
});
