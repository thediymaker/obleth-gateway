import type { BoonSettingsView, KnowledgeSettingsView } from "@/lib/obleth";

// Why a boon can't take effect, keyed by the boon's vocabulary value. A boon
// missing from the map is ready to grant.
//
// Every boon has a global switch (and some a helper model) that the data plane
// checks before it does anything: `image_gen_eligible` and its siblings in
// obleth-proxy `boons/mod.rs` all require the model's grant AND
// `settings.<boon>.active()`. When the global side is off the grant is inert
// and the gateway logs nothing, so the model just behaves as if the boon were
// never granted — a chat model says it cannot draw, a text-only model says it
// cannot see. Surfacing the reason next to the grant is what keeps that from
// reading as a gateway bug.
export type BoonBlockers = Record<string, string>;

const OFF_IN_BOONS = "it is switched off in Settings → Boons.";

/** True when a helper-model setting is absent or blank. */
function unset(value: string | null | undefined): boolean {
  return !value || value.trim() === "";
}

/**
 * Which boons cannot currently be granted, and why. Mirrors each
 * `active()` in obleth-config `types.rs`: every boon needs its global switch,
 * and `vision` and `image_generation` additionally need a helper model.
 *
 * Both settings objects are optional so the caller can pass whatever `safe()`
 * returned: an admin-API hiccup yields no blockers rather than a form that
 * refuses every grant.
 */
export function boonBlockers(
  boons?: BoonSettingsView | null,
  knowledge?: KnowledgeSettingsView | null,
): BoonBlockers {
  const blockers: BoonBlockers = {};
  if (boons) {
    if (!boons.vision_enabled) blockers.vision = OFF_IN_BOONS;
    else if (unset(boons.vision_fallback_model))
      blockers.vision = "no describer model is set in Settings → Boons.";
    if (!boons.structured_output_enabled) blockers.structured_output = OFF_IN_BOONS;
    if (!boons.compression_enabled) blockers.compression = OFF_IN_BOONS;
    if (!boons.image_generation_enabled) blockers.image_generation = OFF_IN_BOONS;
    else if (unset(boons.image_generation_model))
      blockers.image_generation = "no image model is set in Settings → Boons.";
    if (!boons.speculation_enabled) blockers.speculation = OFF_IN_BOONS;
  }
  // Knowledge is the one boon configured on its own page, not the Boons tab.
  if (knowledge && !knowledge.enabled) {
    blockers.knowledge = "retrieval is switched off in Settings → Knowledge.";
  }
  return blockers;
}
