// @vitest-environment jsdom
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it } from "vitest";
import { Sheet } from "./sheet";
import { DestructiveConfirm } from "./destructive-confirm";

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

function Page({ field = true }: { field?: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button type="button" onClick={() => setOpen(true)}>Open</button>
      <Sheet open={open} onClose={() => setOpen(false)} title="alice" description="A key">
        {field ? <input aria-label="Name" /> : <p>Nothing to type</p>}
      </Sheet>
    </>
  );
}

const dialog = () => document.querySelector<HTMLElement>("[role=dialog]");
const escape = () => act(async () => { document.activeElement!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })); });

it("is a named dialog that opens on its first field, closes on Escape, and gives focus back", async () => {
  await act(async () => root.render(<Page />));
  const opener = host.querySelector("button")!;
  opener.focus();
  await act(async () => opener.click());
  expect(dialog()?.getAttribute("aria-label")).toBe("alice");
  expect(document.activeElement?.getAttribute("aria-label")).toBe("Name");

  await escape();
  await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
  expect(dialog()).toBeNull();
  expect(document.activeElement).toBe(opener);
});

it("keeps focus inside when it has no field", async () => {
  await act(async () => root.render(<Page field={false} />));
  await act(async () => host.querySelector("button")!.click());
  expect(dialog()!.contains(document.activeElement)).toBe(true);
});

it("unlocks a typed confirmation only once the name is typed, and Cancel does nothing", async () => {
  const confirmed: string[] = [];
  let open = true;
  const render = () => root.render(
    <DestructiveConfirm open={open} onOpenChange={(o) => { open = o; render(); }} title="Delete team-a?" description="Its keys go with it." confirmWord="team-a" confirmLabel="Delete tenant" onConfirm={() => confirmed.push("yes")} />,
  );
  await act(async () => render());
  const go = () => [...document.querySelectorAll("button")].find((b) => b.textContent === "Delete tenant")!;
  expect(document.querySelector("input[type=checkbox]")).toBeNull();
  expect(go().disabled).toBe(true);

  const input = document.querySelector<HTMLInputElement>("input")!;
  const type = (v: string) => act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, v);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
  await type("team");
  expect(go().disabled).toBe(true);
  await type("team-a");
  expect(go().disabled).toBe(false);

  await act(async () => [...document.querySelectorAll("button")].find((b) => b.textContent === "Cancel")!.click());
  expect(confirmed).toEqual([]);
  expect(open).toBe(false);
});
