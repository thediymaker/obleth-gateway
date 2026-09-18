import { describe, expect, it } from "vitest";
import { boonBlockers } from "./boon-availability";
import type { BoonSettingsView, KnowledgeSettingsView } from "@/lib/obleth";

const boons = (over: Partial<BoonSettingsView> = {}): BoonSettingsView =>
  ({
    vision_enabled: true,
    vision_fallback_model: "describer",
    structured_output_enabled: true,
    compression_enabled: true,
    image_generation_enabled: true,
    image_generation_model: "flux-2",
    speculation_enabled: true,
    ...over,
  }) as BoonSettingsView;

const knowledge = (enabled: boolean): KnowledgeSettingsView =>
  ({ enabled }) as KnowledgeSettingsView;

describe("boonBlockers", () => {
  it("blocks nothing when every boon is switched on and wired up", () => {
    expect(boonBlockers(boons(), knowledge(true))).toEqual({});
  });

  it("names the switch for a boon that is off", () => {
    const blockers = boonBlockers(boons({ speculation_enabled: false }), knowledge(true));
    expect(blockers.speculation).toContain("Settings → Boons");
    expect(blockers.vision).toBeUndefined();
  });

  it("names the missing helper model for a boon that is on but unwired", () => {
    // The exact shape that made image generation look broken: granted on the
    // model, enabled globally, no image model chosen.
    const blockers = boonBlockers(
      boons({ image_generation_model: "  ", vision_fallback_model: null }),
      knowledge(true),
    );
    expect(blockers.image_generation).toContain("no image model");
    expect(blockers.vision).toContain("no describer model");
  });

  it("reads knowledge from its own settings page", () => {
    expect(boonBlockers(boons(), knowledge(false)).knowledge).toContain("Settings → Knowledge");
  });

  it("fails open when the settings could not be loaded", () => {
    // `safe()` hands us null on an admin-API hiccup; a form that refused every
    // grant would be worse than one that lets the operator through.
    expect(boonBlockers(null, null)).toEqual({});
  });
});
