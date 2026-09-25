import { describe, it, expect } from "vitest";
import { parseUpstreamHeaders, upstreamHeadersText } from "./upstream-headers";

describe("parseUpstreamHeaders", () => {
  it("reads one `Name: value` per line and skips blank lines", () => {
    expect(parseUpstreamHeaders("routing-strategy: prefix-cache\n\nX-Team:  ops \n")).toEqual({
      "routing-strategy": "prefix-cache",
      "X-Team": "ops",
    });
  });

  it("keeps everything after the first colon as the value", () => {
    expect(parseUpstreamHeaders("x-target: http://svc:8000")).toEqual({
      "x-target": "http://svc:8000",
    });
  });

  it("sends a name without a value as null, meaning keep the stored value", () => {
    expect(parseUpstreamHeaders("x-upstream-token:\nrouting-strategy")).toEqual({
      "x-upstream-token": null,
      "routing-strategy": null,
    });
  });

  it("returns an empty object for an emptied field, which clears the headers", () => {
    expect(parseUpstreamHeaders("  \n")).toEqual({});
  });
});

describe("upstreamHeadersText", () => {
  it("lists stored names with blank values, which round-trip as keep", () => {
    const text = upstreamHeadersText(["routing-strategy", "x-upstream-token"]);
    expect(text).toBe("routing-strategy:\nx-upstream-token:");
    expect(parseUpstreamHeaders(text)).toEqual({
      "routing-strategy": null,
      "x-upstream-token": null,
    });
    expect(upstreamHeadersText(undefined)).toBe("");
  });
});
