// @vitest-environment jsdom
import { describe, expect, it, afterEach } from "vitest";
import { createRoot, type Root } from "react-dom/client";
import { act } from "react";
import { RouterReadinessCard } from "./router-readiness-card";
import type { RouterReadinessView } from "@/lib/obleth";

let host: HTMLDivElement;
let root: Root | null = null;

afterEach(() => {
  act(() => root?.unmount());
  root = null;
  host?.remove();
});

async function render(readiness: RouterReadinessView | null) {
  host = document.createElement("div");
  document.body.appendChild(host);
  await act(async () => {
    root = createRoot(host);
    root.render(<RouterReadinessCard readiness={readiness} />);
  });
}

const report = (over: Partial<RouterReadinessView> = {}): RouterReadinessView => ({
  findings: [],
  pool_size: 3,
  classifier_active: true,
  difficulty_enabled: true,
  ...over,
});

describe("RouterReadinessCard", () => {
  it("renders nothing at all without a report (failed fetch)", async () => {
    await render(null);
    expect(host.textContent).toBe("");
  });

  it("shows the all-clear when there are no findings", async () => {
    await render(report());
    expect(host.textContent).toContain("Nothing to flag");
    expect(host.textContent).toContain("3 models in the auto pool");
  });

  it("orders warnings before info and renders both title and detail", async () => {
    await render(
      report({
        findings: [
          {
            severity: "info",
            code: "tiering_off",
            title: "Difficulty tiering is off",
            detail: "Request difficulty is classified but unused.",
            models: [],
          },
          {
            severity: "warn",
            code: "uncovered_tag",
            title: "No auto-eligible model carries the `math` tag",
            detail: "Requests about math route on price alone.",
            models: [],
          },
        ],
      }),
    );
    const items = Array.from(host.querySelectorAll("li")).map((li) => li.textContent ?? "");
    expect(items).toHaveLength(2);
    expect(items[0]).toContain("`math`");
    expect(items[1]).toContain("tiering is off");
    expect(host.textContent).toContain("route on price alone");
  });
});
