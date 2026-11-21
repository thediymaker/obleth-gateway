// @vitest-environment jsdom
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SettingsForm, type SaveResult } from "./settings-form";
import type { FormSnapshot } from "@/lib/models-model";

let root: Root;
let host: HTMLDivElement;
const frame = () => act(async () => { await new Promise((r) => requestAnimationFrame(() => r(null))); });
const bar = () => host.querySelector('[aria-label="Unsaved changes"]');
const button = (label: string) => [...host.querySelectorAll("button")].find((b) => b.textContent === label)!;
const field = (name: string) => host.querySelector<HTMLInputElement>(`[name="${name}"]`)!;
async function type(name: string, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(field(name), value);
    field(name).dispatchEvent(new Event("input", { bubbles: true }));
  });
  await frame();
}

const sectionOf = (f: string) => (["id", "sections", "loaded"].includes(f) ? null : f === "secret" ? "auth" : "main");
const labelOf = (f: string) => f.toUpperCase();

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

async function show(props: Partial<Parameters<typeof SettingsForm>[0]>, children: React.ReactNode) {
  await act(async () => root.render(<SettingsForm id="x" sectionOf={sectionOf} labelOf={labelOf} save={async () => ({ ok: true })} {...props}>{children}</SettingsForm>));
  await frame();
}

it("names the changes in the order the page shows them, and counts a cleared checkbox", async () => {
  await show({}, <><input name="zeta" defaultValue="1" /><input name="alpha" defaultValue="1" /><input type="checkbox" name="on" defaultChecked /></>);
  await type("alpha", "2");
  await type("zeta", "2");
  await act(async () => field("on").click());
  await frame();
  expect(bar()!.textContent).toContain("3 unsaved");
  expect(bar()!.textContent).toContain("ZETA · ALPHA · ON");
});

it("lets fields that arrive late join the baseline instead of reading as edits", async () => {
  function Late() {
    const [here, setHere] = useState(false);
    return <>{here ? <><input type="hidden" name="loaded" value="1" /><input name="picked" defaultValue="a" /></> : <button type="button" onClick={() => setHere(true)}>Load</button>}<input name="name" defaultValue="n" /></>;
  }
  const adopt = (base: FormSnapshot, now: FormSnapshot) => (base.has("loaded") || !now.has("loaded") ? base : new Map([...base, ["loaded", "1"], ["picked", now.get("picked")!]]));
  await show({ adopt }, <Late />);
  await act(async () => button("Load").click());
  await frame();
  expect(bar()).toBeNull();
  await type("picked", "b");
  expect(bar()!.textContent).toContain("PICKED");
});

it("blanks the fields it is told to once their section saves", async () => {
  const save = vi.fn(async (): Promise<SaveResult> => ({ ok: true }));
  await show({ save, clearOnSave: (saved) => (saved.includes("auth") ? ["secret"] : []) }, <><input name="secret" defaultValue="" /><input name="name" defaultValue="n" /></>);
  await type("secret", "s3cret");
  await act(async () => button("Save changes").click());
  await frame();
  expect((save.mock.calls[0] as unknown as [FormData])[0].get("secret")).toBe("s3cret");
  expect(field("secret").value).toBe("");
  expect(bar()!.textContent).not.toContain("unsaved");
});

it("shows a save's warnings and keeps them until dismissed", async () => {
  await show({ save: async () => ({ ok: true, warnings: ["The server did not list it."] }) }, <input name="name" defaultValue="n" />);
  await type("name", "m");
  await act(async () => button("Save changes").click());
  await frame();
  expect(bar()!.textContent).toContain("Saved.");
  expect(bar()!.textContent).toContain("The server did not list it.");
  await act(async () => { await new Promise((r) => setTimeout(r, 2600)); });
  expect(bar()).not.toBeNull();
  await act(async () => button("Dismiss").click());
  expect(bar()).toBeNull();
});
