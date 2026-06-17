"use client";

import { useEffect, useRef, useState, type CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";
import type { SpanEntry, UsageLogEntry } from "@/lib/obleth";
import { cn } from "@/lib/utils";

interface Props {
  row: UsageLogEntry;
}

const MIN_NODE_WIDTH = 120;
const MAX_NODE_WIDTH = 168;
const NODE_HEIGHT = 116;
const MIN_NODE_GAP = 20;
const ROW_GAP = 34;
const CANVAS_PAD_X = 18;
const CANVAS_PAD_Y = 18;

export function RequestDetail({ row }: Props) {
  const { data: spans, isLoading } = useQuery({
    queryKey: ["spans", row.request_id],
    queryFn: async () => {
      const res = await fetch(`/api/live/usage/logs/${row.request_id}/spans`);
      if (!res.ok) return [] as SpanEntry[];
      return res.json() as Promise<SpanEntry[]>;
    },
    staleTime: 60_000,
    enabled: row.has_trace,
  });

  if (row.has_trace && isLoading) {
    return (
      <div className="animate-pulse px-4 py-6 text-xs text-muted-foreground">
        Loading trace...
      </div>
    );
  }

  if (spans && spans.length > 0) {
    return (
      <div>
        <TraceView spans={spans} />
        <UsageDetailCard row={row} withTrace />
      </div>
    );
  }

  return <UsageDetailCard row={row} />;
}

function spanAccent(name: string, status: string): string {
  if (status === "error") return "hsl(0 68% 60%)";
  if (name === "upstream" || name === "dispatch") return "hsl(198 68% 58%)";
  if (name === "cache_lookup") return "hsl(38 72% 58%)";
  return "hsl(158 54% 54%)";
}

function spanLabel(name: string): string {
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
  if (name.startsWith("mcp:")) return name.slice(4);
  return name;
}

function spanHint(name: string): string {
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
  if (name.startsWith("mcp:")) return "Tool server call";
  return "";
}

function parseAttrs(raw: string): [string, string][] {
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

interface SpanNode {
  span: SpanEntry;
  children: SpanNode[];
}

interface CanvasNode {
  id: string;
  node: SpanNode;
  x: number;
  y: number;
  width: number;
  height: number;
}

interface CanvasEdge {
  id: string;
  d: string;
  endX: number;
  endY: number;
  error: boolean;
}

function buildTree(spans: SpanEntry[]): SpanNode[] {
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

function flattenTree(nodes: SpanNode[]): SpanNode[] {
  return nodes.flatMap((node) => [node, ...flattenTree(node.children)]);
}

function nodeId(span: SpanEntry): string {
  return `${span.span_name}:${span.start_ms}`;
}

function layoutCanvas(nodes: SpanNode[], availableWidth: number): {
  nodes: CanvasNode[];
  edges: CanvasEdge[];
  width: number;
  height: number;
} {
  const width = Math.max(Math.floor(availableWidth || 760), 260);
  const innerWidth = Math.max(width - CANVAS_PAD_X * 2, 1);
  const oneRowMin =
    nodes.length * MIN_NODE_WIDTH + Math.max(0, nodes.length - 1) * MIN_NODE_GAP;
  const perRow =
    nodes.length === 0
      ? 1
      : oneRowMin <= innerWidth
        ? nodes.length
        : Math.max(
            1,
            Math.floor((innerWidth + MIN_NODE_GAP) / (MIN_NODE_WIDTH + MIN_NODE_GAP)),
          );

  const canvasNodes: CanvasNode[] = [];
  for (let start = 0; start < nodes.length; start += perRow) {
    const rowNodes = nodes.slice(start, start + perRow);
    const row = Math.floor(start / perRow);
    const rowCount = rowNodes.length;
    const naturalWidth =
      rowCount * MAX_NODE_WIDTH + Math.max(0, rowCount - 1) * MIN_NODE_GAP;
    const nodeWidth =
      rowCount === 1
        ? Math.min(MAX_NODE_WIDTH, Math.max(Math.min(MIN_NODE_WIDTH, innerWidth), innerWidth))
        : Math.min(
            MAX_NODE_WIDTH,
            Math.max(
              MIN_NODE_WIDTH,
              (innerWidth - Math.max(0, rowCount - 1) * MIN_NODE_GAP) / rowCount,
            ),
          );
    const gap =
      rowCount > 1
        ? Math.max(
            MIN_NODE_GAP,
            (innerWidth - rowCount * nodeWidth) / Math.max(1, rowCount - 1),
          )
        : 0;
    const rowWidth = rowCount * nodeWidth + Math.max(0, rowCount - 1) * gap;
    const rowInset = rowCount === 1 ? Math.max(0, (innerWidth - nodeWidth) / 2) : 0;
    const fillInset =
      oneRowMin <= innerWidth || naturalWidth > innerWidth ? rowInset : Math.max(0, (innerWidth - rowWidth) / 2);

    rowNodes.forEach((node, column) => {
      canvasNodes.push({
        id: nodeId(node.span),
        node,
        x: CANVAS_PAD_X + fillInset + column * (nodeWidth + gap),
        y: CANVAS_PAD_Y + row * (NODE_HEIGHT + ROW_GAP),
        width: nodeWidth,
        height: NODE_HEIGHT,
      });
    });
  }

  const edges = canvasNodes.slice(1).map((to, i) => {
    const from = canvasNodes[i];
    const startX = from.x + from.width;
    const startY = from.y + from.height / 2;
    const endX = to.x;
    const endY = to.y + to.height / 2;
    const sameRow = Math.abs(startY - endY) < 2;
    const d = sameRow
      ? horizontalEdge(startX, startY, endX, endY)
      : wrappedEdge(startX, startY, endX, endY, width);

    return {
      id: `${from.id}->${to.id}`,
      d,
      endX,
      endY,
      error: to.node.span.status === "error",
    };
  });

  const rowCount = Math.max(1, Math.ceil(nodes.length / perRow));
  const height = CANVAS_PAD_Y * 2 + rowCount * NODE_HEIGHT + Math.max(0, rowCount - 1) * ROW_GAP;

  return { nodes: canvasNodes, edges, width, height };
}

function horizontalEdge(startX: number, startY: number, endX: number, endY: number): string {
  const dx = Math.max(24, endX - startX);
  return `M ${startX} ${startY} C ${startX + dx * 0.5} ${startY}, ${
    endX - dx * 0.5
  } ${endY}, ${endX} ${endY}`;
}

function wrappedEdge(
  startX: number,
  startY: number,
  endX: number,
  endY: number,
  width: number,
): string {
  const railX = width - CANVAS_PAD_X;
  const midY = (startY + endY) / 2;
  return `M ${startX} ${startY} C ${railX} ${startY}, ${railX} ${midY}, ${railX} ${midY} C ${railX} ${endY}, ${
    endX - 28
  } ${endY}, ${endX} ${endY}`;
}

function TraceView({ spans }: { spans: SpanEntry[] }) {
  const canvasRef = useRef<HTMLDivElement | null>(null);
  const [canvasWidth, setCanvasWidth] = useState(0);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const root = spans.find((s) => s.span_name === "proxy_request");
  const tree = buildTree(spans.filter((s) => s.span_name !== "proxy_request"));
  const flowNodes = flattenTree(tree)
    .filter((n) => !n.span.span_name.startsWith("boon:tool_loop:iter:"))
    .sort((a, b) => a.span.start_ms - b.span.start_ms);
  const maxDuration = Math.max(...flowNodes.map((n) => n.span.duration_ms), 1);
  const totalMs = root?.duration_ms ?? flowNodes.reduce((a, n) => a + n.span.duration_ms, 0);
  const hasError = spans.some((s) => s.status === "error");
  const layout = layoutCanvas(flowNodes, canvasWidth);
  const selectedNode = selectedId
    ? (layout.nodes.find((n) => n.id === selectedId)?.node ?? null)
    : null;

  useEffect(() => {
    const el = canvasRef.current;
    if (!el) return;

    const updateWidth = () => setCanvasWidth(Math.floor(el.clientWidth));
    updateWidth();

    const observer = new ResizeObserver(updateWidth);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  return (
    <div className="trace-grid border-t border-border/40 px-3 py-4 sm:px-4">
      <div className="mb-3 flex flex-col gap-1.5 sm:flex-row sm:items-center sm:justify-between">
        <p className="flex items-center gap-2 text-[10px] font-semibold uppercase tracking-widest text-muted-foreground">
          <span
            className={cn(
              "inline-block h-1.5 w-1.5 rounded-full",
              hasError ? "bg-red-500" : "bg-emerald-400",
            )}
          />
          Request Flow
        </p>
        <span className="text-[10px] tabular-nums text-muted-foreground">
          {flowNodes.length} {flowNodes.length === 1 ? "node" : "nodes"} / {fmtMs(totalMs)} total
        </span>
      </div>

      {flowNodes.length > 0 ? (
        <div ref={canvasRef} className="trace-canvas-scroll">
          <div className="trace-canvas" style={{ width: layout.width, height: layout.height }}>
            <svg
              className="trace-edge-layer"
              width={layout.width}
              height={layout.height}
              viewBox={`0 0 ${layout.width} ${layout.height}`}
              aria-hidden
            >
              {layout.edges.map((edge) => (
                <g key={edge.id} className={cn("trace-edge", edge.error && "trace-edge-error")}>
                  <path d={edge.d} />
                  <circle cx={edge.endX} cy={edge.endY} r="3.5" />
                </g>
              ))}
            </svg>
            {layout.nodes.map((canvasNode) => (
              <FlowNode
                key={canvasNode.id}
                canvasNode={canvasNode}
                maxDuration={maxDuration}
                isSelected={selectedId === canvasNode.id}
                onSelect={() =>
                  setSelectedId(selectedId === canvasNode.id ? null : canvasNode.id)
                }
              />
            ))}
          </div>
        </div>
      ) : (
        <div className="rounded-md border border-border/70 bg-background/45 px-3 py-4 text-xs text-muted-foreground">
          No recorded spans for this trace.
        </div>
      )}

      {selectedNode && (
        <SpanExpandDetail node={selectedNode} onClose={() => setSelectedId(null)} />
      )}
    </div>
  );
}

function FlowNode({
  canvasNode,
  maxDuration,
  isSelected,
  onSelect,
}: {
  canvasNode: CanvasNode;
  maxDuration: number;
  isSelected: boolean;
  onSelect: () => void;
}) {
  const { span, children } = canvasNode.node;
  const accent = spanAccent(span.span_name, span.status);
  const attrs = parseAttrs(span.attributes);
  const visibleAttrs = attrs.slice(0, 3);
  const hiddenAttrCount = attrs.length - visibleAttrs.length;
  const widthPct = Math.max(7, Math.round((span.duration_ms / maxDuration) * 100));
  const hint = spanHint(span.span_name);

  const style = {
    "--trace-accent": accent,
    left: canvasNode.x,
    top: canvasNode.y,
    width: canvasNode.width,
    height: canvasNode.height,
  } as CSSProperties;

  return (
    <article
      className={cn(
        "trace-flow-node cursor-pointer",
        span.status === "error" && "trace-flow-node-error",
        isSelected && "trace-flow-node-selected",
      )}
      style={style}
      onClick={onSelect}
    >
      <span className="trace-handle trace-handle-in" aria-hidden />
      <span className="trace-handle trace-handle-out" aria-hidden />

      <div className="trace-node-header">
        <div className="min-w-0">
          <div className="trace-node-title">
            <span className="trace-node-dot" aria-hidden />
            <span className="truncate">{spanLabel(span.span_name)}</span>
          </div>
          {hint && <div className="trace-node-hint">{hint}</div>}
        </div>
        <span className="trace-node-time">{fmtMs(span.duration_ms)}</span>
      </div>

      <div className="trace-node-meter">
        <span style={{ width: `${widthPct}%` }} />
      </div>

      <dl className="trace-node-meta">
        {visibleAttrs.map(([k, v]) => (
          <div key={`${k}:${v}`} className="contents">
            <dt>{k}</dt>
            <dd title={v}>{v}</dd>
          </div>
        ))}
        {hiddenAttrCount > 0 && (
          <div className="col-span-2 text-muted-foreground/70">+{hiddenAttrCount} more</div>
        )}
        {visibleAttrs.length === 0 && (
          <div className="col-span-2 text-muted-foreground/55">No attrs</div>
        )}
      </dl>

      {children.length > 0 && <span className="trace-node-child-count">{children.length}</span>}
    </article>
  );
}

function parseAttrsRaw(raw: string): Record<string, unknown> {
  try {
    return JSON.parse(raw) as Record<string, unknown>;
  } catch {
    return {};
  }
}

function SpanExpandDetail({ node, onClose }: { node: SpanNode; onClose: () => void }) {
  const { span, children } = node;
  const allAttrs = parseAttrs(span.attributes);

  const iterChildren = children
    .filter((c) => c.span.span_name.startsWith("boon:tool_loop:iter:"))
    .sort((a, b) => a.span.start_ms - b.span.start_ms);

  const maxIterMs = Math.max(...iterChildren.map((c) => c.span.duration_ms), 1);

  return (
    <div className="mt-3 rounded-md border border-border/50 bg-muted/5 px-3 py-3 text-xs">
      <div className="mb-2.5 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span className="text-[10px] font-semibold uppercase tracking-widest text-muted-foreground">
            {spanLabel(span.span_name)}
            {spanHint(span.span_name) && (
              <span className="ml-1 font-normal normal-case opacity-60">
                — {spanHint(span.span_name)}
              </span>
            )}
          </span>
          <span className="font-mono text-[10px] tabular-nums text-muted-foreground/70">
            {fmtMs(span.duration_ms)}
          </span>
        </div>
        <button
          onClick={onClose}
          className="text-[10px] text-muted-foreground/50 hover:text-muted-foreground"
        >
          ✕
        </button>
      </div>

      {allAttrs.length > 0 && (
        <dl className="mb-3 grid grid-cols-2 gap-x-6 gap-y-1 sm:grid-cols-3 lg:grid-cols-4">
          {allAttrs.map(([k, v]) => (
            <div key={k} className="flex min-w-0 flex-col gap-0.5">
              <dt className="text-[10px] text-muted-foreground">{k}</dt>
              <dd className="truncate font-mono text-[11px] text-foreground/75" title={v}>
                {v}
              </dd>
            </div>
          ))}
        </dl>
      )}

      {iterChildren.length > 0 && (
        <>
          <div className="mb-2 flex items-center gap-3">
            <span className="text-[10px] font-semibold uppercase tracking-widest text-muted-foreground">
              Iterations
            </span>
            <div className="flex items-center gap-2 text-[10px] text-muted-foreground/60">
              <span className="inline-block h-2 w-3 rounded-sm bg-amber-500/60" />
              tool
              <span className="ml-1 inline-block h-2 w-3 rounded-sm bg-blue-500/50" />
              model
            </div>
          </div>
          <div className="space-y-1.5">
            {iterChildren.map((child) => {
              const raw = parseAttrsRaw(child.span.attributes);
              const toolMs = Number(raw.tool_ms ?? 0);
              const modelMs = Number(raw.model_ms ?? 0);
              const tools = Array.isArray(raw.tools)
                ? (raw.tools as string[]).join(", ")
                : String(raw.tools ?? "");
              const total = toolMs + modelMs || 1;
              const toolPct = (toolMs / total) * 100;
              const barWidthPct = (child.span.duration_ms / maxIterMs) * 100;

              return (
                <div key={child.span.span_name} className="flex items-center gap-2">
                  <span className="w-10 shrink-0 text-right text-[10px] text-muted-foreground">
                    {spanLabel(child.span.span_name)}
                  </span>
                  <div className="relative h-3.5 flex-1 overflow-hidden rounded-sm bg-muted/25">
                    <div
                      className="absolute inset-y-0 left-0 flex"
                      style={{ width: `${barWidthPct}%` }}
                    >
                      <div
                        className="h-full bg-amber-500/60"
                        style={{ width: `${toolPct}%` }}
                      />
                      <div className="h-full flex-1 bg-blue-500/45" />
                    </div>
                  </div>
                  <div className="flex w-80 shrink-0 items-center gap-1.5 text-[10px] tabular-nums">
                    <span className="shrink-0 text-amber-400/80">{fmtMs(toolMs)}</span>
                    <span className="shrink-0 text-muted-foreground/40">+</span>
                    <span className="shrink-0 text-blue-400/70">{fmtMs(modelMs)}</span>
                    {tools && (
                      <span
                        className="ml-1 truncate text-muted-foreground/55"
                        title={tools}
                      >
                        {tools}
                      </span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        </>
      )}
    </div>
  );
}

function UsageDetailCard({ row, withTrace }: { row: UsageLogEntry; withTrace?: boolean }) {
  const subtitle = withTrace
    ? null
    : row.has_trace
      ? "- Trace data unavailable"
      : "- Not traced";
  return (
    <div className="border-t border-border/40 bg-muted/10 px-4 py-3">
      <p className="mb-2 text-[10px] font-semibold uppercase tracking-widest text-muted-foreground">
        Request Details
        {subtitle && <span className="ml-2 font-normal normal-case opacity-50">{subtitle}</span>}
      </p>
      <dl className="grid grid-cols-2 gap-x-8 gap-y-1 text-xs sm:grid-cols-3 lg:grid-cols-4">
        <DetailRow label="Request ID" value={row.request_id} mono />
        <DetailRow label="Session" value={row.session_id} mono />
        <DetailRow label="Model" value={row.model} />
        <DetailRow label="Status" value={String(row.status_code)} />
        <DetailRow label="Input tokens" value={row.input_tokens.toLocaleString()} mono />
        <DetailRow label="Output tokens" value={row.output_tokens.toLocaleString()} mono />
        <DetailRow label="Total tokens" value={row.total_tokens.toLocaleString()} mono />
        <DetailRow label="Queue wait" value={fmtMs(row.queue_wait_ms)} mono />
        <DetailRow label="TTFB" value={fmtMs(row.ttft_ms)} mono />
        <DetailRow label="Duration" value={fmtMs(row.total_ms)} mono />
        <DetailRow label="Cache" value={row.cache_status} />
        <DetailRow label="Admission" value={row.admission} />
      </dl>
    </div>
  );
}

function DetailRow({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5">
      <dt className="text-[10px] text-muted-foreground">{label}</dt>
      <dd className={cn("truncate text-[11px]", mono && "font-mono")}>{value || "--"}</dd>
    </div>
  );
}

function fmtMs(ms: number): string {
  if (!ms || ms <= 0) return "--";
  return ms < 1000 ? `${ms}ms` : `${(ms / 1000).toFixed(2)}s`;
}
