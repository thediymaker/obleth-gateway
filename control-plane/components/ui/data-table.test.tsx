// @vitest-environment jsdom
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Cell, DataRow, DataTable } from "./data-table";
import { FilterBar } from "./filter-bar";
import { Tabs } from "./tabs";

const push = vi.fn();
vi.mock("next/navigation", () => ({ useRouter: () => ({ push }) }));

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  push.mockClear();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

const render = (ui: React.ReactNode) => act(async () => root.render(ui));
const key = (el: Element, k: string) => act(async () => { el.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true })); });

it("shows its empty row when there are no rows", async () => {
  await render(<DataTable label="Models" columns={[{ label: "Model" }, { label: "Type" }]} empty="No models yet.">{[]}</DataTable>);
  const cell = host.querySelector("tbody td")!;
  expect(cell.textContent).toBe("No models yet.");
  expect(cell.getAttribute("colspan")).toBe("2");
  expect(host.querySelectorAll("thead th")).toHaveLength(2);
});

it("opens a row's page on a click or Enter, but not from its own controls", async () => {
  const toggled = vi.fn();
  await render(
    <DataTable label="Models" columns={[{ label: "" }, { label: "Model" }]}>
      <DataRow href="/models/a" label="Open a">
        <Cell><input type="checkbox" aria-label="Select a" onChange={toggled} /></Cell>
        <Cell>a</Cell>
      </DataRow>
    </DataTable>,
  );
  const row = host.querySelector<HTMLTableRowElement>("tbody tr")!;
  expect(row.tabIndex).toBe(0);
  expect(row.getAttribute("aria-label")).toBe("Open a");

  await act(async () => host.querySelector<HTMLInputElement>('[aria-label="Select a"]')!.click());
  expect(toggled).toHaveBeenCalled();
  expect(push).not.toHaveBeenCalled();

  await act(async () => (row.cells[1] as HTMLElement).click());
  expect(push).toHaveBeenCalledWith("/models/a");

  push.mockClear();
  row.focus();
  await key(row, "Enter");
  expect(push).toHaveBeenCalledWith("/models/a");
});

it("leaves a row with nowhere to go out of the tab order", async () => {
  await render(<DataTable label="Endpoints" columns={[{ label: "Name" }]}><DataRow><Cell>a</Cell></DataRow></DataTable>);
  expect(host.querySelector("tbody tr")!.hasAttribute("tabindex")).toBe(false);
});

function Filtered() {
  const [q, setQ] = useState("");
  return (
    <>
      <input aria-label="Other field" />
      <FilterBar search={{ value: q, onChange: setQ, label: "Search models" }} />
    </>
  );
}

it("focuses its search on '/', not while typing elsewhere, and Escape clears it", async () => {
  await render(<Filtered />);
  const search = host.querySelector<HTMLInputElement>('[aria-label="Search models"]')!;
  const other = host.querySelector<HTMLInputElement>('[aria-label="Other field"]')!;

  other.focus();
  await key(other, "/");
  expect(document.activeElement).toBe(other);

  other.blur();
  await key(document.body, "/");
  expect(document.activeElement).toBe(search);

  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(search, "llama");
    search.dispatchEvent(new Event("input", { bubbles: true }));
  });
  expect(search.value).toBe("llama");
  await key(search, "Escape");
  expect(search.value).toBe("");
});

it("moves between tabs with the arrow keys and keeps one tab stop", async () => {
  function Strip() {
    const [tab, setTab] = useState<"a" | "b" | "c">("a");
    return <Tabs label="Views" value={tab} onChange={setTab} tabs={[{ value: "a", label: "A", count: 2 }, { value: "b", label: "B" }, { value: "c", label: "C" }]} />;
  }
  await render(<Strip />);
  const tabs = () => [...host.querySelectorAll<HTMLButtonElement>("[role=tab]")];
  expect(tabs().map((t) => t.textContent)).toEqual(["A · 2", "B", "C"]);
  expect(tabs().map((t) => t.tabIndex)).toEqual([0, -1, -1]);
  tabs()[0].focus();
  await key(tabs()[0], "ArrowLeft");
  expect(tabs()[2].getAttribute("aria-selected")).toBe("true");
  expect(document.activeElement).toBe(tabs()[2]);
  await key(tabs()[2], "Home");
  expect(tabs()[0].getAttribute("aria-selected")).toBe("true");
});
