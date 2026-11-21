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

describe("boon spans", () => {
  it("names the structured-output, tool loop and speculation spans", () => {
    expect(spanLabel("boon:structured_output")).toBe("Structured Output");
    expect(spanLabel("boon:structured_repair")).toBe("Repair");
    expect(spanLabel("boon:tool_loop:iter:0")).toBe("Iter 0");
    expect(spanLabel("boon:web_search")).toBe("Web Search");
    expect(spanLabel("boon:speculation")).toBe("Speculation");
    expect(spanLabel("boon:speculation:draft")).toBe("Draft");
    expect(spanLabel("boon:speculation:verify")).toBe("Verify");
    expect(spanLabel("boon:speculation:escalate")).toBe("Escalate");
    expect(spanHint("boon:speculation:escalate")).toBe("Target model continuing the answer");
  });
});
