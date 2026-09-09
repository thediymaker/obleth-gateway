import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { beforeEach, afterEach, expect, it, vi } from "vitest";
import { CreateTenant } from "./create-tenant";
import { TooltipProvider } from "./ui/tooltip";
import { createTenantAction } from "@/app/actions";
vi.mock("@/app/actions", () => ({ createTenantAction: vi.fn() }));
let root: Root;
let host: HTMLDivElement;
const input = (name: string) => host.querySelector<HTMLInputElement>(`[name="${name}"]`)!;
async function tab(label: string) {
  const button = [...host.querySelectorAll<HTMLButtonElement>('[role="tab"]')].find((el) => el.textContent?.startsWith(label))!;
  await act(async () => button.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 0 })));
}
async function submit() { await act(async () => host.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }))); }
beforeEach(async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.mocked(createTenantAction).mockReset();
  host = document.createElement("div"); document.body.append(host); root = createRoot(host);
  await act(async () => root.render(<TooltipProvider><CreateTenant models={["alpha"]} tenantWeights={[100]} /></TooltipProvider>));
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });
it("retains fields through section changes and submits the complete draft from Models", async () => {
  input("name").value = "Research";
  input("organization").value = "Lab";
  await tab("Traffic limits"); input("tokens_per_minute").value = "1200";
  await tab("Budgets"); input("budget_cost_usd").value = "12.50";
  await tab("Basics"); expect(input("name").value).toBe("Research");
  await tab("Models");
  vi.mocked(createTenantAction).mockResolvedValue({ ok: false, error: "Try again" });
  await submit();
  const data = vi.mocked(createTenantAction).mock.calls[0][0];
  expect(data.get("name")).toBe("Research");
  expect(data.get("organization")).toBe("Lab");
  expect(data.get("tokens_per_minute")).toBe("1200");
  expect(data.get("budget_cost_usd")).toBe("12.50");
  expect(data.get("timezone")).toBe("UTC");
  expect(data.get("budget_period")).toBe("lifetime");
  expect(input("name").value).toBe("Research");
  expect(host.querySelector('[role="alert"]')?.textContent).toBe("Try again");
});
it("opens Basics when submitting elsewhere with a missing required name", async () => {
  await tab("Models"); await submit();
  expect(host.querySelector('[role="tab"][data-state="active"]')?.textContent).toContain("Basics");
  expect(createTenantAction).not.toHaveBeenCalled();
});
it("preserves the draft after an unexpected server failure", async () => {
  input("name").value = "Keep me";
  vi.mocked(createTenantAction).mockRejectedValue(new Error("offline"));
  await submit();
  expect(input("name").value).toBe("Keep me");
  expect(host.querySelector('[role="alert"]')?.textContent).toContain("Your draft is still here");
});
