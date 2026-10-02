import { describe, expect, it } from "vitest";
import type { Conversation } from "@/lib/charo/conversation";
import type { PlaygroundSession } from "./playground";
import { requestFor, snippet } from "./get-code";

const base: PlaygroundSession = { id: "s1", title: "t", mode: "compare", models: ["llama"], generation: { systemPrompt: "" } };
const lanes = (messages: Conversation["messages"]) => new Map([[0, { messages, activeTarget: null, targetStart: null }]]);

describe("Get code", () => {
  it("replays a chat lane up to its latest prompt, with the session's generation settings", () => {
    const { path, body } = requestFor(
      { ...base, generation: { systemPrompt: "Be brief.", temperature: 0.2, maxTokens: 64 } },
      0,
      lanes([
        { id: "1", role: "user", content: "hi" },
        { id: "2", role: "assistant", content: "hello" },
        { id: "3", role: "user", content: "again" },
        { id: "4", role: "assistant", content: "sure" },
      ]),
    );
    expect(path).toBe("/v1/chat/completions");
    expect(body).toEqual({
      model: "llama",
      messages: [
        { role: "system", content: "Be brief." },
        { role: "user", content: "hi" },
        { role: "assistant", content: "hello" },
        { role: "user", content: "again" },
      ],
      temperature: 0.2,
      max_tokens: 64,
    });
  });

  it("leaves out failed turns and Assistant activity cards", () => {
    const { body } = requestFor(base, 0, lanes([
      { id: "1", role: "user", content: "hi" },
      { id: "2", role: "assistant", content: "", workflowActivityId: "benchmark" },
      { id: "3", role: "assistant", content: "partial", error: "503" },
    ]));
    expect(body.messages).toEqual([{ role: "user", content: "hi" }]);
  });

  it("builds the image and verdicts requests from the session's drafts", () => {
    expect(requestFor({ ...base, mode: "image", imageModel: "sdxl", imagePrompt: "a cat", imageSeed: 7 }, 0, new Map())).toEqual({
      path: "/v1/images/generations",
      body: { model: "sdxl", prompt: "a cat", size: "512x512", n: 1, seed: 7 },
    });
    const verdicts = requestFor({ ...base, mode: "verdicts", verdictState: '{"a": 1}', verdictQuestions: [{ id: "ok", type: "boolean", instructions: "Fine?" }] }, 0, new Map());
    expect(verdicts.path).toBe("/v1/verdicts");
    expect(verdicts.body).toEqual({ model: "auto", state: { a: 1 }, questions: { ok: { type: "boolean", instructions: "Fine?" } } });
    const search = requestFor({ ...base, mode: "search", searchTool: "searxng-search", searchQuery: " rust ", searchDomains: "arxiv.org", searchTimeRange: "month" }, 0, new Map());
    expect(search.path).toBe("/v1/search");
    expect(search.body).toEqual({ search_tool_name: "searxng-search", query: "rust", max_results: 5, search_domain_filter: ["arxiv.org"], time_range: "month" });
  });

  it("quotes single quotes safely for the shell and writes Python literals", () => {
    const body = { model: "m", messages: [{ role: "user", content: "it's false" }], stream: false };
    expect(snippet("curl", "https://gw/v1/chat/completions", body)).toContain(`"it'\\''s false"`);
    const py = snippet("python", "https://gw/v1/chat/completions", body);
    expect(py).toContain('"stream": False');
    // Message text is left alone; only JSON literals change spelling.
    expect(py).toContain('"it\'s false"');
  });
});
