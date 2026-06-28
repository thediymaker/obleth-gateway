//! ASCII art + decorative helpers for the obench TUI.
//!
//! The obleth emblem is rendered straight from the canonical brand PNG
//! (`obench/assets/obleth.png`) at runtime. The logo art is a *filled*
//! silhouette derived from the image, downsampled to whatever character grid
//! the panel can afford — so it always looks right and rescales cleanly as the
//! terminal window grows or shrinks, with no hand-tuned ASCII to maintain.

use std::sync::OnceLock;

use ratatui::prelude::*;

use crate::tui::theme;

/// The canonical brand logo, embedded so the binary is self-contained.
const LOGO_PNG: &[u8] = include_bytes!("../../assets/obleth.png");

/// A cropped, filled silhouette of the logo: `pix[y * w + x]` is `true` where
/// the mark is "ink" (the pillars), `false` for the surrounding background.
struct Silhouette {
    w: usize,
    h: usize,
    pix: Vec<bool>,
}

/// Decode the PNG once and derive a filled silhouette mask. The logo is a black
/// *outline* enclosing white interiors, so a plain luminance threshold would
/// only capture the strokes. Instead we flood-fill the exterior background and
/// treat everything it cannot reach (outline + enclosed interior) as ink, then
/// crop to the mark's bounding box.
fn silhouette() -> &'static Silhouette {
    static MASK: OnceLock<Silhouette> = OnceLock::new();
    MASK.get_or_init(build_silhouette)
}

fn build_silhouette() -> Silhouette {
    let empty = Silhouette {
        w: 0,
        h: 0,
        pix: Vec::new(),
    };
    let Ok(img) = image::load_from_memory(LOGO_PNG) else {
        return empty;
    };
    let img = img.to_rgba8();
    let (iw, ih) = img.dimensions();
    let (iw, ih) = (iw as usize, ih as usize);
    if iw == 0 || ih == 0 {
        return empty;
    }

    // Ink = dark, opaque pixels (the outline strokes).
    let mut ink = vec![false; iw * ih];
    for y in 0..ih {
        for x in 0..iw {
            let [r, g, b, a] = img.get_pixel(x as u32, y as u32).0;
            let lum = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            ink[y * iw + x] = a > 128 && lum < 128.0;
        }
    }

    // Flood-fill the exterior: start from every border pixel and spread across
    // non-ink cells. Whatever is reached is background "outside" the mark.
    let mut outside = vec![false; iw * ih];
    let mut stack: Vec<usize> = Vec::new();
    let seed = |x: usize, y: usize, outside: &mut [bool], stack: &mut Vec<usize>| {
        let i = y * iw + x;
        if !ink[i] && !outside[i] {
            outside[i] = true;
            stack.push(i);
        }
    };
    for x in 0..iw {
        seed(x, 0, &mut outside, &mut stack);
        seed(x, ih - 1, &mut outside, &mut stack);
    }
    for y in 0..ih {
        seed(0, y, &mut outside, &mut stack);
        seed(iw - 1, y, &mut outside, &mut stack);
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % iw, i / iw);
        let visit = |nx: usize, ny: usize, outside: &mut [bool], stack: &mut Vec<usize>| {
            let j = ny * iw + nx;
            if !ink[j] && !outside[j] {
                outside[j] = true;
                stack.push(j);
            }
        };
        if x > 0 {
            visit(x - 1, y, &mut outside, &mut stack);
        }
        if x + 1 < iw {
            visit(x + 1, y, &mut outside, &mut stack);
        }
        if y > 0 {
            visit(x, y - 1, &mut outside, &mut stack);
        }
        if y + 1 < ih {
            visit(x, y + 1, &mut outside, &mut stack);
        }
    }

    // Silhouette = anything the exterior flood could not reach.
    // Compute its bounding box so the logo fills the render area tightly.
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (iw, ih, 0usize, 0usize);
    for y in 0..ih {
        for x in 0..iw {
            if !outside[y * iw + x] {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if min_x > max_x || min_y > max_y {
        return empty;
    }

    // Recenter the crop horizontally about the ink centroid so the logo's
    // visual centre (the gap between the pillars) lands at the middle of the
    // rendered art. Without this, a source image whose ink is even slightly
    // off-centre would make the wordmark beneath it look misaligned.
    let (mut sum_x, mut count) = (0usize, 0usize);
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if !outside[y * iw + x] {
                sum_x += x;
                count += 1;
            }
        }
    }
    if count > 0 {
        let axis = sum_x / count;
        let half = (axis - min_x).max(max_x - axis);
        min_x = axis.saturating_sub(half);
        max_x = (axis + half).min(iw - 1);
    }

    let (w, h) = (max_x - min_x + 1, max_y - min_y + 1);
    let mut pix = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            pix[y * w + x] = !outside[(min_y + y) * iw + (min_x + x)];
        }
    }
    Silhouette { w, h, pix }
}

/// Render the logo as ASCII rows sized to fit within `max_w` x `max_h` cells,
/// preserving the mark's proportions (terminal cells are ~2x taller than wide).
/// Each output cell samples a block of the silhouette and maps its ink coverage
/// onto an ASCII shading ramp, giving crisp filled pillars with anti-aliased
/// edges at any scale. The art is capped to a tasteful maximum so it stays a
/// logo rather than ballooning to fill a tall panel.
pub fn render_mark(max_w: u16, max_h: u16) -> Vec<String> {
    let m = silhouette();
    if m.w == 0 || m.h == 0 || max_w == 0 || max_h == 0 {
        return Vec::new();
    }
    let (iw, ih) = (m.w as f64, m.h as f64);

    // Keep the emblem logo-sized: never wider than ~44 cols or taller than ~22
    // rows, regardless of how much room the panel offers.
    const MAX_COLS: u16 = 44;
    const MAX_ROWS: u16 = 22;
    let bound_w = max_w.min(MAX_COLS);
    let bound_h = max_h.min(MAX_ROWS);

    // Cells are roughly twice as tall as wide, so a column is ~0.5 the visual
    // height of a row. Pick the largest grid that fits and keeps proportions.
    let rows_from_w = (bound_w as f64 * ih / (2.0 * iw)).floor();
    let rows = (bound_h as f64).min(rows_from_w).max(1.0) as usize;
    let cols = ((2.0 * rows as f64 * iw / ih).round() as usize).clamp(1, bound_w as usize);

    // ASCII shading ramp: blank background -> light edge -> solid pillar body.
    const RAMP: [char; 5] = [' ', '.', ':', '+', '#'];
    let mut out = Vec::with_capacity(rows);
    for ry in 0..rows {
        let y0 = ry * m.h / rows;
        let y1 = (((ry + 1) * m.h + rows - 1) / rows).min(m.h);
        let mut line = String::with_capacity(cols);
        for cx in 0..cols {
            let x0 = cx * m.w / cols;
            let x1 = (((cx + 1) * m.w + cols - 1) / cols).min(m.w);
            let (mut on, mut total) = (0usize, 0usize);
            for yy in y0..y1.max(y0 + 1) {
                for xx in x0..x1.max(x0 + 1) {
                    if yy < m.h && xx < m.w {
                        total += 1;
                        if m.pix[yy * m.w + xx] {
                            on += 1;
                        }
                    }
                }
            }
            let cov = if total == 0 {
                0.0
            } else {
                on as f64 / total as f64
            };
            let idx = (cov * (RAMP.len() - 1) as f64).round() as usize;
            line.push(RAMP[idx]);
        }
        out.push(line);
    }
    out
}

/// Render the emblem stacked above the "obleth" wordmark and a muted subtitle —
/// a clean vertical lockup that scales to the available `max_w` x `max_h` cells.
/// Returns styled lines ready for a `Paragraph`; the caller handles centring.
/// The emblem + name are accent/foreground; the subtitle is muted.
pub fn lockup_lines(subtitle: &str, max_w: u16, max_h: u16) -> Vec<Line<'static>> {
    let accent = Style::default()
        .fg(theme::ACCENT)
        .add_modifier(Modifier::BOLD);
    let name = Style::default().fg(theme::FG).add_modifier(Modifier::BOLD);
    let sub = Style::default().fg(theme::MUTED);

    // Reserve three rows beneath the mark for: spacer, wordmark, subtitle.
    let art_h = max_h.saturating_sub(3);
    let mut lines: Vec<Line<'static>> = render_mark(max_w, art_h)
        .into_iter()
        .map(|row| Line::from(Span::styled(row, accent)))
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("o b l e t h", name)));
    lines.push(Line::from(Span::styled(subtitle.to_string(), sub)));
    lines
}

/// A unicode block "meter" of `width` cells filled to `ratio` (0.0..=1.0).
/// Uses eighth-block glyphs so the last cell shows partial fill — much smoother
/// than a plain `#` bar. Returns the styled spans for one line.
pub fn meter(ratio: f64, width: usize, fill: Color, track: Color) -> Vec<Span<'static>> {
    let ratio = ratio.clamp(0.0, 1.0);
    let total_eighths = (ratio * (width as f64) * 8.0).round() as usize;
    let full = total_eighths / 8;
    let rem = total_eighths % 8;

    let mut filled = "█".repeat(full.min(width));
    let mut used = full.min(width);
    if used < width && rem > 0 {
        // Partial eighth-block: ▏▎▍▌▋▊▉
        const PARTS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
        filled.push_str(PARTS[rem]);
        used += 1;
    }
    let track_str = "░".repeat(width.saturating_sub(used));

    vec![
        Span::styled(filled, Style::default().fg(fill)),
        Span::styled(track_str, Style::default().fg(track)),
    ]
}
