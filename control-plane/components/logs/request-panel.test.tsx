import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RequestPanel, StepDetail } from "./request-panel";
import { NIL_REQUEST_ID, type LogFilters } from "@/lib/logs-model";
import type { SpanNode } from "@/lib/trace-model";
import type { SpanEntry, UsageLogEntry } from "@/lib/obleth";

vi.mock("next/link", () => ({ default: ({ href, children, ...rest }: { href: string; children: React.ReactNode }) => <a href={href} {...rest}>{children}</a> }));

// Forward-computed from the gateway's own formula, same as Task 13's fixture:
// base = 0.6*0.9 + 0.4*0.7 = 0.82; untagged -> blend = base; score = base * 1.0
const scoredRow = {
  model: "gpt-oss-120b",
  level: 2,
  spare: 0.9,
  cost_score: 0.7,
  tag_score: 0,
  bias: 1.0,
  score: 0.82,
  chosen: true,
};

const newShapeExplain = {
  chosen: "gpt-oss-120b",
  difficulty: 3,
  difficulty_source: "heuristic",
  tags: [],
  tag_source: "heuristic",
  classifier_ms: 4,
  tier_domains: [],
  tier_floor: 0,
  tier_floor_clamped: false,
  weights: { capacity: 0.6, cost: 0.4, tag: 0.5, soft_cap: 8, difficulty_enabled: false },
  temperature: 0,
  uniform: 0.5,
  sampled: false,
  scored: [scoredRow],
  rejected: [{ reason: "context window too small", models: ["tiny-model"] }],
};

function span(over: Partial<SpanEntry> = {}): SpanEntry {
  return {
    request_id: "req-1",
    span_name: "auto_route",
    parent_span: "proxy_request",
    start_ms: 0,
    duration_ms: 5,
    status: "ok",
    attributes: "{}",
    ...over,
  };
}

function node(over: Partial<SpanEntry> = {}): SpanNode {
  return { span: span(over), children: [] };
}

let root: Root;
let host: HTMLDivElement;

async function render(n: SpanNode) {
  await act(async () => {
    root.render(<StepDetail node={n} onClose={() => {}} />);
  });
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

describe("auto_route span rendering", () => {
  it("renders RouteExplainPanel for a new-shape span (scored array present)", async () => {
    await render(node({ attributes: JSON.stringify(newShapeExplain) }));
    // Only RouteExplainPanel produces per-candidate score rows and the
    // rejection grouping by reason — assert on both.
    expect(host.textContent).toContain("gpt-oss-120b");
    expect(host.textContent).toContain("0.820");
    expect(host.textContent).toMatch(/context window too small/i);
    expect(host.textContent).toMatch(/not considered/i);
  });

  it("falls back to raw attribute rendering for a pre-upgrade span (regression guard)", async () => {
    const legacy = { chosen: "m", candidates: 7, tags: ["coding"] };
    await render(node({ attributes: JSON.stringify(legacy) }));
    // Raw dl rendering shows each key/value pair, including the field name
    // that a truthiness check on `candidates` (a number, not an array)
    // would otherwise misclassify as the new shape.
    expect(host.textContent).toContain("candidates");
    expect(host.textContent).toContain("7");
    expect(host.textContent).toContain("chosen");
    // The panel never rendered: no "Not considered" rejection grouping.
    expect(host.textContent).not.toMatch(/not considered/i);
  });

  it("falls back to raw rendering for the gateway's serialization-failure payload", async () => {
    const failure = { chosen: "m", error: "serde_json::to_value failed" };
    await render(node({ attributes: JSON.stringify(failure) }));
    expect(host.textContent).toContain("error");
    expect(host.textContent).toMatch(/serialization-failure|serde_json/i);
  });

  it("does not crash on unparseable attributes", async () => {
    await render(node({ attributes: "not json {{{" }));
    expect(host.querySelector("section")).not.toBeNull();
    expect(host.textContent).toContain("No attributes recorded.");
  });

  it("does not crash on empty attributes", async () => {
    await render(node({ attributes: "{}" }));
    expect(host.querySelector("section")).not.toBeNull();
    expect(host.textContent).toContain("No attributes recorded.");
  });

  it("leaves non-auto_route spans on the raw renderer even with a scored-shaped payload", async () => {
    await render(node({ span_name: "upstream", attributes: JSON.stringify(newShapeExplain) }));
    expect(host.textContent).not.toMatch(/not considered/i);
  });

  it("falls back to raw rendering when scored is present but weights is missing", async () => {
    const { weights: _weights, ...malformed } = newShapeExplain;
    await render(node({ attributes: JSON.stringify(malformed) }));
    expect(host.textContent).not.toMatch(/not considered/i);
    expect(host.textContent).toContain("scored");
  });

  it("falls back to raw rendering when a scored entry is missing score", async () => {
    const malformed = {
      ...newShapeExplain,
      scored: [{ model: "gpt-oss-120b", level: 2, spare: 0.9, cost_score: 0.7, tag_score: 0, bias: 1.0, chosen: true }],
    };
    await render(node({ attributes: JSON.stringify(malformed) }));
    expect(host.textContent).not.toMatch(/not considered/i);
  });

  it("falls back to raw rendering when tags is missing", async () => {
    const { tags: _tags, ...malformed } = newShapeExplain;
    await render(node({ attributes: JSON.stringify(malformed) }));
    expect(host.textContent).not.toMatch(/not considered/i);
  });

  it("falls back to raw rendering when tier_domains is missing", async () => {
    const { tier_domains: _tierDomains, ...malformed } = newShapeExplain;
    await render(node({ attributes: JSON.stringify(malformed) }));
    expect(host.textContent).not.toMatch(/not considered/i);
  });

  it("falls back to raw rendering when a rejected entry is missing models", async () => {
    const malformed = { ...newShapeExplain, rejected: [{ reason: "context window too small" }] };
    await render(node({ attributes: JSON.stringify(malformed) }));
    expect(host.textContent).not.toMatch(/not considered/i);
  });

  it("falls back to raw rendering when temperature or uniform is missing", async () => {
    const { temperature: _temperature, ...malformed } = newShapeExplain;
    await render(node({ attributes: JSON.stringify(malformed) }));
    expect(host.textContent).not.toMatch(/not considered/i);
  });
});

const entry = (over: Partial<UsageLogEntry> = {}): UsageLogEntry => ({
  request_id: "7c1e40a2-1111-2222-3333-444455556666", ts_ms: 10_000_000, tenant_id: "t-cs", key_id: "k-1", model: "glm-5-3",
  request_type: "chat", session_id: "", session_id_source: "none", device_id: "", admission: "fast", status_code: 200,
  input_tokens: 1200, output_tokens: 300, total_tokens: 1500, queue_wait_ms: 0, ttft_ms: 400, total_ms: 2400,
  cache_status: "off", cost_usd: 0.002, energy_wh: 0, energy_cost_usd: 0, co2_g: 0,
  tenant_name: "cs-teaching", key_name: "canvas-tutor", key_prefix: "sk-ct9", has_trace: false,
  parent_request_id: NIL_REQUEST_ID, model_variant: "", ...over,
});

describe("the request panel", () => {
  const calls: string[] = [];
  let helpers: UsageLogEntry[] = [];
  const draft = entry({ request_id: "d1000000-0000-0000-0000-000000000000", ts_ms: 9_999_000, model: "north-mini-code-glm", request_type: "speculation_draft", admission: "boon", total_tokens: 812, cost_usd: 0.0004, parent_request_id: "7c1e40a2-1111-2222-3333-444455556666" });
  const check = entry({ request_id: "d2000000-0000-0000-0000-000000000000", ts_ms: 9_999_500, request_type: "speculation_verify", admission: "boon", total_tokens: 1, cost_usd: 0.0001, parent_request_id: "7c1e40a2-1111-2222-3333-444455556666" });

  beforeEach(() => {
    calls.length = 0;
    helpers = [];
    vi.stubGlobal("fetch", vi.fn(async (url: string) => {
      calls.push(url);
      return { ok: true, json: async () => (url.includes("parent_request_id=") ? helpers : []) } as Response;
    }));
  });
  afterEach(() => vi.unstubAllGlobals());

  async function show(row: UsageLogEntry, onFilter: (patch: Partial<LogFilters>) => void = () => {}) {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    await act(async () => {
      root.render(
        <QueryClientProvider client={client}>
          <RequestPanel row={row} onClose={() => {}} onStep={() => {}} canStep={{ newer: false, older: false }} onFilter={onFilter} />
        </QueryClientProvider>,
      );
    });
    await act(async () => { await new Promise((r) => setTimeout(r, 0)); });
  }

  const fact = (label: string) => [...host.querySelectorAll("dt")].find((d) => d.textContent === label)?.nextElementSibling ?? null;
  const helperSection = () => host.querySelector('[aria-label="Helper calls"]');

  it("says why a request failed, whose side it was on, and the reason as recorded", async () => {
    const picked: Partial<LogFilters>[] = [];
    await show(entry({ status_code: 503, output_tokens: 0, cost_usd: 0, error_class: "capacity", error_reason: "No slot came free in 30 s on glm-5-3" }), (p) => picked.push(p));
    expect(host.querySelector("h2")!.textContent).toBe("No capacity");
    const why = host.querySelector('[aria-label="Why it failed"]')!;
    expect(why.textContent).toContain("No capacity, on the platform's side.");
    expect(why.textContent).toContain("No slot came free in 30 s on glm-5-3");
    const like = [...host.querySelectorAll("button")].find((b) => b.textContent === "Failures like this")!;
    await act(async () => like.click());
    expect(picked).toEqual([{ errorClass: "capacity", statusCode: "503", model: "glm-5-3", keyId: "k-1", tenantId: "t-cs", status: "" }]);
  });

  it("gives an older failure's status in place of a reason it never recorded", async () => {
    await show(entry({ status_code: 400, output_tokens: 0, cost_usd: 0, error_class: "client_input", error_reason: "" }));
    const why = host.querySelector('[aria-label="Why it failed"]')!;
    expect(why.textContent).toContain("Request refused, on the caller's side.");
    expect(why.textContent).toContain("HTTP 400");
    // A gateway that does not classify failures leaves the section out.
    await show(entry({ request_id: "b0000000-0000-0000-0000-000000000000", status_code: 502 }));
    expect(host.querySelector('[aria-label="Why it failed"]')).toBeNull();
    expect(host.querySelector("h2")!.textContent).toBe("Bad gateway");
  });

  it("names the variant a client asked for", async () => {
    await show(entry({ model_variant: "glm-5-3-spec" }));
    expect(fact("Variant")!.textContent).toBe("glm-5-3-spec, a variant of glm-5-3 with extra boons");
    await show(entry({ request_id: "e0000000-0000-0000-0000-000000000000" }));
    expect(fact("Variant")).toBeNull();
  });

  it("names the family a client called and the reasoning effort it got", async () => {
    await show(entry({ model: "acme-think", model_family: "acme", reasoning_effort: "high", reasoning_effort_requested: "high" }));
    expect(fact("Called as")!.textContent).toBe("acme, a model family. The reasoning level picked acme-think to answer.");
    expect(fact("Reasoning effort")!.textContent).toBe("Asked for high, served at high.");
    expect(fact("Variant")).toBeNull();
    await show(entry({ request_id: "f0000000-0000-0000-0000-000000000000" }));
    expect(fact("Called as")).toBeNull();
    expect(fact("Reasoning effort")).toBeNull();
  });

  it("says a buffered reply has no first token rather than timing its last", async () => {
    await show(entry({ streamed: false, ttft_ms: 0 }));
    expect(fact("First token")!.textContent).toBe("Not streamed");
    await show(entry({ request_id: "a1000000-0000-0000-0000-000000000000", streamed: true, ttft_ms: 400 }));
    expect(fact("First token")!.textContent).toBe("400 ms");
    // A row from a gateway that did not record it.
    await show(entry({ request_id: "a2000000-0000-0000-0000-000000000000", ttft_ms: 0 }));
    expect(fact("First token")!.textContent).toBe("—");
  });

  it("lists the helper calls a client request made, oldest first, each linked to its own row", async () => {
    helpers = [check, draft];
    await show(entry());
    const asked = calls.find((u) => u.includes("parent_request_id="))!;
    const params = new URL(asked, "http://x").searchParams;
    expect(params.get("parent_request_id")).toBe("7c1e40a2-1111-2222-3333-444455556666");
    expect(Number(params.get("since_ms"))).toBe(10_000_000 - 3_600_000);
    expect(Number(params.get("until_ms"))).toBe(10_000_000 + 3_600_000);
    const items = [...helperSection()!.querySelectorAll("li")];
    expect(items.map((li) => li.textContent)).toEqual([
      "north-mini-code-glm · speculation draft812 tokens · $0.0004",
      "glm-5-3 · draft check1 token · $0.0001",
    ]);
    expect(items[0].querySelector("a")!.getAttribute("href")).toBe("/logs?requestId=d1000000-0000-0000-0000-000000000000");
  });

  it("shows nothing for a request that made no helper calls", async () => {
    await show(entry());
    expect(calls.some((u) => u.includes("parent_request_id="))).toBe(true);
    expect(helperSection()).toBeNull();
    expect(host.textContent).not.toContain("Helper call");
  });

  it("links a helper call back to the request it served, and looks for no helpers of its own", async () => {
    await show(draft);
    const served = fact("Helper call for")!;
    expect(served.textContent).toBe("request 7c1e40a2, the client request this call served");
    expect(served.querySelector("a")!.getAttribute("href")).toBe("/logs?requestId=7c1e40a2-1111-2222-3333-444455556666");
    expect(fact("Type")!.textContent).toBe("speculation draft");
    expect(calls.some((u) => u.includes("parent_request_id="))).toBe(false);
  });

  it("asks nothing of a gateway too old to link helper calls", async () => {
    const { parent_request_id: _parent, model_variant: _variant, ...old } = entry();
    await show(old as UsageLogEntry);
    expect(calls).toEqual([]);
    expect(fact("Helper call for")).toBeNull();
  });
});
