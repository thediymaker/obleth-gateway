import { describe, expect, it } from "vitest";
import { isValidKeyPrefix, keyPrefixExample, keyPrefixProblem } from "./key-prefix";

describe("key prefix", () => {
  it("accepts what the gateway accepts", () => {
    for (const ok of ["obl", "sk", "rc", "air", "a", "x9"]) expect(isValidKeyPrefix(ok)).toBe(true);
    for (const bad of ["", "abcd", "Obl", "9ab", "a_b", "a-b", " rc"]) expect(isValidKeyPrefix(bad)).toBe(false);
  });

  it("says what is wrong in a sentence", () => {
    expect(keyPrefixProblem("rc")).toBeNull();
    expect(keyPrefixProblem("")).toBe("Enter a prefix.");
    expect(keyPrefixProblem("abcd")).toBe("Use at most 3 characters.");
    expect(keyPrefixProblem("9a")).toBe("Start with a lowercase letter.");
    expect(keyPrefixProblem("a-b")).toBe("Use only lowercase letters and digits.");
  });

  it("previews a key", () => {
    expect(keyPrefixExample("air")).toMatch(/^air_[0-9a-f]+…$/);
  });
});
