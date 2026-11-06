// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { applyAdviceFixAction, saveSettingsAction } from "@/app/actions";
import type { AdviceReportView } from "@/lib/obleth";
import { settingsData } from "./fixtures";
import { SettingsPage } from "./settings-page";

vi.mock("@/app/actions", () => ({
  saveSettingsAction: vi.fn(async () => ({ ok: true })),
  testAlertAction: vi.fn(async () => ({ ok: true, results: [] })),
  compactUsageAction: vi.fn(async () => ({ ok: true, partitionsDropped: 0 })),
  resyncCacheAction: vi.fn(async () => ({ ok: true })),
  testEnergyQueryAction: vi.fn(async () => ({ ok: true })),
  restoreBackupAction: vi.fn(async () => ({ ok: true })),
  applyAdviceFixAction: vi.fn(async () => ({ ok: true })),
  snoozeAdviceAction: vi.fn(async () => ({ ok: true })),
  restoreAdviceAction: vi.fn(async () => ({ ok: true })),
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

  it("puts measured findings first, with evidence, the judge's view and a one-step fix", async () => {
    const advice: AdviceReportView = {
      generated_at_ms: Date.now(),
      window_hours: 24,
      judge: { model: "jeffan-31b-1710" },
      snoozed: [{ key: "repeating:x", title: "lab-key keeps sending requests m refuses", until_ms: Date.now() + 86_400_000, by: "a@b", at_ms: Date.now() }],
      findings: [
        {
          key: "images_refused", check: "images_refused", severity: "act", weight: 52_000,
          title: "3 text-only models refused 52,000 requests with images in the last day",
          detail: "These models can read only text.",
          evidence: [{ label: "qwen3-coder-30b-a3b-instruct", value: "19,314 refused" }, { label: "Sent by", value: "aewaisha-AI_Key (rcusers), 52,000" }],
          models: ["qwen3-coder-30b-a3b-instruct", "qwen3-235b", "qwen35-397b"],
          fix: { kind: "grant_boon", boon: "vision", models: ["qwen3-coder-30b-a3b-instruct", "qwen3-235b", "qwen35-397b"] },
          logs: { status_code: 400, error_class: "client_input" },
          judgment: { model: "jeffan-31b-1710", hurting_users: 0.93, worth_acting: 0.88, fix_is_safe: 0.95 },
        },
        { key: "long_answers:glm-5-2", check: "long_answers", severity: "note", weight: 5_000, title: "glm-5-2 writes long answers: 5,000 tokens on average", detail: "Auto can send requests here.", evidence: [], models: ["glm-5-2"] },
      ],
    };
    await render(<SettingsPage data={settingsData({ advice })} />);
    const needs = host.querySelector("#needs")!;
    // The finding to act on, then the two settings findings; the note is folded away.
    expect(needs.querySelector("h2")!.textContent).toBe("Needs you · 3");
    expect(needs.textContent).toContain("19,314 refused");
    expect(needs.textContent).toContain("jeffan-31b-1710 rates it: users affected 93%, worth acting on today 88%, the fix is safe 95%.");
    expect(needs.textContent).toContain("Also noticed · 1");
    expect(needs.textContent).toContain("Set aside · 1");
    expect(needs.querySelector<HTMLAnchorElement>('a[href^="/logs"]')!.getAttribute("href")).toBe("/logs?code=400&class=client_input&window=24h");
    await act(async () => button("Turn on the vision boon for 3 models").click());
    await act(async () => button("Turn on").click());
    expect(vi.mocked(applyAdviceFixAction)).toHaveBeenCalledWith("images_refused");
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

  it("turns on staying with a model for the conversation", async () => {
    await render(<SettingsPage data={settingsData()} />);
    await act(async () => host.querySelector<HTMLInputElement>('input[name="routing.sticky"]')!.click());
    await frame();
    await act(async () => button("Save changes").click());
    expect(saved().get("sections")).toBe("routing");
    expect(saved().get("routing.sticky")).toBe("on");
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
