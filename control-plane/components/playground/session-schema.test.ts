import { z } from "zod";
import { describe, expect, it } from "vitest";
import { ROUTER_PROMPT_MAX_LENGTH, sessionSchema } from "./playground";

// Pins the failure mode fix round 2 found: a `routerPrompt` longer than the
// schema allows doesn't just get rejected for that one session — it fails
// `z.array(sessionSchema).safeParse(...)` for the *whole* array, and
// playground.tsx's mount effect responds to that by silently replacing every
// saved session with a fresh one. So the cap must be generous enough that an
// operator pasting a real prompt never grazes it, and the textarea's
// `maxLength` (checked in router-workspace.test.tsx) must physically prevent
// typing past it in the first place.
const base = { id: "s1", title: "t", mode: "router" as const, models: ["auto"], generation: { systemPrompt: "" } };

describe("sessionSchema / routerPrompt cap", () => {
  it("round-trips a session whose routerPrompt sits exactly at the cap", () => {
    const session = { ...base, routerPrompt: "x".repeat(ROUTER_PROMPT_MAX_LENGTH) };
    const result = z.array(sessionSchema).safeParse([session]);
    expect(result.success).toBe(true);
    if (result.success) expect(result.data[0].routerPrompt).toHaveLength(ROUTER_PROMPT_MAX_LENGTH);
  });

  it("rejects a routerPrompt one character past the cap", () => {
    const session = { ...base, routerPrompt: "x".repeat(ROUTER_PROMPT_MAX_LENGTH + 1) };
    expect(z.array(sessionSchema).safeParse([session]).success).toBe(false);
  });

  it("still parses sessions with no routerPrompt at all (pre-Router-mode localStorage)", () => {
    expect(sessionSchema.safeParse(base).success).toBe(true);
  });
});

describe("sessionSchema / verdicts mode", () => {
  // Same localStorage trap the routerPrompt cap pins: a session the schema
  // rejects wipes EVERY saved session on the next load, so the verdicts
  // draft fields must round-trip and old sessions without them must parse.
  it("round-trips a verdicts session with a full question builder draft", () => {
    const session = {
      ...base,
      mode: "verdicts" as const,
      verdictModel: "glm-5-3",
      verdictState: JSON.stringify({ ticket: "duplicate charge" }),
      verdictQuestions: [
        { id: "is_urgent", type: "boolean" as const, instructions: "Urgent?", trueDesc: "act today" },
        {
          id: "department",
          type: "choice" as const,
          instructions: "Which team?",
          options: [{ name: "billing", description: "payments" }, { name: "technical", description: "bugs" }],
        },
        { id: "frustration", type: "score" as const, instructions: "How frustrated?", levels: ["calm", "annoyed", "angry"] },
      ],
    };
    const result = z.array(sessionSchema).safeParse([session]);
    expect(result.success).toBe(true);
    if (result.success) expect(result.data[0].verdictQuestions).toHaveLength(3);
  });

  it("still parses sessions predating verdicts mode", () => {
    expect(sessionSchema.safeParse({ ...base, mode: "chat" }).success).toBe(true);
  });
});
