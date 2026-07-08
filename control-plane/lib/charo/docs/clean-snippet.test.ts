import { describe, it, expect } from "vitest";
import { cleanSnippet } from "./clean-snippet";

describe("cleanSnippet", () => {
  it("strips markdown emphasis and backticks but keeps content", () => {
    expect(cleanSnippet("set the **tenant** level via `tracing_enabled`")).toBe(
      "set the tenant level via tracing_enabled",
    );
  });

  it("drops table separator rows and turns pipes into interpuncts", () => {
    expect(cleanSnippet("| File | What it adds | | --- | --- | | 0001_init.sql | Full schema |")).toBe(
      "File · What it adds · 0001_init.sql · Full schema",
    );
  });

  it("removes a leading ellipsis and any cut-off partial token after it", () => {
    expect(cleanSnippet("…remental migrations are applied in order")).toBe(
      "migrations are applied in order",
    );
  });

  it("keeps a leading ellipsis word when it starts cleanly capitalized", () => {
    expect(cleanSnippet("…Migrations are applied in order")).toBe("Migrations are applied in order");
  });

  it("collapses whitespace and trims stray separators", () => {
    expect(cleanSnippet("  a   b | ")).toBe("a b");
  });

  it("returns empty string for empty/undefined-ish input", () => {
    expect(cleanSnippet("")).toBe("");
    expect(cleanSnippet("   ")).toBe("");
  });
});
