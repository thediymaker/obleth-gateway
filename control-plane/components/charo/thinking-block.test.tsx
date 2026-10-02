import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ThinkingBlock, thoughtFor } from "./thinking-block";

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(() => { act(() => root.unmount()); host.remove(); });

const render = (props: Parameters<typeof ThinkingBlock>[0]) => act(() => root.render(<ThinkingBlock {...props} />));
const body = () => host.querySelector("[aria-label='Model thinking']");
const toggle = () => host.querySelector<HTMLButtonElement>("button[aria-expanded]")!;

describe("ThinkingBlock", () => {
  it("is open and following along while the model thinks", () => {
    render({ text: "Check 17 and 23.", active: true });
    expect(host.textContent).toContain("Thinking…");
    expect(host.textContent).toContain("4 words");
    expect(body()?.textContent).toBe("Check 17 and 23.");
  });

  it("folds to one line with the time once the answer starts, and opens on a click", () => {
    render({ text: "Check 17 and 23.", active: false, ms: 4200 });
    expect(host.textContent).toContain("Thought for 4.2 s");
    expect(body()).toBeNull();
    act(() => toggle().click());
    expect(body()?.textContent).toBe("Check 17 and 23.");
    expect(toggle().getAttribute("aria-expanded")).toBe("true");
  });

  it("stays as the person left it when thinking ends", () => {
    render({ text: "Thinking…", active: true });
    act(() => toggle().click());
    expect(body()).toBeNull();
    render({ text: "Thinking… done", active: false, ms: 900 });
    expect(body()).toBeNull();
  });
});

describe("thoughtFor", () => {
  it("reads the way people say durations", () => {
    expect(thoughtFor(4200)).toBe("4.2 s");
    expect(thoughtFor(31_400)).toBe("31 s");
    expect(thoughtFor(72_000)).toBe("1 min 12 s");
  });
});
