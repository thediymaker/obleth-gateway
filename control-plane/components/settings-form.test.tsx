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

it("reflects incoming settings in the scoring controls and their readouts", () => {
  expect(range("capacity_weight").value).toBe("0.7");
  expect(range("cost_weight").value).toBe("0.2");
  expect(range("tag_weight").value).toBe("0.3");
  expect(range("temperature").value).toBe("0.4");
  expect(host.querySelector<HTMLInputElement>("#default_soft_cap")!.value).toBe("12");
  expect(selectTrigger("tier_source").textContent).toBe("Derived from cost");
  expect(host.querySelector<HTMLInputElement>('input[type="checkbox"]')).not.toBeNull();

  // Numeric readouts beside each slider.
  expect(host.textContent).toContain("0.70");
  expect(host.textContent).toContain("0.20");
  expect(host.textContent).toContain("0.30");
  expect(host.textContent).toContain("0.4");
});

it("shows Deterministic at temperature 0 instead of a bare number", async () => {
  await render({ ...settings, temperature: 0 });
  expect(host.textContent).toContain("Deterministic");
});

it("submits changed scoring values, the soft cap, tiering toggle, and tier source", async () => {
  await act(async () => {
    setNativeValue(range("capacity_weight"), "0.5", "input");
  });
  await act(async () => {
    setNativeValue(host.querySelector<HTMLInputElement>("#default_soft_cap")!, "20", "input");
  });
  await act(async () => {
    // Index 1: the classifier checkbox ("Enable intent classifier") comes first
    // in the DOM, "Difficulty tiering" is the second checkbox in the form.
    const toggle = host.querySelectorAll<HTMLInputElement>('input[type="checkbox"]')[1]!;
    toggle.click();
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
  expect(range("capacity_weight").value).toBe("0.6");
  expect(range("cost_weight").value).toBe("0.4");
  expect(range("tag_weight").value).toBe("0.5");
  expect(range("temperature").value).toBe("0");
  expect(host.querySelector<HTMLInputElement>("#default_soft_cap")!.value).toBe("8");
  expect(selectTrigger("tier_source").textContent).toBe("Hybrid");
});
