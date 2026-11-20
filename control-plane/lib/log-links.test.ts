import { describe, expect, it } from "vitest";
import { linkTime, logsHref } from "./log-links";

describe("links to the request log", () => {
  it("carries only the filters given", () => {
    expect(logsHref({})).toBe("/logs");
    expect(logsHref({ status: "error", model: "glm-5-3", team: "", window: "24h" })).toBe("/logs?status=error&model=glm-5-3&window=24h");
    expect(logsHref({ code: 502, since: 100, until: 200 })).toBe("/logs?code=502&since=100&until=200");
    expect(logsHref({ requestId: "7c1e40a2" })).toBe("/logs?requestId=7c1e40a2");
    expect(logsHref({ fault: "platform", window: "1h" })).toBe("/logs?fault=platform&window=1h");
    expect(logsHref({ key: "k-1", code: 400, class: "client_input" })).toBe("/logs?key=k-1&code=400&class=client_input");
  });

  it("reads a day as its UTC day, 00:00Z to the moment before the next, wherever the reader is", () => {
    const zone = process.env.TZ;
    try {
      for (const tz of ["America/Phoenix", "Asia/Tokyo", "UTC"]) {
        process.env.TZ = tz;
        expect(new Date(linkTime("2026-09-21", false)!).toISOString()).toBe("2026-09-21T00:00:00.000Z");
        expect(new Date(linkTime("2026-09-21", true)!).toISOString()).toBe("2026-09-21T23:59:59.999Z");
      }
    } finally {
      process.env.TZ = zone;
    }
    expect(linkTime("1790000000000", false)).toBe(1790000000000);
    expect(linkTime("yesterday", false)).toBeUndefined();
    expect(linkTime(undefined, true)).toBeUndefined();
  });
});
