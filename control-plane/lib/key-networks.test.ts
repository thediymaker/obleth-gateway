import { describe, expect, it } from "vitest";
import { networkLines, networksSummary, networksText } from "./key-networks";

describe("key networks", () => {
  it("reads one entry per line or comma, dropping blanks", () => {
    expect(networkLines(" 203.0.113.0/24\n\n2001:db8::/48 , 198.51.100.7\r\n")).toEqual([
      "203.0.113.0/24",
      "2001:db8::/48",
      "198.51.100.7",
    ]);
    expect(networkLines("")).toEqual([]);
    expect(networkLines(null)).toEqual([]);
  });

  it("shows the list one per line and sums it up in words", () => {
    expect(networksText(["10.0.0.0/8", "192.0.2.1/32"])).toBe("10.0.0.0/8\n192.0.2.1/32");
    expect(networksText(undefined)).toBe("");
    expect(networksSummary([])).toBe("Any network");
    expect(networksSummary(undefined)).toBe("Any network");
    expect(networksSummary(["10.0.0.0/8"])).toBe("Only 10.0.0.0/8");
    expect(networksSummary(["10.0.0.0/8", "192.0.2.0/24"])).toBe("Only 2 networks");
  });
});
