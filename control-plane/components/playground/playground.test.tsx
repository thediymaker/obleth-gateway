import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Playground } from "./playground";

let root: Root;
let host: HTMLDivElement;

const models = [
  { id: "1", model_name: "llama", model_type: "chat", enabled: true },
  { id: "2", model_name: "sdxl", model_type: "image", enabled: true },
];

function button(label: string): HTMLButtonElement {
  const match = [...host.querySelectorAll("button")].find(
    (b) => (b.textContent ?? "").trim() === label || b.getAttribute("aria-label") === label,
  );
  if (!match) throw new Error(`no button matching "${label}"`);
  return match as HTMLButtonElement;
}

const fieldLabels = () =>
  [...host.querySelectorAll<HTMLElement>("[aria-label]")].map((el) => el.getAttribute("aria-label"));

beforeEach(async () => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  localStorage.clear();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => ({ ok: true, status: 200, json: async () => models }) as unknown as Response),
  );
  await act(async () => {
    root.render(<Playground scope="test" />);
  });
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

describe("Playground shell", () => {
  // The app shell already renders "Playground" and already owns the navigation
  // toggle. A page-level header repeating both is what this asserts stays gone.
  it("does not repeat the page title or the panel toggle the app shell provides", () => {
    expect(host.querySelector("h1")).toBeNull();
    expect(fieldLabels().filter((l) => l === "Toggle session list")).toHaveLength(1);
  });

  // Every mode keeps its request parameters in the same place, behind the same
  // control. Router used to print its own inline instead, with no button.
  it.each([
    { mode: "Chat", shown: "System prompt", hidden: "Tenant ID" },
    { mode: "Router", shown: "Tenant ID", hidden: "System prompt" },
    { mode: "Image", shown: "Negative prompt", hidden: "System prompt" },
  ])("puts $mode parameters behind the Parameters button", async ({ mode, shown, hidden }) => {
    await act(async () => button(mode).click());
    // Closed by default in every mode: the panel is opt-in, not ambient.
    expect(fieldLabels()).not.toContain(shown);

    await act(async () => button("Parameters").click());
    expect(fieldLabels()).toContain(shown);
    expect(fieldLabels()).not.toContain(hidden);

    await act(async () => button("Parameters").click());
    expect(fieldLabels()).not.toContain(shown);
  });

  it("keeps the panel open across a mode switch, swapping its contents", async () => {
    await act(async () => button("Parameters").click());
    expect(fieldLabels()).toContain("System prompt");
    await act(async () => button("Image").click());
    expect(fieldLabels()).toContain("Negative prompt");
    expect(fieldLabels()).not.toContain("System prompt");
  });
});
