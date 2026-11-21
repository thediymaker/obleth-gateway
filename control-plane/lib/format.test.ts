import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { ago, clamp, clock, clockOrDate, compact, dateLabel, dateTime, duration, formatBytes, formatMs, formatNumber, money, oneDecimal, percent, perMillion, secsAgo, whenLogged } from "./format";

describe("compact", () => {
  it.each([
    [0, "0"],
    [999, "999"],
    [1000, "1K"],
    [1500, "1.5K"],
    [9_999, "10K"],
    [10_000, "10K"],
    [12_345, "12K"],
    [1e6, "1M"],
    [3_400_000, "3.4M"],
    [1e9, "1B"],
    [-1500, "-1.5K"],
    [NaN, "0"],
    [Infinity, "0"],
  ])("%s reads %s", (n, out) => {
    expect(compact(n)).toBe(out);
  });
});

describe("money", () => {
  it.each([
    [0, "$0"],
    [NaN, "$0"],
    [0.004, "< $0.01"],
    [0.01, "$0.01"],
    [4.6, "$4.60"],
    [46, "$46.00"],
    [1234.6, "$1,235"],
    [12_345, "$12K"],
    [2_500_000, "$2.5M"],
    [-4.6, "-$4.60"],
  ])("%s reads %s", (v, out) => {
    expect(money(v)).toBe(out);
  });

  it.each([
    [0.0012, "$0.0012"],
    [0.075, "$0.075"],
    [0.001, "$0.001"],
    [0.00012345, "$0.00012"],
    [0.5, "$0.50"],
    [4.6, "$4.60"],
  ])("precise: %s reads %s", (v, out) => {
    expect(money(v, { precise: true })).toBe(out);
  });

  it("perMillion prices a per-token figure per million tokens", () => {
    expect(perMillion(0.0000006)).toBe("$0.60");
    expect(perMillion(0.0000000015)).toBe("$0.0015");
    expect(perMillion(0)).toBe("free");
  });
});

describe("formatMs", () => {
  it.each([
    [0, "—"],
    [NaN, "—"],
    [340, "340 ms"],
    [1500, "1.5 s"],
    [12_400, "12 s"],
    [150_000, "2.5 min"],
  ])("%s reads %s", (ms, out) => {
    expect(formatMs(ms)).toBe(out);
  });
});

describe("percent", () => {
  it.each([
    [0, "0%"],
    [0.031, "3.1%"],
    [0.05, "5%"],
    [0.42, "42%"],
    [1, "100%"],
    [NaN, "0%"],
  ])("%s reads %s", (share, out) => {
    expect(percent(share)).toBe(out);
  });
});

describe("ago", () => {
  const now = Date.UTC(2026, 10, 20, 15, 0, 0);
  it.each([
    [0, "just now", "now"],
    [59, "59 s ago", "now"],
    [60, "1 min ago", "1m"],
    [59 * 60, "59 min ago", "59m"],
    [23 * 3600, "23 h ago", "23h"],
    [2 * 86_400, "2 d ago", "2d"],
  ])("%s s back reads %s, short %s", (secs, long, short) => {
    expect(ago(now - secs * 1000, now)).toBe(long);
    expect(ago(new Date(now - secs * 1000).toISOString(), now, { short: true })).toBe(short);
  });

  it("gives the date past 60 days, with the year when it differs", () => {
    expect(ago(now - 400 * 86_400_000, now)).toBe(dateLabel(now - 400 * 86_400_000, { year: true }));
    expect(ago(now - 400 * 86_400_000, now)).toMatch(/2025$/);
  });

  it("reads a future time as just now, and no time as never", () => {
    expect(ago(now + 30_000, now)).toBe("just now");
    expect(ago(null, now)).toBe("never");
    expect(ago(undefined, now, { short: true })).toBe("");
    expect(ago("not a date", now)).toBe("never");
    expect(secsAgo(null, now)).toBe("never");
    expect(secsAgo(240, now)).toBe("4 min ago");
  });

  it("a log line is ago within the hour, then the clock", () => {
    expect(whenLogged(now - 12_000, now)).toBe("12 s ago");
    expect(whenLogged(now - 2 * 3_600_000, now)).toBe(clockOrDate(now - 2 * 3_600_000, now));
  });
});

describe("duration", () => {
  it.each([
    [0, "0 s"],
    [45_000, "45 s"],
    [384_000, "6 min 24 s"],
    [720_000, "12 min"],
    [3 * 3_600_000 + 5 * 60_000, "3 h 5 min"],
    [2 * 86_400_000, "2 d"],
    [2 * 86_400_000 + 4 * 3_600_000, "2 d 4 h"],
  ])("%s ms reads %s", (ms, out) => {
    expect(duration(ms)).toBe(out);
  });
});

describe("dates and clocks", () => {
  it("are always written the en-US way", () => {
    const at = new Date(2026, 10, 12, 14, 20, 5);
    expect(clock(at)).toBe("2:20 PM");
    expect(dateLabel(at)).toBe("Nov 12");
    expect(dateLabel(at, { year: true })).toBe("Nov 12, 2026");
    expect(dateTime(at)).toBe("Nov 12, 2:20 PM");
    expect(clockOrDate(at, at.getTime() + 60_000)).toBe("2:20 PM");
    expect(clockOrDate(at, at.getTime() + 2 * 86_400_000)).toBe("Nov 12, 2:20 PM");
    expect(dateLabel("2026-11-12T00:00:00Z", { utc: true })).toBe("Nov 12");
    expect(dateLabel("garbage")).toBe("—");
  });
});

describe("counts", () => {
  it("groups thousands in en-US whatever the browser's locale", () => {
    expect(formatNumber(1234567)).toBe("1,234,567");
    expect(formatNumber(1234.5)).toBe("1,234.5");
    expect(formatNumber(NaN)).toBe("0");
    expect(oneDecimal(1234.56)).toBe("1,234.6");
  });

  it("clamp", () => {
    expect(clamp(5, 0, 3)).toBe(3);
    expect(clamp(-1, 0, 3)).toBe(0);
  });
});

describe("formatBytes", () => {
  it("scales to readable units", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1024)).toBe("1.0 KB");
    expect(formatBytes(1024 * 1024 * 235)).toBe("235.0 MB");
  });

  it("stays in whole bytes below 1 KB", () => {
    expect(formatBytes(500)).toBe("500 B");
    expect(formatBytes(1023)).toBe("1023 B");
  });

  it("reaches GB and TB", () => {
    expect(formatBytes(1024 ** 3)).toBe("1.0 GB");
    expect(formatBytes(1024 ** 4)).toBe("1.0 TB");
  });

  it("caps at TB rather than inventing a larger unit", () => {
    expect(formatBytes(1024 ** 5)).toBe("1024.0 TB");
  });

  it("treats non-finite or negative input as 0 B", () => {
    expect(formatBytes(NaN)).toBe("0 B");
    expect(formatBytes(-5)).toBe("0 B");
  });
});

describe("one home for number formatting", () => {
  const root = join(__dirname, "..");
  const sources: string[] = [];
  const walk = (dir: string) => {
    for (const name of readdirSync(dir)) {
      if (name === "node_modules" || name.startsWith(".")) continue;
      const path = join(dir, name);
      if (statSync(path).isDirectory()) walk(path);
      else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name) && !path.endsWith(join("lib", "format.ts"))) sources.push(path);
    }
  };
  for (const dir of ["app", "components", "lib"]) walk(join(root, dir));

  it("no other file defines a number, money or duration formatter", () => {
    const retired = /\b(?:function|const)\s+(compact|compactAxis|formatMs|money|tokensShort|formatTokens|formatCost|formatCurrency|formatNumber|formatCompact|formatDurationMs|formatUsd)\b/;
    const offenders = sources.filter((p) => retired.test(readFileSync(p, "utf8"))).map((p) => p.slice(root.length + 1));
    expect(offenders).toEqual([]);
  });

  it("numbers are not formatted in the browser's locale", () => {
    const browserLocale = /\.toLocale(?:Date|Time)?String\(/;
    const offenders = sources.filter((p) => browserLocale.test(readFileSync(p, "utf8"))).map((p) => p.slice(root.length + 1));
    expect(offenders).toEqual([]);
  });

  it("no other file defines its own time-ago or duration helper", () => {
    const retired = /\b(?:function\s+(?:ago|agoShort|timeAgo|shortAgo|seenAgo|formatLastUsed|clockLabel|duration|since|stamp)\s*\(|const\s+(?:ago|agoShort|timeAgo|shortAgo|seenAgo|formatLastUsed|clockLabel|duration|since|stamp)\s*=\s*\()/;
    const offenders = sources.filter((p) => retired.test(readFileSync(p, "utf8"))).map((p) => p.slice(root.length + 1));
    expect(offenders).toEqual([]);
  });
});
