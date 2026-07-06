interface Frag { name: string; arguments: string }

export class ToolCallAccumulator {
  private byIndex = new Map<number, Frag>();

  addDelta(deltaToolCalls: unknown): void {
    if (!Array.isArray(deltaToolCalls)) return;
    for (const tc of deltaToolCalls) {
      if (!tc || typeof tc !== "object") continue;
      const idx = typeof (tc as { index?: unknown }).index === "number" ? (tc as { index: number }).index : 0;
      const fn = (tc as { function?: { name?: unknown; arguments?: unknown } }).function ?? {};
      const cur = this.byIndex.get(idx) ?? { name: "", arguments: "" };
      if (typeof fn.name === "string") cur.name += fn.name;
      if (typeof fn.arguments === "string") cur.arguments += fn.arguments;
      this.byIndex.set(idx, cur);
    }
  }

  complete(): Frag[] {
    return [...this.byIndex.entries()].sort((a, b) => a[0] - b[0]).map(([, v]) => v);
  }
}
