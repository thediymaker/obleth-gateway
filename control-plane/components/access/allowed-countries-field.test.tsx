// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { AllowedCountriesField } from "./allowed-countries-field";

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

describe("allowed countries", () => {
  it("names each code as typed and flags what isn't one", async () => {
    await act(async () => root.render(<AllowedCountriesField defaultValue="us, xx de" />));
    const input = host.querySelector("input")!;
    expect(input.getAttribute("name")).toBe("allowed_countries");
    expect(input.value).toBe("us, xx de");
    const items = [...host.querySelectorAll("li")].map((li) => li.textContent);
    expect(items).toEqual(["USUnited States", "XXNot a country code", "DEGermany"]);
  });

  it("shows nothing below an empty box", async () => {
    await act(async () => root.render(<AllowedCountriesField />));
    expect(host.querySelector('[aria-label="Countries"]')).toBeNull();
  });
});
