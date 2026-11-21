import { describe, expect, it } from "vitest";
import { finiteNumber, formNumber, numOr, numOrNull, optionalNonNegInt, optionalPositiveInt, positiveIntWithDefault } from "./form-helpers";

describe("form numbers", () => {
  it.each([
    [null, undefined, undefined, 7, null],
    ["", undefined, undefined, 7, null],
    ["   ", undefined, undefined, 7, null],
    [" 12 ", 12, 12, 12, 12],
    ["1e3", 1000, 1000, 1000, 1000],
    ["-0", -0, -0, -0, -0],
    ["NaN", NaN, undefined, 7, null],
    ["abc", NaN, undefined, 7, null],
    ["Infinity", Infinity, undefined, 7, null],
    ["1e400", Infinity, undefined, 7, null],
  ])("%j reads as %s / %s / %s / %s", (raw, form, finite, or, orNull) => {
    expect(formNumber(raw)).toBe(form);
    expect(finiteNumber(raw)).toBe(finite);
    expect(numOr(raw, 7)).toBe(or);
    expect(numOrNull(raw)).toBe(orNull);
  });
});

describe("form number schemas", () => {
  it.each([
    ["", true, undefined],
    ["  ", true, undefined],
    ["3", true, 3],
    ["1e3", true, 1000],
    ["0", false, undefined],
    ["-1", false, undefined],
    ["2.5", false, undefined],
    ["abc", false, undefined],
  ])("an optional positive integer takes %j: %s", (raw, ok, value) => {
    const r = optionalPositiveInt.safeParse(raw);
    expect(r.success).toBe(ok);
    if (ok) expect(r.data).toBe(value);
  });

  it("a non-negative integer takes zero and -0 but not a negative", () => {
    expect(optionalNonNegInt.safeParse("0").data).toBe(0);
    expect(optionalNonNegInt.safeParse("-0").success).toBe(true);
    expect(optionalNonNegInt.safeParse("-5").success).toBe(false);
  });

  it("a blank field takes the default", () => {
    expect(positiveIntWithDefault(8).parse("")).toBe(8);
    expect(positiveIntWithDefault(8).parse(" 4 ")).toBe(4);
  });
});
