// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { saveSettingsAction } from "@/app/actions";
import { settingsData } from "./fixtures";
import { SettingsPage } from "./settings-page";

vi.mock("@/app/actions", () => ({
  saveSettingsAction: vi.fn(async () => ({ ok: true })),
  testAlertAction: vi.fn(async () => ({ ok: true, results: [] })),
  compactUsageAction: vi.fn(async () => ({ ok: true, partitionsDropped: 0 })),
  resyncCacheAction: vi.fn(async () => ({ ok: true })),
  testEnergyQueryAction: vi.fn(async () => ({ ok: true })),
  restoreBackupAction: vi.fn(async () => ({ ok: true })),
}));
vi.mock("next/navigation", () => ({ useRouter: () => ({ refresh: vi.fn(), push: vi.fn(), replace: vi.fn() }), usePathname: () => "/settings" }));

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true, IntersectionObserver: class { observe() {} unobserve() {} disconnect() {} } });
  Element.prototype.scrollIntoView = () => {};
  vi.mocked(saveSettingsAction).mockClear();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

const render = (node: React.ReactNode) => act(async () => root.render(node));
const frame = () => act(async () => { await new Promise((r) => requestAnimationFrame(r)); });
const button = (text: string) => {
  const b = [...document.querySelectorAll<HTMLButtonElement>("button")].find((x) => x.textContent?.trim() === text || x.getAttribute("aria-label") === text);
  if (!b) throw new Error(`Missing button: ${text}`);
  return b;
};
async function type(el: HTMLInputElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}
const saved = () => vi.mocked(saveSettingsAction).mock.calls[0][0] as FormData;

describe("the Settings page", () => {
  it("leads with what needs you, and marks boons models ask for", async () => {
    await render(<SettingsPage data={settingsData()} />);
    const needs = host.querySelector("#needs")!;
    expect(needs.textContent).toContain("Needs you · 2");
    expect(needs.textContent).toContain("Nobody is alerted");
    expect(needs.textContent).toContain("glm-5-3 asks for Vision, which is off");
    expect(host.querySelector("#boon-vision")!.textContent).toContain("1 asks, off");
    expect(host.querySelector('nav[aria-label="Settings sections"]')!.textContent).toContain("2 of 7 on");
  });

  it("saves only the section that changed", async () => {
    await render(<SettingsPage data={settingsData()} />);
    await type(host.querySelector<HTMLInputElement>('input[name="alerts.quiet_minutes"]')!, "10");
    await frame();
    expect(host.textContent).toContain("1 unsaved");
    await act(async () => button("Save changes").click());
    expect(saved().get("sections")).toBe("alerts");
    expect(saved().get("alerts.quiet_minutes")).toBe("10");
  });

  it("sets the weights from a profile", async () => {
    await render(<SettingsPage data={settingsData()} />);
    await act(async () => [...host.querySelectorAll<HTMLButtonElement>("#routing button[aria-pressed]")].find((b) => b.textContent?.startsWith("Cost saver"))!.click());
    await frame();
    await act(async () => button("Save changes").click());
    expect(saved().get("sections")).toBe("routing");
    expect(saved().get("routing.cost_weight")).toBe("0.9");
    expect(saved().get("routing.difficulty")).toBeNull();
  });

  it("asks before keeping less history", async () => {
    await render(<SettingsPage data={settingsData()} />);
    await act(async () => button("90 days").click());
    await frame();
    await act(async () => button("Save changes").click());
    expect(document.body.textContent).toContain("Keep history for 90 days?");
    expect(saveSettingsAction).not.toHaveBeenCalled();
    await act(async () => button("Keep less").click());
    expect(saved().get("retention.days")).toBe("90");
  });

  it("doesn't offer a section it couldn't read", async () => {
    await render(<SettingsPage data={settingsData({ router: null })} />);
    expect(host.querySelector("#routing")!.textContent).toContain("couldn't be read");
    expect(host.querySelector('[name^="routing."]')).toBeNull();
  });

  it("finds a setting by what it does", async () => {
    await render(<SettingsPage data={settingsData()} />);
    const find = host.querySelector<HTMLInputElement>('input[aria-label="Find a setting"]')!;
    await act(async () => find.focus());
    await type(find, "prometheus");
    expect([...host.querySelectorAll('[role="option"]')].map((o) => o.textContent)).toEqual(["Energy"]);
  });
});
