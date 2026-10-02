import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SearchWorkspace, domainList } from "./search-workspace";
import type { PlaygroundSession } from "./playground";
import type { ModelRoute } from "@/lib/obleth";

let root: Root;
let host: HTMLDivElement;
let bodies: unknown[];
let respond: () => { ok: boolean; status: number; json: unknown };

const models = [
  { id: "1", model_name: "glm-5-3", model_type: "chat", enabled: true },
  { id: "2", model_name: "searxng-search", model_type: "search", enabled: true },
  { id: "3", model_name: "retired-search", model_type: "search", enabled: false },
] as unknown as ModelRoute[];

function session(patch: Partial<PlaygroundSession> = {}): PlaygroundSession {
  return {
    id: "s1",
    title: "Untitled session",
    mode: "search",
    models: ["auto"],
    generation: { systemPrompt: "" },
    searchQuery: "attention is all you need",
    ...patch,
  };
}

const found = {
  results: [
    { title: "Attention Is All You Need", url: "https://arxiv.org/abs/1706.03762", snippet: "The dominant sequence transduction models…", date: "2017-06-12T00:00:00", last_updated: null },
    { title: "", url: "https://www.example.com/a", snippet: "", date: null, last_updated: null },
  ],
  latencyMs: 640,
  requestId: "rid-9",
};

function render(s: PlaygroundSession, list = models, update = vi.fn()) {
  act(() => {
    root.render(<SearchWorkspace session={s} update={update} models={list} loading={false} />);
  });
  return update;
}

async function clickRun() {
  await act(async () => {
    host.querySelector<HTMLButtonElement>("[aria-label='Run search']")!.click();
  });
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  bodies = [];
  respond = () => ({ ok: true, status: 200, json: found });
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  vi.stubGlobal(
    "fetch",
    vi.fn(async (_url: string, init?: RequestInit) => {
      bodies.push(JSON.parse(String(init?.body)));
      const r = respond();
      return { ok: r.ok, status: r.status, json: async () => r.json } as unknown as Response;
    }),
  );
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

describe("SearchWorkspace", () => {
  it("searches with the only enabled tool, the query and the default result count", async () => {
    render(session());
    await clickRun();
    expect(bodies).toEqual([{ tool: "searxng-search", query: "attention is all you need", maxResults: 5 }]);
  });

  it("sends the site filter and time range when set", async () => {
    render(session({ searchDomains: "arxiv.org\n\n-pinterest.com, arxiv.org", searchTimeRange: "year", searchMaxResults: 3 }));
    await clickRun();
    expect(bodies[0]).toMatchObject({ maxResults: 3, domains: ["arxiv.org", "-pinterest.com"], timeRange: "year" });
  });

  it("shows each result's title, site, date and snippet, and the run's details", async () => {
    render(session());
    await clickRun();
    const text = host.textContent ?? "";
    expect(text).toContain("Attention Is All You Need");
    expect(text).toContain("arxiv.org");
    expect(text).toContain("2017-06-12");
    expect(text).toContain("The dominant sequence transduction models");
    // A result without a title is shown by its link, on its bare host.
    expect(text).toContain("https://www.example.com/a");
    expect(text).toContain("example.com");
    expect(text).toContain("2 results");
    expect(text).toContain("640 ms");
    expect(text).toContain("rid-9");
    expect(host.querySelector("a[href='https://arxiv.org/abs/1706.03762']")?.getAttribute("rel")).toContain("noopener");
  });

  it("says plainly when a search finds nothing", async () => {
    respond = () => ({ ok: true, status: 200, json: { results: [], latencyMs: 300, requestId: null } });
    render(session());
    await clickRun();
    expect(host.textContent).toContain("found nothing");
  });

  it("shows the gateway's error", async () => {
    respond = () => ({ ok: false, status: 502, json: { error: "the search upstream is unreachable" } });
    render(session());
    await clickRun();
    expect(host.querySelector("[role='alert']")?.textContent).toContain("the search upstream is unreachable");
  });

  it("explains how to add a tool when none is registered, and cannot run", async () => {
    render(session(), models.filter((m) => m.model_type !== "search"));
    expect(host.textContent).toContain("No search tool is registered yet");
    expect(host.querySelector<HTMLButtonElement>("[aria-label='Run search']")!.disabled).toBe(true);
  });

  it("cannot run without a query", () => {
    render(session({ searchQuery: "   " }));
    expect(host.querySelector<HTMLButtonElement>("[aria-label='Run search']")!.disabled).toBe(true);
  });
});

describe("domainList", () => {
  it("takes one site per line or comma, trimmed, without repeats, at most 20", () => {
    expect(domainList(" arxiv.org \n\nnature.com,arxiv.org ,-x.org")).toEqual(["arxiv.org", "nature.com", "-x.org"]);
    expect(domainList("")).toEqual([]);
    expect(domainList(Array.from({ length: 25 }, (_, i) => `d${i}.org`).join("\n"))).toHaveLength(20);
  });
});
