import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createModelAction, listUpstreamModelsAction } from "@/app/actions";
import { AddModelSheet, guessModelType, suggestName } from "./add-model";
import type { ModelRoute } from "@/lib/obleth";

vi.mock("@/app/actions", () => ({
  applyModelManifestAction: vi.fn(),
  createModelAction: vi.fn(async () => ({ ok: true, model: { id: "n", name: "qwen3-6-coder" }, enabled: false, check: "HTTP 404" })),
  listUpstreamModelsAction: vi.fn(async () => ({ ok: true, base: "http://x/v1", models: [{ id: "Qwen/Qwen3.6-Coder" }, { id: "BAAI/bge-m3" }] })),
  activateModelsAction: vi.fn(),
}));
vi.mock("@/components/provider-import-wizard", () => ({ ProviderImportWizard: () => null }));
const push = vi.fn();
vi.mock("next/navigation", () => ({ useRouter: () => ({ refresh: vi.fn(), push }) }));

let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  vi.mocked(createModelAction).mockClear();
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

function type(el: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  setter.call(el, value);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}
const input = (label: string) => document.querySelector<HTMLInputElement>(`#field-${label}`)!;
const button = (text: string) => [...document.querySelectorAll("button")].find((b) => b.textContent === text)!;

async function open(models: Partial<ModelRoute>[] = []) {
  await act(async () => {
    root.render(<AddModelSheet mode="connect" onModeChange={() => {}} onClose={() => {}} slurmEnabled={false} models={models as ModelRoute[]} />);
  });
}

describe("guessing from the upstream name", () => {
  it.each([
    ["BAAI/bge-m3", "embedding"],
    ["openai/whisper-large-v3", "audio_transcription"],
    ["hexgrad/Kokoro-82M-tts", "audio_speech"],
    ["black-forest-labs/FLUX.2-dev", "image"],
    ["Wan-AI/Wan2.2-T2V", "video"],
    ["Qwen/Qwen3-235B-A22B", "chat"],
  ])("reads %s as %s", (upstream, type) => {
    expect(guessModelType(upstream)).toBe(type);
  });

  it("suggests the last path segment as the name", () => {
    expect(suggestName("Qwen/Qwen3.6-Coder_480B")).toBe("qwen3.6-coder-480b");
  });
});

describe("adding a model from an endpoint", () => {
  it("lists what the endpoint serves, and picking one fills in the name and type", async () => {
    await open();
    await act(async () => type(input("api_base"), "http://x/v1"));
    await act(async () => button("Check connection").click());
    expect(listUpstreamModelsAction).toHaveBeenCalledWith({ apiBase: "http://x/v1", apiKey: undefined });
    const options = [...document.querySelectorAll('[role="radio"]')];
    expect(options.map((o) => o.textContent)).toEqual(["Qwen/Qwen3.6-Coder", "BAAI/bge-m3"]);
    await act(async () => (options[1] as HTMLButtonElement).click());
    expect(input("model_name_input").value).toBe("bge-m3");
    expect(document.querySelector<HTMLInputElement>('input[type="hidden"][name="model_type"]')!.value).toBe("embedding");
  });

  it("refuses a name that is already a model or an alias", async () => {
    await open([{ model_name: "taken", aliases: ["also-taken"] }]);
    await act(async () => type(input("model_name_input"), "also-taken"));
    expect(document.body.textContent).toContain("Already a model or alias.");
  });

  it("creates with the price per token, then opens the new model's page", async () => {
    await open();
    await act(async () => type(input("api_base"), "http://x/v1"));
    await act(async () => type(input("upstream_display"), "Qwen/Qwen3.6-Coder"));
    await act(async () => type(input("input_cost_per_token_input"), "0.4"));
    await act(async () => type(input("output_cost_per_token_input"), "1.6"));
    await act(async () => button("Create model").click());
    const data = vi.mocked(createModelAction).mock.calls[0][0] as FormData;
    expect(data.get("model_name")).toBe("qwen3.6-coder");
    expect(data.get("upstream_model")).toBe("Qwen/Qwen3.6-Coder");
    expect(data.get("model_type")).toBe("chat");
    expect(data.get("input_cost_per_token")).toBe("0.0000004");
    expect(data.get("output_cost_per_token")).toBe("0.0000016");
    expect(data.get("auto_eligible")).toBe("on");
    expect(push).toHaveBeenCalledWith("/models/qwen3-6-coder");
  });

  it("stays open to add another, saying the last one is waiting for a check", async () => {
    await open();
    await act(async () => type(input("api_base"), "http://x/v1"));
    await act(async () => type(input("upstream_display"), "Qwen/Qwen3.6-Coder"));
    await act(async () => button("Create and add another").click());
    expect(document.body.textContent).toContain("was created switched off");
    expect(document.body.textContent).toContain("First check: HTTP 404");
    expect(input("upstream_display").value).toBe("");
  });
});
