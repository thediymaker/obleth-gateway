import { describe, it, expect } from "vitest";
import { normalizeModelApiNameDraft, normalizeModelApiNameFinal } from "./model-name";

describe("normalizeModelApiNameFinal", () => {
  it("lowercases and turns slashes/spaces into dashes", () => {
    expect(normalizeModelApiNameFinal("meta-llama/Llama-3.1-70B")).toBe(
      "meta-llama-llama-3.1-70b",
    );
  });

  it("trims leading/trailing dashes and dots", () => {
    expect(normalizeModelApiNameFinal("  GPT 4o  ")).toBe("gpt-4o");
  });

  it("collapses repeated separators", () => {
    expect(normalizeModelApiNameFinal("a__b  c//d")).toBe("a-b-c-d");
  });
});

describe("normalizeModelApiNameDraft", () => {
  it("keeps a trailing dash so mid-typing is preserved", () => {
    expect(normalizeModelApiNameDraft("gpt ")).toBe("gpt-");
  });
});
