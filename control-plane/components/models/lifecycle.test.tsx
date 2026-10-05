import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { setModelStatusAction } from "@/app/actions";
import { LifecycleSection, replacementOptions, type LifecycleCandidate } from "./lifecycle";
import type { ModelRoute } from "@/lib/obleth";

vi.mock("@/app/actions", () => ({ setModelStatusAction: vi.fn(async () => ({ ok: true })) }));

const base = { id: "u", model_name: "glm-4-5v", model_type: "chat", lifecycle: { status: "active" } } as unknown as ModelRoute;

const models: LifecycleCandidate[] = [
  { name: "glm-4-5v", type: "chat", status: "active" },
  { name: "gemma4-31b-it", type: "chat", status: "active" },
  { name: "qwen3-vl-32b-instruct", type: "chat", status: "deprecated" },
  { name: "llama2-70b", type: "chat", status: "retired" },
  { name: "qwen3-embedding-8b", type: "embedding", status: "active" },
];

let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  vi.mocked(setModelStatusAction).mockClear();
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

async function render(model: ModelRoute = base, callers: Parameters<typeof LifecycleSection>[0]["callers"] = []) {
  const onChanged = vi.fn();
  await act(async () => {
    root.render(<LifecycleSection model={model} models={models} callers={callers} onChanged={onChanged} />);
  });
  return onChanged;
}

const radio = (value: string) => host.querySelector<HTMLInputElement>(`input[name="lifecycle_status"][value="${value}"]`)!;
const saveButton = () => [...host.querySelectorAll("button")].find((b) => b.textContent?.includes("Save lifecycle"))!;

describe("replacementOptions", () => {
  it("offers same-type models that are still served, never the model itself", () => {
    expect(replacementOptions(base, models)).toEqual([
      { value: "gemma4-31b-it", label: "gemma4-31b-it" },
      { value: "qwen3-vl-32b-instruct", label: "qwen3-vl-32b-instruct (deprecated)" },
    ]);
  });
});

describe("LifecycleSection", () => {
  it("shows only the status for an active model, and saves nothing until it changes", async () => {
    await render();
    expect(radio("active").checked).toBe(true);
    expect(host.textContent).not.toContain("Replacement");
    expect(saveButton().disabled).toBe(true);
  });

  it("deprecates with the fields a caller sees", async () => {
    const onChanged = await render();
    await act(async () => radio("deprecated").click());
    expect(host.textContent).toContain("Replacement");
    expect(host.textContent).toContain("x-obleth-model-status: deprecated");
    const date = host.querySelector<HTMLInputElement>('input[type="date"]')!;
    await act(async () => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      setter.call(date, "2099-10-19");
      date.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(host.textContent).toContain("Sunset: Oct 19, 2099");
    await act(async () => saveButton().click());
    expect(setModelStatusAction).toHaveBeenCalledWith("u", {
      status: "deprecated",
      replacement: null,
      retire_at: "2099-10-19T00:00:00Z",
      note: "",
      redirect: false,
    });
    expect(onChanged).toHaveBeenCalled();
  });

  it("previews the 410 a retired model answers with", async () => {
    const retired = {
      ...base,
      lifecycle: { status: "retired", replacement: "gemma4-31b-it", changed_at: "2026-10-19T00:00:00Z", note: "Ask rc." },
      effective_status: "retired",
    } as unknown as ModelRoute;
    await render(retired);
    expect(host.textContent).toContain("410 · The model `glm-4-5v` was retired on 2026-10-19. Use `gemma4-31b-it` instead. Ask rc.");
  });

  it("lists who called it, and says so when nobody did", async () => {
    await render(base, [{ value: "k1", label: "ssaeidi1-ledger-agent", requests: 1263, errors: 0 }]);
    expect(host.textContent).toContain("ssaeidi1-ledger-agent");
    expect(host.textContent).toContain("1,263 requests");
    expect(host.querySelector('a[href*="key=k1"]')).not.toBeNull();
    await render(base, []);
    expect(host.textContent).toContain("No requests in the last 30 days.");
  });
});
