import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { createTenantAction } from "@/app/actions";
import { AddTenantSheet } from "./add-tenant";
import type { Tenant } from "@/lib/obleth";

const push = vi.fn();
vi.mock("next/navigation", () => ({ useRouter: () => ({ push }) }));
vi.mock("@/app/actions", () => ({ createTenantAction: vi.fn() }));

let root: Root;
let host: HTMLDivElement;
const field = (name: string) => document.querySelector<HTMLInputElement>(`[name="${name}"]`)!;

beforeEach(async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  push.mockReset();
  vi.mocked(createTenantAction).mockReset();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  const tenants = [{ id: "t1", name: "voyager", fairshare_group: "default" }] as Tenant[];
  await act(async () => root.render(<AddTenantSheet open onClose={() => {}} tenants={tenants} />));
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });

it("adds a tenant with the settings page's fields and defaults, then opens its page", async () => {
  expect(field("fairshare_group").value).toBe("default");
  expect(field("weight").value).toBe("100");
  expect(field("tokens_per_minute").value).toBe("");
  field("name").value = "research-computing";
  field("organization").value = "Research Computing";
  vi.mocked(createTenantAction).mockResolvedValue({ ok: true, tenant: { id: "t2", name: "research-computing" } });
  await act(async () => document.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })));
  const data = vi.mocked(createTenantAction).mock.calls[0][0];
  expect([...data.keys()].sort()).toEqual(["fairshare_group", "max_in_flight", "name", "organization", "tokens_per_minute", "weight"]);
  expect([data.get("name"), data.get("organization"), data.get("fairshare_group"), data.get("weight")]).toEqual(["research-computing", "Research Computing", "default", "100"]);
  expect(push).toHaveBeenCalledWith("/tenants/research-computing");
});

it("keeps the sheet open with the gateway's reason when it refuses", async () => {
  field("name").value = "voyager";
  vi.mocked(createTenantAction).mockResolvedValue({ ok: false, error: "A tenant called voyager already exists" });
  await act(async () => document.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true })));
  expect(document.body.textContent).toContain("A tenant called voyager already exists");
  expect(push).not.toHaveBeenCalled();
});
