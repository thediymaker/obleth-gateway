// Muted categorical palette for the live dashboards. Known fairshare groups get
// stable colors; everything else cycles the palette by index. Reports keeps its
// own brighter palette on purpose — don't merge them.
export const PALETTE = [
  "hsl(210 8% 70%)",
  "hsl(205 13% 58%)",
  "hsl(165 11% 56%)",
  "hsl(235 8% 60%)",
  "hsl(35 12% 58%)",
  "hsl(190 9% 56%)",
  "hsl(260 8% 62%)",
];

export const GROUP_PALETTE: Record<string, string> = {
  chatbot: "hsl(160 13% 58%)",
  api: "hsl(205 13% 62%)",
  analytics: "hsl(35 13% 58%)",
  batch: "hsl(260 9% 62%)",
  default: "hsl(240 6% 62%)",
};

/** Catch-all bucket ("others") in top-N charts. */
export const OTHERS_COLOR = "hsl(240 6% 42%)";

export function colorForGroup(name: string, index = 0): string {
  return GROUP_PALETTE[name] ?? PALETTE[index % PALETTE.length];
}
