import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ChartTooltip, TooltipRow, TooltipShell } from "./chart-tooltip";
import { seriesCurrent, seriesPrevious, seriesRest, seriesTones } from "@/lib/chart-style";

describe("chart tooltips", () => {
  it("frames a title and rows in one shell", () => {
    const html = renderToStaticMarkup(
      <TooltipShell title="10:00–10:15">
        <TooltipRow label="This window" value="1.2K" />
        <TooltipRow muted label="Yesterday" value="980" swatch={seriesPrevious} />
      </TooltipShell>,
    );
    expect(html).toMatchInlineSnapshot(`"<div class="min-w-44 rounded-lg border border-border bg-chart-tooltip px-3 py-2 text-xs shadow-xl"><p class="mb-1.5 font-mono text-muted-foreground">10:00–10:15</p><div class="flex items-center justify-between gap-6"><span class="inline-flex items-center gap-1.5">This window</span><span class="font-mono tabular-nums">1.2K</span></div><div class="flex items-center justify-between gap-6 text-muted-foreground"><span class="inline-flex items-center gap-1.5"><span class="h-2 w-2 shrink-0 rounded-[2px]" style="background:hsl(var(--chart-previous))"></span>Yesterday</span><span class="font-mono tabular-nums">980</span></div></div>"`);
  });

  it("leaves the title out when there is none", () => {
    const html = renderToStaticMarkup(<TooltipShell><TooltipRow label="Running" value="3" /></TooltipShell>);
    expect(html).not.toContain("<p");
  });

  it("the generic tooltip writes each series with its swatch and formatter", () => {
    const html = renderToStaticMarkup(<ChartTooltip active label="8 users" payload={[{ name: "p50", value: 1500, color: seriesCurrent, dataKey: "p50" }]} valueFormatter={(v) => `${v} ms`} />);
    expect(html).toContain("8 users");
    expect(html).toContain("1500 ms");
    expect(html).toContain("background:hsl(var(--chart-1))");
    expect(renderToStaticMarkup(<ChartTooltip active={false} payload={[]} />)).toBe("");
  });

  it("series colours come from the --chart-* tokens", () => {
    for (const c of [seriesCurrent, seriesPrevious, seriesRest, ...seriesTones]) expect(c).toMatch(/^hsl\(var\(--chart-[a-z0-9]+\)\)$/);
  });
});
