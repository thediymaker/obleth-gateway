import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { StatusDot, StatusPill, StatusText } from "./status";
import { HealthGlyph } from "@/components/overview/ui";
import { StatusMark } from "@/components/models/ui";
import { StateMark } from "@/components/deployments/ui";
import { tenantGlyph } from "@/components/access/ui";
import { STATE_LABEL, glyphOf, replicaGlyph, watchedState, type DeployState } from "@/lib/deployments-model";
import { STATUS_LABELS, checkStatusLabel, type ModelStatus } from "@/lib/models-model";

const glyphOfHtml = (html: string) => /data-glyph="(\w+)"/.exec(html)?.[1];

describe("one status vocabulary", () => {
  it.each([
    ["on", "bg-secondary-foreground"],
    ["half", "linear-gradient"],
    ["none", "border-muted-foreground"],
    ["off", "border-dashed"],
  ] as const)("a %s dot draws its own shape", (glyph, cls) => {
    const html = renderToStaticMarkup(<StatusDot glyph={glyph} label="x" />);
    expect(glyphOfHtml(html)).toBe(glyph);
    expect(html).toContain(cls);
    expect(html).toContain('role="img"');
  });

  it("a status that needs someone is the inverted pill, never a dot", () => {
    expect(glyphOfHtml(renderToStaticMarkup(<StatusPill glyph="on" attention>Not answering</StatusPill>))).toBe("attention");
    expect(glyphOfHtml(renderToStaticMarkup(<StatusText glyph="on" attention>Failed</StatusText>))).toBe("attention");
  });

  it.each([
    ["serving", "on", "Serving"],
    ["unchecked", "none", "Not checked yet"],
    ["maintenance", "off", "Maintenance"],
    ["off", "off", "Off"],
    ["down", "attention", "DOWN"],
  ] as [ModelStatus, string, string][])("a model that is %s reads %s and %s", (status, glyph, word) => {
    const html = renderToStaticMarkup(<StatusMark status={status} />);
    expect(glyphOfHtml(html)).toBe(glyph);
    expect(html).toContain(word);
  });

  it.each([
    ["healthy", "on"],
    ["unknown", "none"],
    ["maintenance", "off"],
    ["unhealthy", "attention"],
  ] as const)("health %s draws %s", (state, glyph) => {
    expect(glyphOfHtml(renderToStaticMarkup(<HealthGlyph state={state} />))).toBe(glyph);
  });

  it.each(Object.keys(STATE_LABEL) as DeployState[])("deployment state %s shows its word and mark", (state) => {
    const html = renderToStaticMarkup(<StateMark state={state} />).replace(/&#x27;/g, "'");
    expect(html).toContain(STATE_LABEL[state]);
    expect(glyphOfHtml(html)).toBe(glyphOf(state));
  });

  it("a model turned off reads Off on Models and on Deployments alike", () => {
    const view = watchedState(undefined, { enabled: false });
    expect(view.state).toBe("off");
    expect(STATE_LABEL[view.state]).toBe(STATUS_LABELS.off);
    expect(checkStatusLabel("disabled")).toBe("Off");
    expect(checkStatusLabel("unhealthy")).toBe("Failing");
  });

  it("dashed means deliberately not serving; ring means no data", () => {
    expect(glyphOf("off")).toBe("off");
    expect(glyphOf("stale")).toBe("none");
    expect(tenantGlyph("suspended")).toBe("off");
    expect(tenantGlyph("archived")).toBe("off");
    expect(tenantGlyph("active")).toBe("on");
    expect(replicaGlyph("queued")).toBe("half");
    expect(replicaGlyph("lost")).toBe("off");
  });
});

describe("status is monochrome", () => {
  const root = join(__dirname, "..", "..");
  const colour = /\b(?:text|bg|border|ring|fill|stroke)-(?:green|red|emerald|amber|yellow|violet|purple|sky|blue|rose|orange|lime|teal|cyan|indigo|fuchsia|pink)-\d|\bdark:/;
  const files = [
    "components/ui/status.tsx",
    "components/overview/ui.tsx",
    "components/models/ui.tsx",
    "components/deployments/ui.tsx",
    "components/access/ui.tsx",
    "components/access/key-access-view.tsx",
    "components/batches/batches-view.tsx",
    "components/mcp/server-sheet.tsx",
    "components/knowledge/knowledge-list.tsx",
    "components/models/endpoints.tsx",
    ...readdirSync(join(root, "components/playground")).filter((f) => f.endsWith(".tsx") && !f.includes(".test.")).map((f) => `components/playground/${f}`),
    ...readdirSync(join(root, "components/charo")).filter((f) => f.endsWith(".tsx") && !f.includes(".test.") && f !== "rail.tsx").map((f) => `components/charo/${f}`),
    ...readdirSync(join(root, "components/charo/results")).filter((f) => f.endsWith(".tsx") && !f.includes(".test.")).map((f) => `components/charo/results/${f}`),
  ];

  it.each(files)("%s uses no colour classes", (file) => {
    expect(readFileSync(join(root, file), "utf8")).not.toMatch(colour);
  });

  it("violet is kept to the assistant's own messages", () => {
    expect(readFileSync(join(root, "components/charo/rail.tsx"), "utf8")).toMatch(/violet/);
  });
});
