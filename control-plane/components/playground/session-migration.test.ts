import { expect, it } from "vitest";
import { migrateLegacySessions } from "./session-migration";
import type { PlaygroundSession } from "./playground";
it("preserves both old modes without mislabelling direct-model responses", () => {
  const data = new Map<string, string>([["root:one:chat", JSON.stringify({ activeTarget: null, messages: ["tool result"] })], ["root:one:compare:0", "alpha transcript"]]);
  const storage = { getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => { data.set(key, value); } };
  const sessions: PlaygroundSession[] = [{ id: "one", title: "Saved", mode: "chat", models: ["alpha", "beta"], generation: { systemPrompt: "" } }];
  migrateLegacySessions(sessions, "root", storage);
  expect(sessions).toHaveLength(2);
  expect(sessions[0].models).toEqual(["alpha", "beta"]);
  expect(sessions[1].models).toEqual(["charo"]);
  expect(data.get("root:one:compare:0")).toBe("alpha transcript");
  expect(data.get("root:one-chat:compare:0")).toBe(data.get("root:one:chat"));
  migrateLegacySessions(sessions, "root", storage);
  expect(sessions).toHaveLength(2);
});
