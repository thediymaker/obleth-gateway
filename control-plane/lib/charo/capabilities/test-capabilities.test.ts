import { describe, it, expect, vi, beforeEach } from "vitest";

// Mock the telemetry lookups the tool uses for trace assembly.
vi.mock("@/lib/obleth", () => ({
  obleth: {
    getRequestSpans: vi.fn(async () => []),
    usageLogs: vi.fn(async () => []),
  },
}));

import { obleth } from "@/lib/obleth";
import { testCapabilitiesTool } from "./test-capabilities";
import type { ToolCtx } from "@/lib/charo/tools/types";

function jsonResponse(body: unknown, headers: Record<string, string> = {}, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json", ...headers },
  });
}

const completion = (content: string) => ({ choices: [{ message: { content } }] });

function ctxWith(gatewayChat: ToolCtx["gatewayChat"]): ToolCtx {
  return { settings: {} as never, gatewayChat, signal: new AbortController().signal };
}

describe("test_capabilities tool", () => {
  beforeEach(() => {
    vi.mocked(obleth.getRequestSpans).mockResolvedValue([]);
    vi.mocked(obleth.usageLogs).mockResolvedValue([]);
  });

  it("parseArgs requires a model and defaults tests to ping", () => {
    expect(() => testCapabilitiesTool.parseArgs({})).toThrow();
    expect(testCapabilitiesTool.parseArgs({ model: "m" })).toEqual({ model: "m", tests: ["ping"] });
    expect(testCapabilitiesTool.parseArgs({ model: "m", tests: ["ping", "json"] }))
      .toEqual({ model: "m", tests: ["ping", "json"] });
    expect(testCapabilitiesTool.parseArgs({ model: "m", tests: ["ping", "ping", "json"] }))
      .toEqual({ model: "m", tests: ["ping", "json"] });
  });

  it("runs each selected test against the model and evaluates outcomes", async () => {
    const gateway = vi.fn(async (body: { messages: unknown[] }) =>
      jsonResponse(completion("Hello, I'm here."), { "x-obleth-request-id": "req-1" }),
    );
    // Make the trace land on the first poll so fetchTrace doesn't sleep its budget.
    vi.mocked(obleth.usageLogs).mockResolvedValue([{ model: "gemma", status_code: 200 } as never]);
    const emitted: unknown[] = [];
    const result = await testCapabilitiesTool.run(
      { model: "gemma", tests: ["ping"] },
      ctxWith(gateway as never),
      (p) => emitted.push(p),
    );
    expect(gateway).toHaveBeenCalledTimes(1);
    expect(result.modelName).toBe("gemma");
    expect(result.tests[0]).toMatchObject({ id: "ping", status: "pass" });
    expect(emitted).toHaveLength(1); // one progress event per test
  });

  it("marks a test failed when the gateway returns a non-2xx", async () => {
    const gateway = vi.fn(async () => jsonResponse({ error: "boom" }, {}, 500));
    const result = await testCapabilitiesTool.run(
      { model: "m", tests: ["ping"] },
      ctxWith(gateway as never),
      () => {},
    );
    expect(result.tests[0].status).toBe("fail");
  });

  it("sends an image part for the vision test", async () => {
    let sent: any;
    const gateway = vi.fn(async (body: any) => { sent = body; return jsonResponse(completion("a shape")); });
    await testCapabilitiesTool.run({ model: "m", tests: ["vision"] }, ctxWith(gateway as never), () => {});
    const content = sent.messages[0].content;
    expect(Array.isArray(content)).toBe(true);
    expect(content.some((p: any) => p.type === "image_url")).toBe(true);
  });
});
