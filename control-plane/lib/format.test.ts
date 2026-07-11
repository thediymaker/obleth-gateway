import { describe, expect, it } from "vitest";
import {
  clamp,
  formatCompact,
  formatDecimal,
  formatDelta,
  formatDurationMs,
  formatPct,
  formatScore,
  truncateId,
} from "./format";

describe("formatCompact", () => {
  it("keeps small numbers plain", () => {
    expect(formatCompact(0)).toBe("0");
    expect(formatCompact(9.44)).toBe("9.4");
    expect(formatCompact(950)).toBe("950");
  });

  it("switches to k at 1000 with lowercase k", () => {
    expect(formatCompact(1200)).toBe("1.2k");
    expect(formatCompact(15_000)).toBe("15k");
  });

  it("handles M/B and negatives", () => {
    expect(formatCompact(3_400_000)).toBe("3.4M");
    expect(formatCompact(1_200_000_000)).toBe("1.2B");
    expect(formatCompact(-1200)).toBe("-1.2k");
  });

  it("returns 0 for non-finite input", () => {
    expect(formatCompact(NaN)).toBe("0");
    expect(formatCompact(Infinity)).toBe("0");
  });
});

describe("formatDurationMs", () => {
  it("renders sub-second in ms and longer in seconds", () => {
    expect(formatDurationMs(340)).toBe("340ms");
    expect(formatDurationMs(1500)).toBe("1.50s");
  });

  it("returns -- for zero/invalid", () => {
    expect(formatDurationMs(0)).toBe("--");
    expect(formatDurationMs(NaN)).toBe("--");
  });
});

describe("truncateId", () => {
  it("keeps short ids and truncates long ones head…tail", () => {
    expect(truncateId("abc123")).toBe("abc123");
    expect(truncateId("0123456789abcdef")).toBe("01234567…cdef");
  });
});

describe("misc formatters", () => {
  it("formatPct", () => {
    expect(formatPct(3.14)).toBe("3.1%");
    expect(formatPct(42)).toBe("42%");
    expect(formatPct(NaN)).toBe("0%");
  });

  it("formatDecimal", () => {
    expect(formatDecimal(3.44)).toBe("3.4");
    expect(formatDecimal(12.6)).toBe("13");
  });

  it("formatDelta", () => {
    expect(formatDelta(0.01)).toBe("0");
    expect(formatDelta(2.4)).toBe("+2.4");
    expect(formatDelta(-11.2)).toBe("-11");
  });

  it("formatScore", () => {
    expect(formatScore(4.567)).toBe("4.57");
    expect(formatScore(56.2)).toBe("56");
    expect(formatScore(4200)).toBe("4.2k");
  });

  it("clamp", () => {
    expect(clamp(5, 0, 3)).toBe(3);
    expect(clamp(-1, 0, 3)).toBe(0);
  });
});
