import { describe, expect, it } from "vitest";
import { linkTime, logsHref } from "./log-links";

describe("links to the request log", () => {
  it("carries only the filters given", () => {
    expect(logsHref({})).toBe("/logs");
    expect(logsHref({ status: "error", model: "glm-5-3", team: "", window: "24h" })).toBe("/logs?status=error&model=glm-5-3&window=24h");
    expect(logsHref({ code: 502, since: 100, until: 200 })).toBe("/logs?code=502&since=100&until=200");
  });

  it("reads a day as its local midnight, and a day's end as the moment before the next", () => {
    const start = linkTime("2026-09-21", false)!;
    const end = linkTime("2026-09-21", true)!;
    expect(new Date(start).getHours()).toBe(0);
    expect(new Date(start).getDate()).toBe(21);
    expect(end - start).toBe(86_400_000 - 1);
    expect(linkTime("1790000000000", false)).toBe(1790000000000);
    expect(linkTime("yesterday", false)).toBeUndefined();
    expect(linkTime(undefined, true)).toBeUndefined();
  });
});
