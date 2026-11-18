// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { AllowedNetworksField } from "./allowed-networks-field";

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

const named = [
  { name: "Partner lab", ranges: ["198.51.100.0/24"], ours: false },
  { name: "Campus", ranges: ["10.0.0.0/8", "203.0.113.0/24", "2001:db8::/48"], ours: true },
  { name: "Cluster pods", ranges: ["10.42.0.0/16"], ours: true },
];

const chip = (name: string) => [...host.querySelectorAll<HTMLButtonElement>("button[aria-pressed]")].find((b) => b.textContent === name)!;
const hidden = () => host.querySelector<HTMLInputElement>('input[name="allowed_networks"]')!.value;
const area = () => host.querySelector("textarea")!;

function type(el: HTMLTextAreaElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!;
  setter.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

describe("allowed networks", () => {
  it("presses the networks a stored list holds whole, and keeps the rest in the box", async () => {
    const stored = "10.0.0.0/8\n203.0.113.0/24\n2001:DB8::/48\n10.42.0.0/16\n192.0.2.7\n198.51.100.0/25";
    await act(async () => root.render(<AllowedNetworksField defaultValue={stored} named={named} />));
    expect(chip("Campus").getAttribute("aria-pressed")).toBe("true");
    expect(chip("Cluster pods").getAttribute("aria-pressed")).toBe("true");
    expect(chip("Partner lab").getAttribute("aria-pressed")).toBe("false");
    expect(area().value).toBe("192.0.2.7\n198.51.100.0/25");
    expect(area().getAttribute("aria-label")).toBe("Other addresses or ranges");
    expect(area().hasAttribute("name")).toBe(false);
    expect(hidden()).toBe("10.0.0.0/8\n203.0.113.0/24\n2001:db8::/48\n10.42.0.0/16\n192.0.2.7\n198.51.100.0/25");
  });

  it("adds every range of a network when pressed and removes them when unpressed", async () => {
    await act(async () => root.render(<AllowedNetworksField defaultValue={"192.0.2.7\n203.0.113.0/24"} named={named} />));
    expect(chip("Campus").getAttribute("aria-pressed")).toBe("false");
    await act(async () => chip("Campus").click());
    expect(chip("Campus").getAttribute("aria-pressed")).toBe("true");
    expect(area().value).toBe("192.0.2.7");
    expect(hidden()).toBe("10.0.0.0/8\n203.0.113.0/24\n2001:db8::/48\n192.0.2.7");
    // A narrower network stays selectable beside a broader one.
    await act(async () => chip("Cluster pods").click());
    expect(hidden()).toBe("10.0.0.0/8\n203.0.113.0/24\n2001:db8::/48\n10.42.0.0/16\n192.0.2.7");
    await act(async () => chip("Campus").click());
    expect(chip("Campus").getAttribute("aria-pressed")).toBe("false");
    expect(hidden()).toBe("10.42.0.0/16\n192.0.2.7");
  });

  it("keeps other lines as typed, and presses a network once all its ranges are typed", async () => {
    await act(async () => root.render(<AllowedNetworksField named={named} />));
    expect(hidden()).toBe("");
    await act(async () => type(area(), "192.0.2.7\n 198.51.100.0/24 "));
    expect(chip("Partner lab").getAttribute("aria-pressed")).toBe("true");
    expect(hidden()).toBe("192.0.2.7\n198.51.100.0/24");
    await act(async () => chip("Partner lab").click());
    expect(area().value).toBe("192.0.2.7");
    expect(hidden()).toBe("192.0.2.7");
  });

  it("unpresses a network whose ranges were typed on one line with commas", async () => {
    await act(async () => root.render(<AllowedNetworksField named={named} />));
    await act(async () => type(area(), "192.0.2.7, 10.0.0.0/8, 203.0.113.0/24, 2001:db8::/48"));
    expect(chip("Campus").getAttribute("aria-pressed")).toBe("true");
    await act(async () => chip("Campus").click());
    expect(chip("Campus").getAttribute("aria-pressed")).toBe("false");
    expect(area().value).toBe("192.0.2.7");
    expect(hidden()).toBe("192.0.2.7");
  });

  it("keeps a chosen network's ranges when it is renamed while the form is open", async () => {
    const sol = [{ name: "Sol", ranges: ["10.139.120.0/24", "10.139.121.0/24"], ours: true }];
    await act(async () => root.render(<AllowedNetworksField defaultValue="192.0.2.7" named={sol} />));
    await act(async () => chip("Sol").click());
    const renamed = [{ name: "Sol cluster", ranges: sol[0].ranges, ours: true }];
    await act(async () => root.render(<AllowedNetworksField defaultValue="192.0.2.7" named={renamed} />));
    expect(chip("Sol cluster").getAttribute("aria-pressed")).toBe("true");
    expect(hidden()).toBe("10.139.120.0/24\n10.139.121.0/24\n192.0.2.7");
    await act(async () => root.render(<AllowedNetworksField defaultValue="192.0.2.7" named={[]} />));
    expect(area().value).toBe("10.139.120.0/24\n10.139.121.0/24\n192.0.2.7");
    expect(hidden()).toBe("10.139.120.0/24\n10.139.121.0/24\n192.0.2.7");
  });

  it("offers our own networks first", async () => {
    await act(async () => root.render(<AllowedNetworksField named={named} />));
    expect([...host.querySelectorAll("button[aria-pressed]")].map((b) => b.textContent)).toEqual(["Campus", "Cluster pods", "Partner lab"]);
  });

  it("shows a few dozen networks and says how many more there are, always showing a chosen one", async () => {
    const many = Array.from({ length: 30 }, (_, i) => ({ name: `Net ${i}`, ranges: [`10.${i}.0.0/16`], ours: false }));
    await act(async () => root.render(<AllowedNetworksField defaultValue="10.29.0.0/16" named={many} />));
    expect(host.querySelectorAll("button[aria-pressed]").length).toBe(25);
    expect(chip("Net 29").getAttribute("aria-pressed")).toBe("true");
    const more = [...host.querySelectorAll("button")].find((b) => b.textContent === "Show 5 more")!;
    await act(async () => more.click());
    expect(host.querySelectorAll("button[aria-pressed]").length).toBe(30);
  });

  it("is a plain box when no network is named", async () => {
    await act(async () => root.render(<AllowedNetworksField defaultValue="192.0.2.7" />));
    expect(host.querySelector('[aria-label="Named networks"]')).toBeNull();
    expect(area().getAttribute("aria-label")).toBe("Allowed networks");
    expect(hidden()).toBe("192.0.2.7");
  });
});
