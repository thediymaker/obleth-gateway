import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Sheet } from "./ui";

let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

const frame = () => act(() => new Promise<void>((r) => setTimeout(r, 30)));

function render(onClose: () => void) {
  act(() =>
    root.render(
      <Sheet open onClose={onClose} title="Panel">
        <input aria-label="first" />
        <input aria-label="second" />
      </Sheet>,
    ),
  );
}

describe("Sheet", () => {
  it("keeps focus where someone is typing when the parent re-renders with a new onClose", async () => {
    render(() => {});
    await frame();
    const second = host.querySelector<HTMLInputElement>('input[aria-label="second"]')!;
    second.focus();
    // A parent that refetches passes a fresh callback on every render.
    render(() => {});
    await frame();
    render(() => {});
    await frame();
    expect(document.activeElement).toBe(second);
  });

  it("calls the latest onClose on Escape", async () => {
    const first = vi.fn();
    const latest = vi.fn();
    render(first);
    render(latest);
    await frame();
    act(() => { document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" })); });
    expect(latest).toHaveBeenCalledTimes(1);
    expect(first).not.toHaveBeenCalled();
  });
});
