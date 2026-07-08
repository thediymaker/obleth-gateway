// @vitest-environment jsdom
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { CharoMarkdown } from "./markdown";

function html(text: string): string {
  return renderToStaticMarkup(<CharoMarkdown text={text} />);
}

describe("CharoMarkdown", () => {
  it("renders bold as a semibold strong, not literal asterisks", () => {
    const out = html("set it at the **tenant level** today");
    expect(out).toContain("<strong");
    expect(out).toContain("tenant level");
    expect(out).not.toContain("**");
  });

  it("renders inline code as a tinted mono chip", () => {
    const out = html("flip the `tracing_enabled` flag");
    expect(out).toMatch(/<code[^>]*>tracing_enabled<\/code>/);
    expect(out).not.toContain("`");
  });

  it("renders fenced code blocks inside a scrollable pre", () => {
    const out = html("```bash\ncurl -X PUT /api/v1/tenants/42\n```");
    expect(out).toContain("<pre");
    expect(out).toContain("curl -X PUT /api/v1/tenants/42");
  });

  it("renders gfm tables", () => {
    const out = html("| a | b |\n| --- | --- |\n| 1 | 2 |");
    expect(out).toContain("<table");
    expect(out).toContain("<td");
  });

  it("renders lists and links", () => {
    const out = html("- item one\n- [docs](https://obleth.com/docs)");
    expect(out).toContain("<ul");
    expect(out).toMatch(/<a[^>]*href="https:\/\/obleth\.com\/docs"/);
  });

  it("does not crash on malformed markdown", () => {
    expect(() => html("**unclosed `chaos | ---")).not.toThrow();
  });
});
