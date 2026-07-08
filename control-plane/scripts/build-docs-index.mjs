// Builds the checked-in docs search index from the sibling obleth-docs checkout.
// Run: npm run build:docs-index   (regenerate + commit when docs change)
import { readdir, readFile, writeFile } from "node:fs/promises";
import { join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { chunkMdx, routeFromPath } from "../lib/charo/docs/chunk.ts";

const here = fileURLToPath(new URL(".", import.meta.url));
const DOCS_REPO = process.env.DOCS_REPO
  ? resolve(process.env.DOCS_REPO)
  : resolve(here, "../../../obleth-docs");
const DOCS_ROOT = join(DOCS_REPO, "contents", "docs");
const OUT = resolve(here, "../lib/charo/docs/index.json");

async function walk(dir) {
  const out = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...(await walk(full)));
    else if (entry.name.endsWith(".mdx")) out.push(full);
  }
  return out;
}

const files = await walk(DOCS_ROOT);
const chunks = [];
for (const file of files) {
  const rel = relative(DOCS_ROOT, file).split(sep).join("/");
  const route = routeFromPath(rel);
  const source = await readFile(file, "utf8");
  for (const c of chunkMdx(source, route)) {
    // Cap chunk text so the index and grounding payloads stay lean.
    chunks.push({ ...c, text: c.text.slice(0, 1500) });
  }
}
chunks.sort((a, b) => a.id.localeCompare(b.id));

const index = { generatedAt: new Date().toISOString(), chunks };
await writeFile(OUT, JSON.stringify(index, null, 2) + "\n");
console.log(`Wrote ${chunks.length} chunks from ${files.length} files -> ${OUT}`);
