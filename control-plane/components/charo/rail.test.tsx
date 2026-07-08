// @vitest-environment jsdom
import { describe, it, expect } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { Rail, MicroLabel } from "./rail";

describe("Rail / MicroLabel", () => {
  it("MicroLabel renders uppercase micro text", () => {
    const out = renderToStaticMarkup(<MicroLabel>Sources · 3</MicroLabel>);
    expect(out).toContain("uppercase");
    expect(out).toContain("Sources · 3");
  });

  it("Rail wraps children with a violet left border by default", () => {
    const out = renderToStaticMarkup(<Rail><p>body</p></Rail>);
    expect(out).toContain("border-l-2");
    expect(out).toContain("border-violet-500/45");
    expect(out).toContain("body");
  });

  it("Rail supports a destructive tone", () => {
    const out = renderToStaticMarkup(<Rail tone="destructive">x</Rail>);
    expect(out).toContain("border-destructive/50");
  });
});
