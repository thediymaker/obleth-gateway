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

  it("opens a new session on the launcher, and a mode card leaves it", async () => {
    expect(host.textContent).toContain("What do you want to try?");
    expect(fieldLabels()).not.toContain("System prompt");
    const card = [...host.querySelectorAll("button")].find((b) => b.textContent?.startsWith("Image"))!;
    await act(async () => card.click());
    expect(host.textContent).not.toContain("What do you want to try?");
    expect(fieldLabels()).toContain("Negative prompt");
  });

  // Every mode but Verdicts keeps its request parameters in the same place: a
  // settings panel beside the work, open by default on a wide screen.
  it.each([
    { mode: "Chat", shown: "System prompt", hidden: "Tenant ID" },
    { mode: "Router", shown: "Tenant ID", hidden: "System prompt" },
    { mode: "Image", shown: "Negative prompt", hidden: "System prompt" },
  ])("shows $mode parameters in the settings panel, which one button hides", async ({ mode, shown, hidden }) => {
    await act(async () => button(mode).click());
    expect(fieldLabels()).toContain(shown);
    expect(fieldLabels()).not.toContain(hidden);

    await act(async () => button("Hide settings").click());
    expect(fieldLabels()).not.toContain(shown);

    await act(async () => button("Show settings").click());
    expect(fieldLabels()).toContain(shown);
  });

  it("keeps the panel's visibility across a mode switch, swapping its contents", async () => {
    await act(async () => button("Chat").click());
    expect(fieldLabels()).toContain("System prompt");
    await act(async () => button("Image").click());
    expect(fieldLabels()).toContain("Negative prompt");
    expect(fieldLabels()).not.toContain("System prompt");
    await act(async () => button("Hide settings").click());
    await act(async () => button("Router").click());
    expect(fieldLabels()).not.toContain("Tenant ID");
  });

  it("has no settings panel in Verdicts, whose three columns already hold everything", async () => {
    await act(async () => button("Verdicts").click());
    expect(fieldLabels()).not.toContain("Hide settings");
    expect(fieldLabels()).toContain("State");
  });

  it("imports an exported session under a fresh id, conversations included", async () => {
    const exported = {
      session: { id: "old", title: "Imported chat", mode: "compare", models: ["llama"], generation: { systemPrompt: "" } },
      conversations: { "compare:0": { messages: [], activeTarget: null, targetStart: null }, "image": [{ url: "x" }] },
    };
    const input = host.querySelector<HTMLInputElement>("input[type='file'][accept*='json']")!;
    const file = new File([JSON.stringify(exported)], "playground-session.json", { type: "application/json" });
    Object.defineProperty(input, "files", { value: [file] });
    await act(async () => { input.dispatchEvent(new Event("change", { bubbles: true })); await new Promise((r) => setTimeout(r, 0)); });

    const saved = JSON.parse(localStorage.getItem("obleth-playground:test") ?? "[]") as { id: string; title: string }[];
    const imported = saved.find((s) => s.title === "Imported chat")!;
    expect(imported.id).not.toBe("old");
    expect(localStorage.getItem(`obleth-playground:test:${imported.id}:compare:0`)).not.toBeNull();
    // Only conversation lanes come back; nothing else is written.
    expect(localStorage.getItem(`obleth-playground:test:${imported.id}:image`)).toBeNull();
  });
});
