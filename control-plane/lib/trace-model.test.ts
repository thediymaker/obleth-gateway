import { describe, expect, it } from "vitest";
import { spanHint, spanLabel } from "./trace-model";

describe("verdict spans", () => {
  it("names the question from the attribute on new rows and from the span name on old ones", () => {
    expect(spanLabel("verdict:question", JSON.stringify({ question_id: "q7", type: "boolean" }))).toBe("Question q7");
    expect(spanLabel("verdict:question")).toBe("Question ?");
    expect(spanLabel("verdict:q:q7")).toBe("Question q7");
    expect(spanHint("verdict:question")).toBe("Single-token typed-verdict call");
    expect(spanHint("verdict:q:q7")).toBe("Single-token typed-verdict call");
    expect(spanLabel("upstream")).toBe("Upstream");
  });
});
