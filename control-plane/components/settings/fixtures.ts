// Test fixtures shaped like prod's settings (2026-09-28).
import type { AlertSettingsView, AutoRouterSettingsView, BoonSettingsView, CharoSettingsView, EnergySettingsView, KnowledgeSettingsView, ModelRoute, RouterReadinessView, SlurmSettingsView } from "@/lib/obleth";
import type { SettingsData } from "./settings-page";

export const alerts = (over: Partial<AlertSettingsView> = {}): AlertSettingsView => ({ slack_webhook_set: false, min_interval_secs: 300, email: null, ...over });

export const router = (over: Partial<AutoRouterSettingsView> = {}): AutoRouterSettingsView => ({
  classifier_enabled: true, classifier_model: "router-classifier-v2", classifier_timeout_ms: 2500, available_tags: ["coding", "math"],
  capacity_weight: 0.6, cost_weight: 0.4, tag_weight: 0.5, default_soft_cap: 8, temperature: 0, difficulty_enabled: true, tier_source: "hybrid",
  messages_default_model: "glm-5-3-flash", ...over,
}) as AutoRouterSettingsView;

export const boons = (over: Partial<BoonSettingsView> = {}): BoonSettingsView => ({
  vision_enabled: false, vision_fallback_model: null, vision_describe_prompt: "Describe it.", vision_max_images: 6, vision_timeout_ms: 30000,
  structured_output_enabled: false, structured_output_fixer_model: null, structured_output_max_repair_attempts: 1, structured_output_timeout_ms: 30000,
  tool_loop_enabled: false, tool_loop_max_turns: 4, tool_loop_tool_timeout_ms: 30000, tool_loop_deadline_secs: 300, tool_loop_nudge: "Use tools.",
  compression_enabled: false, compression_min_tokens: 128, compression_max_segments: 64, compression_original_ttl_secs: 3600, compression_max_lossy_segments: 4,
  compression_code_compaction: false, compression_dedup: false, compression_compact_logs: false, compression_allow_lossy: false, compression_neural_keep_ratio: 0.5,
  image_generation_enabled: true, image_generation_model: "flux-2", image_generation_tool_description: "Draw.", image_generation_allowed_sizes: ["512x512", "1024x1024"],
  image_generation_max_images_per_request: 2, image_generation_timeout_ms: 120000,
  speculation_enabled: true, speculation_draft_model: "north-mini-code", speculation_verify_model: null, speculation_classify_model: "olmo3-7b-instruct",
  speculation_agree_min: 0.5, speculation_lp_min: -1, speculation_abort_agree: 0.45, speculation_abort_lp: -1.6, speculation_first_chunk_tokens: 80,
  speculation_chunk_tokens: 250, speculation_decide_by_tokens: 450, speculation_max_draft_tokens: 2048, speculation_pace_ms: 9, speculation_timeout_ms: 45000,
  speculation_draft_chat_template_kwargs: { reasoning: false },
  speculation_category_gates: [{ tag: "coding", speculate: true, agree_min: 0.4, lp_min: -1 }],
  speculation_unlisted_categories_speculate: false, speculation_verify_url_template: null,
  web_search_enabled: false, web_search_tool: null, web_search_tool_description: "Search.", web_search_max_results: 5,
  web_search_max_searches_per_request: 3, web_search_timeout_ms: 15000,
  ...over,
}) as BoonSettingsView;

export const model = (name: string, over: Partial<ModelRoute> = {}): ModelRoute => ({ id: name, model_name: name, model_type: "chat", enabled: true, boons: [], tool_servers: [], ...over }) as unknown as ModelRoute;

export const readiness = (over: Partial<RouterReadinessView> = {}): RouterReadinessView => ({ findings: [], pool_size: 23, classifier_active: true, difficulty_enabled: true, ...over });

export function settingsData(over: Partial<SettingsData> = {}): SettingsData {
  return {
    alerts: alerts(),
    router: router(),
    readiness: readiness(),
    boons: boons(),
    compressor: null,
    knowledge: { enabled: false } as KnowledgeSettingsView,
    energy: { enabled: false, prometheus_url: "", power_query: "", poll_interval_secs: 60, energy_cost_per_kwh: 0, carbon_g_per_kwh: 0, pue: 1 } as EnergySettingsView,
    charo: { enabled: true, brain_model: "north-mini-code", tools_enabled: { run_benchmark: true }, bench_max_concurrency: 40, bench_max_duration_s: 120, bench_max_requests: 500 } as CharoSettingsView,
    retention: { days: 180, configured: false },
    slurm: { enabled: false, provisioner_running: true } as SlurmSettingsView,
    models: [model("glm-5-3", { boons: ["vision", "image_generation", "speculation"] }), model("north-mini-code"), model("flux-2", { model_type: "image" } as Partial<ModelRoute>)],
    version: { gateway: { version: "0.9.0", git_sha: "abcdef1234", built_at: null }, controlPlane: { version: "0.9.0", sha: null }, latest: null, updateAvailable: false },
    ...over,
  };
}
