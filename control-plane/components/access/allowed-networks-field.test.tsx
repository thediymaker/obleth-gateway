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

describe("allowed networks", () => {
  it("adds every range of a named network not yet listed, and won't add one twice", async () => {
    await act(async () => root.render(<AllowedNetworksField defaultValue="203.0.113.0/24" named={named} />));
    const area = host.querySelector("textarea")!;
    const chip = (name: string) => [...host.querySelectorAll("button")].find((b) => b.textContent === name)!;
    expect(chip("Campus").disabled).toBe(false);
    await act(async () => chip("Campus").click());
    expect(area.value).toBe("203.0.113.0/24\n10.0.0.0/8\n2001:db8::/48");
    expect(chip("Campus").disabled).toBe(true);
    expect(chip("Cluster pods").title).toBe("10.42.0.0/16");
  });

  it("offers our own networks first", async () => {
    await act(async () => root.render(<AllowedNetworksField named={named} />));
    expect([...host.querySelectorAll("button")].map((b) => b.textContent)).toEqual(["Campus", "Cluster pods", "Partner lab"]);
  });

  it("offers nothing when no network is named", async () => {
    await act(async () => root.render(<AllowedNetworksField />));
    expect(host.querySelector('[aria-label="Add a named network"]')).toBeNull();
    expect(host.querySelector("textarea")!.getAttribute("name")).toBe("allowed_networks");
  });
});
