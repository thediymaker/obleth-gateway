import { describe, expect, it } from "vitest";
import { numParam, strParam, utcDay } from "./log-params";

const sp = (q: string) => new URLSearchParams(q);

describe("query parameters", () => {
  it("never hand on NaN: a missing, blank or non-numeric number is the fallback", () => {
    expect(numParam(sp("limit=50"), "limit")).toBe(50);
    expect(numParam(sp("limit=abc"), "limit")).toBeUndefined();
    expect(numParam(sp("limit="), "limit", 7)).toBe(7);
    expect(numParam(sp(""), "limit", 7)).toBe(7);
    expect(numParam(sp("limit=0"), "limit", 7)).toBe(0);
  });

  it("trim text and treat blank as absent", () => {
    expect(strParam(sp("model=%20m1%20"), "model")).toBe("m1");
    expect(strParam(sp("model=%20"), "model")).toBeUndefined();
  });

  it("name UTC days", () => {
    const now = Date.parse("2026-11-19T23:30:00Z");
    expect(utcDay(0, now)).toBe("2026-11-19");
    expect(utcDay(7, now)).toBe("2026-11-12");
  });
});
