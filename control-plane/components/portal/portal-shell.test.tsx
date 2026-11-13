// @vitest-environment jsdom
//
// TDDEV-5338 regression: when selectPortalTenant completes, React 19 commits
// the RSC payload (current flips to the new tenant) together with a
// form.reset() on the action <form> in that commit's mutation phase
// (react-dom commitMutationEffects, flag 1024 — verified in the react-dom
// 19.3.1 source: recursivelyResetForms). The browser bounces the controlled
// <select> back to its first option; the committed `value` prop is unchanged
// across that commit, so React's diff never rewrites the DOM and the
// switcher stays desynced on the previous tenant until the next full
// navigation. TenantSwitcher must re-sync the DOM select to the committed
// value in the commit where the RSC payload flips `current`.
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@/app/(portal)/portal-actions", () => ({
  selectPortalTenant: vi.fn(),
}));

import { selectPortalTenant } from "@/app/(portal)/portal-actions";
import type { PortalTenant } from "@/lib/portal-context";
import { TenantSwitcher } from "./portal-shell";

const PERSONAL: PortalTenant = { id: "tenant-personal", name: "personal", kind: "personal", role: "owner" };
const STANDARD: PortalTenant = { id: "tenant-test", name: "test.local", kind: "standard", role: "member" };
const TENANTS = [PERSONAL, STANDARD];

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.clearAllMocks();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

const select = () => host.querySelector<HTMLSelectElement>("select")!;
const form = () => host.querySelector<HTMLFormElement>("form")!;

async function render(current: PortalTenant) {
  await act(async () => root.render(<TenantSwitcher tenants={TENANTS} current={current} />));
}

async function pick(tenantId: string) {
  await act(async () => {
    const el = select();
    el.value = tenantId;
    el.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

// Simulates the real app's action-completion commit: the RSC payload flips
// `current` and React's mutation phase resets the action form in the same
// commit. (Passive effects run after the mutation phase, so the reset lands
// before TenantSwitcher's effects — the exact order the fix relies on.)
async function completeAction(current: PortalTenant) {
  await act(async () => {
    form().reset();
    root.render(<TenantSwitcher tenants={TENANTS} current={current} />);
  });
}

describe("TenantSwitcher", () => {
  it("submits the server action and keeps the new tenant after the post-action form.reset() commit (TDDEV-5338)", async () => {
    await render(PERSONAL);
    expect(select().value).toBe(PERSONAL.id);

    await pick(STANDARD.id);
    expect(selectPortalTenant).toHaveBeenCalledTimes(1);
    const formData = vi.mocked(selectPortalTenant).mock.calls[0]?.[0] as FormData;
    expect(formData.get("tenantId")).toBe(STANDARD.id);

    // The action completes: RSC payload flips current, form.reset() bounces
    // the controlled select back to its first option (personal) in the same
    // commit's mutation phase.
    await completeAction(STANDARD);

    // The select shows the new tenant, the badge matches, and it stays there
    // through a later tick without any further update.
    expect(select().value).toBe(STANDARD.id);
    expect(form().querySelector("span")?.textContent).toBe(STANDARD.kind);

    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
      root.render(<TenantSwitcher tenants={TENANTS} current={STANDARD} />);
    });
    expect(select().value).toBe(STANDARD.id);
  });

  it("switching back to the first tenant also survives the post-action reset", async () => {
    await render(STANDARD);
    expect(select().value).toBe(STANDARD.id);

    await pick(PERSONAL.id);
    await completeAction(PERSONAL);

    expect(select().value).toBe(PERSONAL.id);
    expect(form().querySelector("span")?.textContent).toBe(PERSONAL.kind);
  });

  it("stays in sync on an RSC re-render that does not change the tenant", async () => {
    await render(STANDARD);
    expect(select().value).toBe(STANDARD.id);

    // RSC refresh: same tenant, form.reset() in the commit — the select must
    // remain on the current tenant (no bounce, no flicker to a stale value).
    await completeAction(STANDARD);
    expect(select().value).toBe(STANDARD.id);
  });
});
