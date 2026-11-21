// @vitest-environment jsdom
import { act, useState, useTransition } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { ConfirmDialog, useConfirm } from "./confirm-dialog";

// The shape every destructive action in the dashboard uses: ask inside a
// transition, then do the work. The dialog's open state used to be set inside
// the transition, and React holds a transition's updates until its action
// finishes -- which here waits on the dialog -- so the dialog never showed and
// the action hung with its buttons disabled (bulk key delete, 2026-09-30).
function AskInsideTransition() {
  const { confirm, confirmElement } = useConfirm();
  const [pending, start] = useTransition();
  const [result, setResult] = useState("none");
  return (
    <>
      <button
        type="button"
        data-testid="ask"
        disabled={pending}
        onClick={() =>
          start(async () => {
            const ok = await confirm({ tone: "destructive", title: "Delete 2 keys?", description: "Cannot be undone.", confirmLabel: "Delete" });
            setResult(ok ? "confirmed" : "cancelled");
          })
        }
      >
        Delete…
      </button>
      <output data-testid="result">{result}</output>
      {confirmElement}
    </>
  );
}

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

const dialog = () => document.querySelector('[role="dialog"]');
const button = (label: string) =>
  [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === label);

async function ask() {
  act(() => root.render(<AskInsideTransition />));
  await act(async () => {
    button("Delete…")!.click();
  });
}

describe("useConfirm", () => {
  it("opens, and resolves true, when asked from inside a transition", async () => {
    await ask();
    expect(dialog()).not.toBeNull();

    await act(async () => {
      button("Delete")!.click();
    });
    expect(document.querySelector('[data-testid="result"]')?.textContent).toBe("confirmed");
    expect(dialog()).toBeNull();
    expect((document.querySelector('[data-testid="ask"]') as HTMLButtonElement).disabled).toBe(false);
  });

  it("resolves false on Cancel", async () => {
    await ask();
    expect(dialog()).not.toBeNull();
    await act(async () => {
      button("Cancel")!.click();
    });
    expect(document.querySelector('[data-testid="result"]')?.textContent).toBe("cancelled");
  });
});

describe("ConfirmDialog tone", () => {
  function show(tone?: "default" | "destructive") {
    act(() => root.render(<ConfirmDialog open onOpenChange={() => {}} onConfirm={() => {}} title="Replace alice?" description="Makes a new key." confirmLabel="Go" tone={tone} />));
    const title = document.querySelector("h2")!;
    return { title, go: button("Go")! };
  }

  it("is neutral unless asked to be destructive", () => {
    const { title, go } = show();
    expect(title.className).not.toContain("text-destructive");
    expect(title.querySelector("svg")).toBeNull();
    expect(go.className).toContain("bg-primary");
    expect(go.className).not.toContain("bg-destructive");
  });

  it("marks a destructive question in red, title and button", () => {
    const { title, go } = show("destructive");
    expect(title.className).toContain("text-destructive");
    expect(title.querySelector("svg")).not.toBeNull();
    expect(go.className).toContain("bg-destructive");
  });
});
