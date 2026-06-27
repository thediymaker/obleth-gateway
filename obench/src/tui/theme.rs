use ratatui::style::Color;

// Matched to control-plane globals.css.
#[allow(dead_code)] // palette parity with globals.css; reserved for explicit bg fills
pub const BASE: Color = Color::Rgb(10, 10, 12); // near-black neutral
pub const FG: Color = Color::Rgb(244, 244, 246); // light foreground
pub const MUTED: Color = Color::Rgb(140, 140, 150);
pub const ACCENT: Color = Color::Rgb(66, 184, 131); // emerald "trace-accent"
pub const ALERT: Color = Color::Rgb(220, 90, 90); // error/429 red
