// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Picker } from "./picker";
import type { RecipeCard, RecipeDeployPreview } from "@/components/recipes/recipe-card";

vi.mock("@/app/actions", () => ({ lookupHfModelAction: vi.fn(async () => ({ ok: false, error: "Couldn't reach huggingface.co from the dashboard." })) }));

const preview = (engine: string): RecipeDeployPreview => ({ apiModelName: "my-model", modelType: "chat", engine, port: 8000, healthPath: "/health", targetReplicas: 1, maxJobFailures: 2, partition: "", scriptBody: "", rawBody: "", warnings: [], inputs: [], kind: "engine" });
const card = (id: string, engine: string, source: "file" | "db", name: string): RecipeCard => ({ id, valid: true, name, engine, description: `${name} for any model`, warnings: [], source, preview: preview(engine) });

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

describe("starting from Hugging Face", () => {
  it("offers every engine recipe, saved copies first, before any lookup", async () => {
    const onPick = vi.fn();
    const recipes = [card("vllm", "vllm", "file", "vLLM"), card("site-vllm", "vllm", "db", "vLLM with our image"), card("ollama", "ollama", "file", "Ollama")];
    await act(async () => root.render(<Picker recipes={recipes} launches={[]} resources={{ partitions: [], accounts: [], qos: [] } as never} tab="hf" onTab={() => {}} picked={null} onPick={onPick} onNext={() => {}} />));
    const section = host.querySelector('section[aria-label="Launch with an engine"]')!;
    expect(section.textContent).toContain("Or launch with an engine and type the model");
    const rows = [...section.querySelectorAll("button[aria-pressed]")].map((b) => b.textContent);
    expect(rows).toEqual(["vLLMvLLM with our image, saved on this gateway", "vLLMvLLM for any model", "OllamaOllama for any model"]);
    await act(async () => (section.querySelectorAll("button[aria-pressed]")[0] as HTMLButtonElement).click());
    expect(onPick).toHaveBeenCalledWith({ recipeId: "site-vllm" });
  });
});
