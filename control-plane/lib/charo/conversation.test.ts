import { describe, expect, it } from "vitest";
import { restoreConversation } from "./conversation";
import { chatRequestSchema } from "./chat-request";

describe("saved conversations", () => {
  it("marks interrupted output and clears stale polling state", () => {
    const restored = restoreConversation(JSON.stringify({ messages: [{ id: "1", role: "assistant", content: "partial", streaming: true, tracePending: true }], activeTarget: "model" }));
    expect(restored?.messages[0]).toMatchObject({ streaming: false, tracePending: false, content: "partial" });
    expect(restored?.messages[0].error).toContain("Interrupted");
  });
  it("ignores invalid persisted data", () => {
    expect(restoreConversation("not-json")).toBeNull();
    expect(restoreConversation('{"messages":[null]}')).toBeNull();
  });
});
describe("generation validation", () => {
  const request = { model: "auto", messages: [{ role: "user", content: "hello" }] };
  it("accepts model defaults and bounded generation controls", () => {
    expect(chatRequestSchema.safeParse(request).success).toBe(true);
    expect(chatRequestSchema.safeParse({ ...request, generation: { temperature: 0, maxTokens: 100, systemPrompt: "Be brief" } }).success).toBe(true);
  });
  it.each([{ temperature: -1 }, { temperature: 3 }, { maxTokens: 0 }, { maxTokens: 1.5 }, { maxTokens: 131073 }])("rejects invalid settings %j", (generation) => {
    expect(chatRequestSchema.safeParse({ ...request, generation }).success).toBe(false);
  });
});
