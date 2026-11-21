// Chart colours and hover styling shared by every chart. All of them read the
// --chart-* tokens in app/globals.css, so a theme change reaches every chart.

/** The current window or the only series. */
export const seriesCurrent = "hsl(var(--chart-1))";
/** The comparison window, drawn dashed behind the current one. */
export const seriesPrevious = "hsl(var(--chart-previous))";
/** Ranked series, brightest first; anything past them is seriesRest. */
export const seriesTones = ["hsl(var(--chart-1))", "hsl(var(--chart-2))", "hsl(var(--chart-3))", "hsl(var(--chart-4))"] as const;
export const seriesRest = "hsl(var(--chart-rest))";
/** The ring around a hovered point: the card's own colour, so the dot reads as cut out. */
export const activeDot = { r: 4, strokeWidth: 2, stroke: "hsl(var(--card))" } as const;

export const axisTick = { fill: "hsl(var(--chart-axis))", fontSize: 11 } as const;
export const chartGrid = { stroke: "hsl(var(--chart-grid))", strokeDasharray: "3 3" } as const;
/** Subtle vertical guide for time-series; never a bright fill highlight. */
export const timeCursor = { stroke: "hsl(var(--chart-cursor))", strokeWidth: 1 } as const;
/** The band behind a hovered bar. */
export const barCursor = { fill: "hsl(var(--chart-grid) / 0.5)" } as const;
