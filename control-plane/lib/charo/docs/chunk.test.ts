import { describe, it, expect } from "vitest";
import { chunkMdx, routeFromPath } from "./chunk";

const SAMPLE = `---
title: "API Keys"
description: "How to mint keys."
---

API keys are the credentials clients use.

<img src="/ui/api-keys.png" alt="screenshot" width={1800} height={1125} />

## How obleth stores keys

obleth never stores the raw secret.

### Creating a key

Click New key on the API Keys page.
`;

describe("routeFromPath", () => {
  it("strips /index.mdx and normalizes separators", () => {
    expect(routeFromPath("guides/api-keys/index.mdx")).toBe("guides/api-keys");
    expect(routeFromPath("guides\\api-keys\\index.mdx")).toBe("guides/api-keys");
    expect(routeFromPath("introduction/index.mdx")).toBe("introduction");
  });
});

describe("chunkMdx", () => {
  const chunks = chunkMdx(SAMPLE, "guides/api-keys");

  it("captures the frontmatter title on every chunk", () => {
    expect(chunks.length).toBeGreaterThan(0);
    expect(chunks.every((c) => c.title === "API Keys")).toBe(true);
  });

  it("makes the pre-heading content an Overview chunk", () => {
    const overview = chunks[0];
    expect(overview.heading).toBe("Overview");
    expect(overview.text).toContain("credentials clients use");
  });

  it("splits on ## and ### headings", () => {
    const headings = chunks.map((c) => c.heading);
    expect(headings).toContain("How obleth stores keys");
    expect(headings).toContain("Creating a key");
  });

  it("strips <img> tags and frontmatter from text", () => {
    const joined = chunks.map((c) => c.text).join("\n");
    expect(joined).not.toContain("<img");
    expect(joined).not.toContain("description:");
  });

  it("derives stable ids from route + slugified heading", () => {
    expect(chunks.find((c) => c.heading === "How obleth stores keys")?.id).toBe(
      "guides/api-keys#how-obleth-stores-keys",
    );
  });

  it("preserves angle-bracket placeholders inside code fences", () => {
    const src = `---\ntitle: "Auth"\n---\n\n## Calling the API\n\n\`\`\`bash\ncurl -H "Authorization: Bearer <OBLETH_ADMIN_TOKEN>" http://x\n\`\`\`\n`;
    const text = chunkMdx(src, "reference/auth").map((c) => c.text).join("\n");
    expect(text).toContain("<OBLETH_ADMIN_TOKEN>");
  });

  it("preserves placeholders inside inline code spans", () => {
    const src = `---\ntitle: "Auth"\n---\n\nUse \`Authorization: Bearer <tenant-key>\` to authenticate.\n`;
    const text = chunkMdx(src, "reference/auth").map((c) => c.text).join("\n");
    expect(text).toContain("<tenant-key>");
  });

  it("does not split on ## lines inside a fenced code block", () => {
    const src = `---\ntitle: "Config"\n---\n\n## Real Heading\n\n\`\`\`yaml\n## not a heading, a yaml comment\nkey: value\n\`\`\`\n`;
    const headings = chunkMdx(src, "guides/config").map((c) => c.heading);
    expect(headings).toEqual(["Real Heading"]);
  });

  it("drops truly empty sections between adjacent headings", () => {
    const src = `---\ntitle: "T"\n---\n\n## A\n## B\n\nbody for B\n`;
    const headings = chunkMdx(src, "x").map((c) => c.heading);
    expect(headings).not.toContain("A");
    expect(headings).toContain("B");
  });

  it("strips <img> even inside code fences but keeps other placeholders", () => {
    const src = `---\ntitle: "MDX"\n---\n\n## Example\n\n\`\`\`mdx\n<img src="/x.png" alt="pic" />\nAuthorization: Bearer <OBLETH_ADMIN_TOKEN>\n\`\`\`\n`;
    const text = chunkMdx(src, "guides/mdx").map((c) => c.text).join("\n");
    expect(text).not.toContain("<img");
    expect(text).toContain("<OBLETH_ADMIN_TOKEN>");
  });

  it("does not corrupt prose containing space-padded numbers", () => {
    const src = `---\ntitle: "T"\n---\n\n## Table\n\n| replicas |  4  |\n\nSet it to  4  by default.\n`;
    const text = chunkMdx(src, "x").map((c) => c.text).join("\n");
    expect(text).toContain("4");
    expect(text).not.toContain("undefined");
  });

  it("tolerates a leading UTF-8 BOM before frontmatter", () => {
    const src = "\uFEFF---\ntitle: \"BOM Doc\"\n---\n\nBody text here.\n";
    const chunks = chunkMdx(src, "x");
    expect(chunks[0].title).toBe("BOM Doc");
    expect(chunks.map((c) => c.text).join("\n")).not.toContain("title:");
  });

  it("preserves placeholders inside ~~~ fenced blocks", () => {
    const src = "---\ntitle: \"T\"\n---\n\n## X\n\n~~~\nAuthorization: Bearer <TOKEN>\n~~~\n";
    const text = chunkMdx(src, "x").map((c) => c.text).join("\n");
    expect(text).toContain("<TOKEN>");
  });

  it("gives colliding headings unique ids within a doc", () => {
    const src = `---\ntitle: "Boons"\n---\n\n## A\n\n### When it applies\n\nfoo\n\n## B\n\n### When it applies\n\nbar\n`;
    const ids = chunkMdx(src, "guides/boons").map((c) => c.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("normalizes CRLF line endings (no stray \\r in chunk text)", () => {
    const src = "---\r\ntitle: \"T\"\r\n---\r\n\r\n## H\r\n\r\nline one\r\nline two\r\n";
    const chunks = chunkMdx(src, "x");
    const text = chunks.map((c) => c.text).join("\n");
    expect(text).not.toContain("\r");
    expect(text).toContain("line one");
    expect(chunks.find((c) => c.heading === "H")).toBeTruthy();
  });
});
