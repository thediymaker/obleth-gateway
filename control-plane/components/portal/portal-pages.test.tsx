// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { PortalModels, type PortalModelSummary } from "./portal-models";
import { PortalUsage } from "./portal-usage";
import type { UsageLogEntry } from "@/lib/obleth";

vi.mock("next/navigation", () => ({ useRouter: () => ({ push: vi.fn() }) }));

let root: Root;
let host: HTMLDivElement;
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

const model = (name: string, over: Partial<PortalModelSummary> = {}): PortalModelSummary =>
  ({
    id: name, model_name: name, description: "", model_type: "chat", quantization: "unknown", context_window: 131072, aliases: [], tags: [], boons: [],
    supports_function_calling: true, supports_tool_choice: true, supports_system_messages: true, supports_response_schema: false, supports_vision: false,
    ...over,
  }) as PortalModelSummary;

it("lists the team's models by name and finds one by alias", async () => {
  await act(async () => root.render(<PortalModels models={[model("alpha", { aliases: ["old-alpha"] }), model("beta", { supports_vision: true })]} tenant={null} gatewayBase="https://gw" />));
  const rows = () => [...host.querySelectorAll('table[aria-label="Models"] tbody tr')].map((r) => r.textContent ?? "");
  expect(rows()).toHaveLength(2);
  expect(rows()[0]).toContain("also old-alpha");
  expect(rows()[0]).toContain("131K");
  expect(rows()[1]).toContain("images");

  const search = host.querySelector<HTMLInputElement>('[aria-label="Search models"]')!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(search, "old-");
    search.dispatchEvent(new Event("input", { bubbles: true }));
  });
  expect(rows()).toHaveLength(1);
  expect(rows()[0]).toContain("alpha");
});

it("marks a failed request as needing a look and counts it", async () => {
  const entry = (status: number, id: string): UsageLogEntry =>
    ({ request_id: id, ts_ms: Date.now(), status_code: status, model: "alpha", key_name: "lab", key_prefix: "obl", total_tokens: 10, total_ms: 1500, cost_usd: 0, request_type: "chat" }) as UsageLogEntry;
  await act(async () => root.render(<PortalUsage usage={[]} keyUsage={[]} recent={[entry(200, "aaaaaaaa1"), entry(502, "bbbbbbbb2")]} gatewayBase="https://gw" />));
  const rows = [...host.querySelectorAll('table[aria-label="Newest requests"] tbody tr')].map((r) => r.textContent ?? "");
  expect(rows[0]).toContain("OK · 200");
  expect(rows[1]).toContain("Failed · 502");
  expect(rows[1]).toContain("1.5 s");
  expect(host.textContent).toContain("of the newest 2 requests");
});

it("uses no colour of its own and loads nothing an administrator's page reads", () => {
  for (const file of ["portal-keys.tsx", "portal-models.tsx", "portal-usage.tsx", "ui.tsx"]) {
    const src = readFileSync(join(__dirname, file), "utf8");
    expect(src, file).not.toMatch(/\b(?:text|bg|border)-(?:emerald|sky|amber|red|green|blue|violet|destructive)\b/);
    // Types only from the gateway client, and no admin server actions.
    expect(src, file).not.toMatch(/^import (?!type )[^;]*from "@\/lib\/obleth";/m);
    expect(src, file).not.toMatch(/from "@\/app\/actions"/);
  }
});
