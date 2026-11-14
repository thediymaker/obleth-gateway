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
  { network: "10.0.0.0/8", name: "Campus" },
  { network: "10.42.0.0/16", name: "Cluster pods" },
];

describe("allowed networks", () => {
  it("adds a named network by its name, and won't add one twice", async () => {
    await act(async () => root.render(<AllowedNetworksField defaultValue="203.0.113.0/24" named={named} />));
    const area = host.querySelector("textarea")!;
    const chip = (name: string) => [...host.querySelectorAll("button")].find((b) => b.textContent === name)!;
    expect(chip("Campus").disabled).toBe(false);
    await act(async () => chip("Campus").click());
    expect(area.value).toBe("203.0.113.0/24\n10.0.0.0/8");
    expect(chip("Campus").disabled).toBe(true);
    expect(chip("Cluster pods").title).toBe("10.42.0.0/16");
  });

  it("offers nothing when no network is named", async () => {
    await act(async () => root.render(<AllowedNetworksField />));
    expect(host.querySelector('[aria-label="Add a named network"]')).toBeNull();
    expect(host.querySelector("textarea")!.getAttribute("name")).toBe("allowed_networks");
  });
});
