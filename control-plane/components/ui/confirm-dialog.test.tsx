// @vitest-environment jsdom
import { act, useState, useTransition } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { useConfirm } from "./confirm-dialog";

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
            const ok = await confirm({ title: "Delete 2 keys?", description: "Cannot be undone." });
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
