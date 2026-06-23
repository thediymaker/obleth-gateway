import { describe, it, expect } from "vitest";
import { parseRecipe, defaultHealthPath } from "./sbatch-recipes";

const BODY = [
  "#!/bin/bash -l",
  "#SBATCH --gres=gpu:gh200:1   # one GH200",
  "#SBATCH --cpus-per-task=72",
  "#SBATCH -p arm",
  "module load cuda",
  "llama-server -hf unsloth/GLM-5.2-GGUF:UD-IQ2_M --host 0.0.0.0 --port 8000",
].join("\n");

function file(header: string, body = BODY): string {
  return `---\n${header}\n---\n${body}`;
}

describe("parseRecipe", () => {
  it("splits header from body and lifts directives", () => {
    const r = parseRecipe(
      "glm",
      file(
        [
          "name: GLM-5.2",
          "engine: llamacpp",
          "model_type: chat",
          "api_model_name: glm-5.2",
          "port: 8000",
        ].join("\n"),
      ),
    );
    expect(r.valid).toBe(true);
    expect(r.id).toBe("glm");
    expect(r.header?.api_model_name).toBe("glm-5.2");
    expect(r.body).toContain("llama-server -hf");
    expect(r.directives?.gres).toBe("gpu:gh200:1");
    expect(r.directives?.partition).toBe("arm");
  });

  it("applies default target_replicas, max_job_failures, and health_path", () => {
    const r = parseRecipe(
      "x",
      file(
        ["name: X", "engine: llamacpp", "model_type: chat", "api_model_name: x", "port: 8000"].join("\n"),
      ),
    );
    expect(r.header?.target_replicas).toBe(2);
    expect(r.header?.max_job_failures).toBe(3);
    expect(defaultHealthPath("ollama")).toBe("/");
    expect(defaultHealthPath("vllm")).toBe("/health");
    expect(defaultHealthPath("llamacpp")).toBe("/health");
  });

  it("lets a header placement field override the parsed directive", () => {
    const r = parseRecipe(
      "x",
      file(
        [
          "name: X",
          "engine: llamacpp",
          "model_type: chat",
          "api_model_name: x",
          "port: 8000",
          "partition: gpu-big", // overrides `#SBATCH -p arm`
        ].join("\n"),
      ),
    );
    // header value, not the parsed "arm"
    expect(r.header?.partition).toBe("gpu-big");
    expect(r.directives?.partition).toBe("arm");
  });

  it("is invalid when a required field is missing", () => {
    const r = parseRecipe(
      "x",
      file(["name: X", "engine: llamacpp", "port: 8000"].join("\n")), // no model_type / api_model_name
    );
    expect(r.valid).toBe(false);
    expect(r.error).toBeTruthy();
  });

  it("is invalid when there is no body", () => {
    const r = parseRecipe(
      "x",
      "---\nname: X\nengine: llamacpp\nmodel_type: chat\napi_model_name: x\nport: 8000\n---\n",
    );
    expect(r.valid).toBe(false);
    expect(r.error).toMatch(/body/i);
  });

  it("is invalid when the frontmatter is malformed (no closing ---)", () => {
    const r = parseRecipe("x", "name: X\nengine: llamacpp");
    expect(r.valid).toBe(false);
    expect(r.warnings).toEqual([]);
  });
});
