import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { AutoRouterSettingsForm } from "./settings-form";
import { setAutoRouterSettingsAction } from "@/app/actions";
import type { AutoRouterSettingsView, ModelRoute } from "@/lib/obleth";

vi.mock("@/app/actions", () => ({ setAutoRouterSettingsAction: vi.fn() }));

let root: Root;
let host: HTMLDivElement;

const models = [
  { id: "1", model_name: "classifier-mini" },
  { id: "2", model_name: "auto" },
] as unknown as ModelRoute[];

const settings: AutoRouterSettingsView = {
  classifier_enabled: true,
  classifier_model: "classifier-mini",
  classifier_timeout_ms: 250,
  available_tags: [],
  capacity_weight: 0.7,
  cost_weight: 0.2,
  tag_weight: 0.3,
  default_soft_cap: 12,
  temperature: 0.4,
  difficulty_enabled: false,
  tier_source: "derived",
};

function range(id: string) {
  return host.querySelector<HTMLInputElement>(`input[type="range"]#${id}`)!;
}

// React tracks the DOM value it last wrote so a plain `el.value = x` (bypassing
// React's own setter) doesn't register as a change and no onChange fires. Go
// through the native prototype setter directly, same trick @testing-library's
// fireEvent uses internally, to make the change visible to React.
function setNativeValue(el: HTMLInputElement | HTMLSelectElement, value: string, eventType: "input" | "change") {
  const descriptor = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), "value")!;
  descriptor.set!.call(el, value);
  el.dispatchEvent(new Event(eventType, { bubbles: true }));
}

/** The dropdown trigger; its text is the selected option's label. */
function selectTrigger(id: string) {
  return host.querySelector<HTMLButtonElement>(`button#${id}`)!;
}

// The dropdown opens on pointerdown and Radix ignores a plain Event there (it
// reads `button`/`ctrlKey`), so send a MouseEvent. Options render in a portal
// on document.body, outside `host`.
async function chooseOption(id: string, label: string) {
  await act(async () => {
    selectTrigger(id).dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, button: 0 }));
  });
  const option = [...document.querySelectorAll<HTMLElement>("[role='menuitem']")].find(
    (el) => el.textContent === label,
  )!;
  await act(async () => {
    option.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

async function render(view: AutoRouterSettingsView | null) {
  // useState initializers only run on mount, so a settings-prop change on an
  // already-mounted form (as the real page never does) wouldn't reset the
  // fields. Each render() call here gets a fresh root, matching how the page
  // actually uses this form: mounted once per server-provided settings value.
  await act(async () => root.unmount());
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await act(async () => {
    root.render(<AutoRouterSettingsForm settings={view} models={models} />);
  });
}

async function save() {
  const button = [...host.querySelectorAll("button")].find((b) =>
    b.textContent?.includes("Save auto routing"),
  )!;
  await act(async () => {
    button.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

// The scoring controls live under the collapsed Advanced disclosure.
async function openAdvanced() {
  const button = [...host.querySelectorAll("button")].find((b) =>
    b.textContent?.startsWith("Advanced —"),
  )!;
  await act(async () => {
    button.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
}

function profileCard(label: string) {
  // The label span may also carry the "recommended" badge's text.
  return [...host.querySelectorAll<HTMLButtonElement>("button[aria-pressed]")].find(
    (b) => b.querySelector("span")?.textContent?.replace(/recommended$/, "") === label,
  )!;
}

beforeEach(async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.mocked(setAutoRouterSettingsAction).mockReset();
  vi.mocked(setAutoRouterSettingsAction).mockResolvedValue({ ok: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  await render(settings);
});

afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

it("reflects incoming settings in the scoring controls and their readouts", async () => {
  await openAdvanced();
  expect(range("capacity_weight").value).toBe("0.7");
  expect(range("cost_weight").value).toBe("0.2");
  expect(range("tag_weight").value).toBe("0.3");
  expect(range("temperature").value).toBe("0.4");
  expect(host.querySelector<HTMLInputElement>("#default_soft_cap")!.value).toBe("12");
  expect(selectTrigger("tier_source").textContent).toBe("Derived from cost");
  expect(host.querySelector('[role="switch"]')).not.toBeNull();

  // Numeric readouts beside each slider.
  expect(host.textContent).toContain("0.70");
  expect(host.textContent).toContain("0.20");
  expect(host.textContent).toContain("0.30");
  expect(host.textContent).toContain("0.4");
});

it("shows Deterministic at temperature 0 instead of a bare number", async () => {
  await render({ ...settings, temperature: 0 });
  await openAdvanced();
  expect(host.textContent).toContain("Deterministic");
});

it("submits changed scoring values, the soft cap, tiering toggle, and tier source", async () => {
  await openAdvanced();
  await act(async () => {
    setNativeValue(range("capacity_weight"), "0.5", "input");
  });
  await act(async () => {
    setNativeValue(host.querySelector<HTMLInputElement>("#default_soft_cap")!, "20", "input");
  });
  await act(async () => {
    const toggle = host.querySelector<HTMLButtonElement>(
      '[role="switch"][aria-label="Difficulty tiering"]',
    )!;
    toggle.dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await chooseOption("tier_source", "Declared only");

  await save();

  expect(setAutoRouterSettingsAction).toHaveBeenCalledTimes(1);
  const body = vi.mocked(setAutoRouterSettingsAction).mock.calls[0][0];
  expect(body.capacity_weight).toBe(0.5);
  expect(body.cost_weight).toBe(0.2);
  expect(body.tag_weight).toBe(0.3);
  expect(body.default_soft_cap).toBe(20);
  expect(body.temperature).toBe(0.4);
  expect(body.difficulty_enabled).toBe(true);
  expect(body.tier_source).toBe("declared");
});

it("falls back to the backend defaults when no settings are saved yet", async () => {
  await render(null);
  await openAdvanced();
  expect(range("capacity_weight").value).toBe("0.6");
  expect(range("cost_weight").value).toBe("0.4");
  expect(range("tag_weight").value).toBe("0.5");
  expect(range("temperature").value).toBe("0");
  expect(host.querySelector<HTMLInputElement>("#default_soft_cap")!.value).toBe("8");
  expect(selectTrigger("tier_source").textContent).toBe("Hybrid");
});

it("recognizes preset values, applies a picked profile, and flips to custom on a hand edit", async () => {
  // The beforeEach settings (0.7/0.2/0.3, cap 12, derived) match no preset.
  expect(profileCard("Custom").getAttribute("aria-pressed")).toBe("true");

  // The backend defaults are exactly the Balanced preset.
  await render(null);
  expect(profileCard("Balanced").getAttribute("aria-pressed")).toBe("true");

  // Picking a profile rewrites every scoring value it owns.
  await act(async () => {
    profileCard("Best answer").dispatchEvent(new MouseEvent("click", { bubbles: true }));
  });
  await save();
  const body = vi.mocked(setAutoRouterSettingsAction).mock.calls[0][0];
  expect(body.tag_weight).toBe(0.9);
  expect(body.cost_weight).toBe(0.05);
  expect(body.difficulty_enabled).toBe(true);

  // A hand edit under Advanced no longer matches the preset.
  await openAdvanced();
  await act(async () => {
    setNativeValue(range("tag_weight"), "0.8", "input");
  });
  expect(profileCard("Best answer").getAttribute("aria-pressed")).toBe("false");
  expect(profileCard("Custom").getAttribute("aria-pressed")).toBe("true");
});
