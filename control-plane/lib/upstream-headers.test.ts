import { describe, it, expect } from "vitest";
import { parseUpstreamHeaders, upstreamHeadersText } from "./upstream-headers";

describe("parseUpstreamHeaders", () => {
  it("reads one `Name: value` per line and skips blank lines", () => {
    expect(parseUpstreamHeaders("x-routing-hint: sticky\n\nX-Team:  ops \n")).toEqual({
      "x-routing-hint": "sticky",
      "X-Team": "ops",
    });
  });

  it("keeps everything after the first colon as the value", () => {
    expect(parseUpstreamHeaders("x-target: http://svc:8000")).toEqual({
      "x-target": "http://svc:8000",
    });
  });

  it("sends a name without a value as null, meaning keep the stored value", () => {
    expect(parseUpstreamHeaders("x-upstream-token:\nx-routing-hint")).toEqual({
      "x-upstream-token": null,
      "x-routing-hint": null,
    });
  });

  it("returns an empty object for an emptied field, which clears the headers", () => {
    expect(parseUpstreamHeaders("  \n")).toEqual({});
  });
});

describe("upstreamHeadersText", () => {
  it("lists stored names with blank values, which round-trip as keep", () => {
    const text = upstreamHeadersText(["x-routing-hint", "x-upstream-token"]);
    expect(text).toBe("x-routing-hint:\nx-upstream-token:");
    expect(parseUpstreamHeaders(text)).toEqual({
      "x-routing-hint": null,
      "x-upstream-token": null,
    });
    expect(upstreamHeadersText(undefined)).toBe("");
  });
});
