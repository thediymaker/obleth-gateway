import type { RouteExplainView, SpanEntry } from "@/lib/obleth";

// A traced request's spans: names people read, attributes, the parent/child
// tree, and a timeline on one scale. Pure, so the request panel stays thin.

export function spanLabel(name: string): string {
  const labels: Record<string, string> = {
    proxy_request: "Request",
    auth_resolve: "Auth",
    auto_route: "Auto Route",
    admission: "Admission",
    cache_lookup: "Cache",
    "boon:vision": "Vision",
    "boon:tool_loop": "Tool Loop",
    "boon:structured_repair": "Repair",
    upstream: "Upstream",
    dispatch: "Upstream",
  };
  if (name in labels) return labels[name];
  if (name.startsWith("boon:tool_loop:iter:")) return `Iter ${name.split(":").pop()}`;
  if (name.startsWith("verdict:q:")) return `Question ${name.slice("verdict:q:".length)}`;
  if (name.startsWith("mcp:")) return name.slice(4);
  return name;
}

export function spanHint(name: string): string {
  const hints: Record<string, string> = {
    auth_resolve: "Key and tenant resolve",
    auto_route: "Model selection",
    admission: "Fairshare queue and budget",
    cache_lookup: "Response cache check",
    upstream: "Provider model call",
    dispatch: "Provider model call",
    "boon:vision": "Image-to-text relay",
    "boon:tool_loop": "MCP tool execution loop",
    "boon:structured_repair": "Schema validation",
  };
  if (name in hints) return hints[name];
  if (name.startsWith("boon:tool_loop:iter:")) return "Tool call + model turn";
  if (name.startsWith("verdict:q:")) return "Single-token typed-verdict call";
  if (name.startsWith("mcp:")) return "Tool server call";
  return "";
}

export function parseAttrs(raw: string): [string, string][] {
  if (!raw || raw === "{}") return [];
  try {
    const obj = JSON.parse(raw) as Record<string, unknown>;
    return Object.entries(obj)
      .filter(([, v]) => v !== null && v !== "" && v !== undefined)
      .map(([k, v]) => {
        if (Array.isArray(v)) return [k, v.join(", ")] as [string, string];
        return [k, String(v)] as [string, string];
      });
  } catch {
    return [];
  }
}

export interface SpanNode {
  span: SpanEntry;
  children: SpanNode[];
}

/**
 * Validates the subset of `RouteExplainView` that `RouteExplainPanel`
 * (control-plane/components/playground/route-explain.tsx) dereferences with
 * an array/string method call, `.toFixed()`, or an object destructure — the
 * accesses that throw during render on a missing or mistyped field, rather
 * than degrade. Full enumerated read list from that file, for reference:
 *
 *   - Read as plain JSX text or through `??`/a ternary, so `undefined`
 *     renders harmlessly instead of throwing — NOT validated here:
 *     `chosen`, `difficulty`, `difficulty_source`, `tag_source`,
 *     `tier_floor`, `classifier_ms`, `sampled`, and each row's `model` /
 *     `chosen`.
 *   - Called with `.length`, `.join`, `.toFixed`, `.map`, or destructured as
 *     an object — throws if missing/mistyped, so validated below:
 *     `tags` (.length/.join), `tier_domains` (.length/.join), `temperature`
 *     (.toFixed), `uniform` (.toFixed), `scored` (.map; each row's `score`,
 *     and — only once a row is expanded — `spare`/`cost_score`/`tag_score`/
 *     `bias`, all via `.toFixed`), `weights` (destructured for `capacity`/
 *     `cost`/`tag`, read when any row is expanded), `rejected` (.length/
 *     .map; each entry's `models` via `.join`).
 *
 * The expand-only fields (`weights`, and each row's `spare`/`cost_score`/
 * `tag_score`/`bias`) are included even though they're behind a click,
 * because a payload that renders fine collapsed and then throws the moment
 * an operator expands a row is not an acceptable degradation either.
 */
function isValidRouteExplain(obj: Record<string, unknown>): boolean {
  if (!Array.isArray(obj.tags)) return false;
  if (!Array.isArray(obj.tier_domains)) return false;
  if (typeof obj.temperature !== "number") return false;
  if (typeof obj.uniform !== "number") return false;
  if (!Array.isArray(obj.scored)) return false;
  if (!Array.isArray(obj.rejected)) return false;

  const weights = obj.weights;
  if (
    typeof weights !== "object" ||
    weights === null ||
    typeof (weights as Record<string, unknown>).capacity !== "number" ||
    typeof (weights as Record<string, unknown>).cost !== "number" ||
    typeof (weights as Record<string, unknown>).tag !== "number"
  ) {
    return false;
  }

  const rowsOk = obj.scored.every((row) => {
    if (typeof row !== "object" || row === null) return false;
    const r = row as Record<string, unknown>;
    return (
      typeof r.score === "number" &&
      typeof r.spare === "number" &&
      typeof r.cost_score === "number" &&
      typeof r.tag_score === "number" &&
      typeof r.bias === "number"
    );
  });
  if (!rowsOk) return false;

  return obj.rejected.every((entry) => {
    if (typeof entry !== "object" || entry === null) return false;
    return Array.isArray((entry as Record<string, unknown>).models);
  });
}

/**
 * Parses an `auto_route` span's attributes as the enriched `RouteExplainView`
 * payload (Task 12), keying detection off `Array.isArray(scored)` — never a
 * truthiness check on `candidates`, since the pre-upgrade payload carries
 * `candidates` as a *number*, not an array — and then validating the rest of
 * the shape `RouteExplainPanel` depends on (see `isValidRouteExplain`).
 * Returns null for anything that doesn't fully match: unparseable JSON, the
 * pre-upgrade `{ chosen, candidates, tags }` span, the gateway's
 * serialization-failure fallback `{ chosen, error }`, and a `scored`-shaped
 * payload that is missing or mistyping one of the other fields the panel
 * reads — all of which fall back to raw rendering instead of crashing.
 */
export function parseRouteExplain(raw: string): RouteExplainView | null {
  try {
    const obj = JSON.parse(raw) as Record<string, unknown>;
    if (obj && typeof obj === "object" && isValidRouteExplain(obj)) {
      return obj as unknown as RouteExplainView;
    }
  } catch {
    // Unparseable attributes: fall back to raw rendering below.
  }
  return null;
}

export function buildTree(spans: SpanEntry[]): SpanNode[] {
  const byName = new Map<string, SpanNode>();
  const byKey = new Map<string, SpanNode>();

  for (const span of spans) {
    const node: SpanNode = { span, children: [] };
    const key = nodeId(span);
    byKey.set(key, node);
    byName.set(span.span_name, node);
  }

  const roots: SpanNode[] = [];
  for (const node of byKey.values()) {
    const parent = byName.get(node.span.parent_span);
    if (parent && parent !== node) {
      parent.children.push(node);
    } else {
      roots.push(node);
    }
  }

  const sortByStart = (a: SpanNode, b: SpanNode) => a.span.start_ms - b.span.start_ms;
  for (const node of byKey.values()) node.children.sort(sortByStart);
  roots.sort(sortByStart);
  return roots;
}

export function flattenTree(nodes: SpanNode[]): SpanNode[] {
  return nodes.flatMap((node) => [node, ...flattenTree(node.children)]);
}

function nodeId(span: SpanEntry): string {
  return `${span.span_name}:${span.start_ms}`;
}

/** One step on the request's timeline, placed on a single scale from the request's start. */
export interface TimelineStep {
  node: SpanNode;
  depth: number;
  /** Start and width as a share of the whole request, 0 to 100. */
  left: number;
  width: number;
}

/**
 * Every span but the request itself, in start order with its depth, placed
 * against the request's own span (or, without one, the spans' extent). Tool
 * loop iterations stay with their loop rather than on the timeline.
 */
export function timeline(spans: SpanEntry[]): { steps: TimelineStep[]; totalMs: number } {
  const root = spans.find((s) => s.span_name === "proxy_request");
  const rest = spans.filter((s) => s !== root);
  const start = root?.start_ms ?? Math.min(...rest.map((s) => s.start_ms), 0);
  const end = root ? root.start_ms + root.duration_ms : Math.max(...rest.map((s) => s.start_ms + s.duration_ms), start + 1);
  const total = Math.max(end - start, 1);
  const steps: TimelineStep[] = [];
  const walk = (nodes: SpanNode[], depth: number) => {
    for (const n of nodes) {
      if (n.span.span_name.startsWith("boon:tool_loop:iter:")) continue;
      const left = Math.min(100, Math.max(0, ((n.span.start_ms - start) / total) * 100));
      steps.push({ node: n, depth, left, width: Math.max(0.4, Math.min(100 - left, (n.span.duration_ms / total) * 100)) });
      walk(n.children, depth + 1);
    }
  };
  walk(buildTree(rest), 0);
  return { steps, totalMs: root?.duration_ms ?? total };
}
