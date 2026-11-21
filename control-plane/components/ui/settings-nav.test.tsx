import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SettingsNav } from "./settings-nav";

vi.mock("next/link", () => ({ default: ({ href, children, ...rest }: { href: string; children: React.ReactNode }) => <a href={href} {...rest}>{children}</a> }));

let root: Root;
let host: HTMLDivElement;
let observed: Element[];
let notify: (entries: Partial<IntersectionObserverEntry>[]) => void;
const scrolled: string[] = [];

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  observed = [];
  scrolled.length = 0;
  vi.stubGlobal("IntersectionObserver", class {
    constructor(cb: (entries: Partial<IntersectionObserverEntry>[]) => void) { notify = cb; }
    observe(el: Element) { observed.push(el); }
    disconnect() {}
  });
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => { cb(0); return 0; });
  Element.prototype.scrollIntoView = function scrollIntoView() { scrolled.push(this.id); };
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  document.body.innerHTML = "";
  vi.unstubAllGlobals();
  history.replaceState(null, "", "/");
});

function render() {
  document.body.insertAdjacentHTML("beforeend", '<section id="alerts"></section><section id="routing"><div id="set-weights"><input id="w" /></div></section><section id="activity"></section>');
  act(() => root.render(
    <SettingsNav
      label="Sections"
      items={[{ id: "alerts", label: "Alerts", count: "on" }, { id: "routing", label: "Routing" }]}
      after={[{ id: "activity", label: "Activity" }, { id: "slurm", label: "Slurm ›", href: "/deployments" }]}
      find={[{ id: "set-weights", label: "Routing weights", words: "price cost", hint: "Routing" }]}
      findId="results"
      dirty={["routing"]}
    />,
  ));
}

const link = (name: string) => [...host.querySelectorAll("a")].find((a) => a.textContent?.startsWith(name))!;

describe("SettingsNav", () => {
  it("watches every in-page section and marks the one nearest the top", () => {
    render();
    expect(observed.map((e) => e.id)).toEqual(["alerts", "routing", "activity"]);
    expect(link("Alerts").getAttribute("aria-current")).toBe("true");
    const routing = document.getElementById("routing")!;
    act(() => notify([{ isIntersecting: true, target: routing, boundingClientRect: { top: 10 } as DOMRect }]));
    expect(link("Routing").getAttribute("aria-current")).toBe("true");
    expect(link("Alerts").getAttribute("aria-current")).toBeNull();
  });

  it("jumps to a section and puts it in the address; links to other pages stay links", () => {
    render();
    act(() => link("Routing").click());
    expect(scrolled).toContain("routing");
    expect(window.location.hash).toBe("#routing");
    expect(link("Slurm").getAttribute("href")).toBe("/deployments");
    expect(link("Routing").querySelector("[aria-label='unsaved changes']")).not.toBeNull();
  });

  it("finds a setting by its words, then centres it, lights it up and focuses its field", () => {
    render();
    const box = host.querySelector<HTMLInputElement>("input[aria-label='Find a setting']")!;
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
    act(() => { setter.call(box, "price"); box.dispatchEvent(new Event("input", { bubbles: true })); });
    expect(host.querySelector("[role='option']")!.textContent).toContain("Routing weights");
    act(() => box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })));
    expect(scrolled).toContain("set-weights");
    expect(document.getElementById("set-weights")!.classList.contains("setting-flash")).toBe(true);
    expect(document.activeElement?.id).toBe("w");
  });

  it("opens at the section the address names", () => {
    history.replaceState(null, "", "#activity");
    render();
    expect(scrolled).toContain("activity");
  });
});
