import { describe, expect, it } from "vitest";
import { sentByAnotherSite } from "./origin";

const from = (init: Record<string, string>) => new Headers(init);

describe("sentByAnotherSite", () => {
  it("lets reads through wherever they come from", () => {
    for (const method of ["GET", "HEAD", "OPTIONS", "get"]) {
      expect(
        sentByAnotherSite(method, from({ host: "dash.example", origin: "https://evil.example" })),
      ).toBe(false);
    }
  });

  it("accepts a change sent by the dashboard's own pages", () => {
    expect(
      sentByAnotherSite("POST", from({ host: "dash.example", origin: "https://dash.example" })),
    ).toBe(false);
    expect(
      sentByAnotherSite("PUT", from({ host: "dash.example:3002", origin: "http://DASH.example:3002" })),
    ).toBe(false);
    // Behind a proxy that rewrites Host, the browser's host is forwarded.
    expect(
      sentByAnotherSite(
        "POST",
        from({ host: "control-plane:3000", "x-forwarded-host": "dash.example", origin: "https://dash.example" }),
      ),
    ).toBe(false);
    expect(sentByAnotherSite("DELETE", from({ host: "dash.example", "sec-fetch-site": "same-origin" }))).toBe(false);
  });

  it("refuses a change sent by another origin, same site or not", () => {
    for (const origin of [
      "https://evil.example",
      "https://grafana.example",
      // Another port of the same host is the same site and a different origin.
      "http://dash.example:3001",
      "null",
      "not a url",
    ]) {
      expect(sentByAnotherSite("POST", from({ host: "dash.example", origin })), origin).toBe(true);
    }
    for (const site of ["same-site", "cross-site"]) {
      expect(sentByAnotherSite("POST", from({ host: "dash.example", "sec-fetch-site": site })), site).toBe(true);
    }
    // An Origin with no host to compare against is not trusted.
    expect(sentByAnotherSite("POST", from({ origin: "https://dash.example" }))).toBe(true);
  });

  it("passes a request no page sent", () => {
    expect(sentByAnotherSite("POST", from({ host: "dash.example" }))).toBe(false);
  });
});
