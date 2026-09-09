import { describe, expect, it } from "vitest";
import { conversationRows } from "./conversation-rows";
import type { ChatTurn } from "@/components/charo/use-charo-stream";
const user = (id: string, promptId?: string): ChatTurn => ({ id, promptId, role: "user", content: "same question" });
const answer = (id: string): ChatTurn => ({ id, role: "assistant", content: id });
describe("shared conversation timeline", () => {
  it("shows a broadcast once with separate answers and keeps repeated questions distinct", () => {
    const rows = conversationRows([[user("a", "2:100"), answer("alpha"), user("c", "2:200"), answer("again")], [user("b", "2:100"), answer("beta")]]);
    expect(rows).toHaveLength(2);
    expect(rows[0].answers.size).toBe(2);
    expect(rows[1].answers.size).toBe(1);
  });
  it("orders follow-ups after continuing with another model or adding a participant", () => {
    const rows = conversationRows([[user("a", "2:100"), answer("a1"), user("d", "2:300"), answer("a3")], [user("b", "2:100"), answer("b1"), user("c", "2:200"), answer("b2")], [user("e", "2:300"), answer("new")]]);
    expect(rows.map((r) => r.id)).toEqual(["2:100", "2:200", "2:300"]);
    expect([...rows[1].answers.keys()]).toEqual([1]);
    expect([...rows[2].answers.keys()]).toEqual([0, 2]);
  });
  it("preserves legacy transcripts and guided activity cards", () => {
    const rows = conversationRows([[answer("workflow"), user("a"), answer("a1")], [user("b"), answer("b1")]]);
    expect(rows[0].answers.get(0)?.[0].id).toBe("workflow");
    expect(rows[1].answers.size).toBe(2);
  });
});
