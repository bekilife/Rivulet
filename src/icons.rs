//! Line icons drawn on a 24×24 grid (stroked with round caps).

use crate::gfx::{Color, Gfx, mul, scale, translate};

pub const PLUS: &str = "M12 5V19M5 12H19";
pub const PLAY: &str = "M8 5.8V18.2L18.2 12Z";
pub const PAUSE: &str = "M8.5 5.5V18.5M15.5 5.5V18.5";
pub const TRASH: &str = "M4.5 7H19.5M9.5 7V4.5H14.5V7M6.5 7L7.5 19.5H16.5L17.5 7M10 10.5V16M14 10.5V16";
pub const SLIDERS: &str = "M4 7H13M17 7H20M4 17H7M11 17H20M15 4.5V9.5M9 14.5V19.5";
pub const CHEVRON_DOWN: &str = "M7 10L12 15L17 10";
pub const CHEVRON_UP: &str = "M7 14L12 9L17 14";
pub const CHECK: &str = "M6.5 12.5L10.5 16.5L17.5 8";
pub const DOWN: &str = "M12 4.5V15.5M7.5 11L12 15.5L16.5 11M6 19.5H18";
pub const UP: &str = "M12 19.5V8.5M7.5 13L12 8.5L16.5 13M6 4.5H18";
pub const FOLDER: &str = "M3.5 7V17.5Q3.5 19 5 19H19Q20.5 19 20.5 17.5V9.5Q20.5 8 19 8H12L10 5.5H5Q3.5 5.5 3.5 7Z";
pub const PEERS: &str = "M8 19V5M4.5 8.5L8 5L11.5 8.5M16 5V19M12.5 15.5L16 19L19.5 15.5";
pub const BOX: &str = "M4.5 8L12 4L19.5 8V16L12 20L4.5 16ZM4.5 8L12 12L19.5 8M12 12V20";
pub const CLOCK: &str = "M12 3.5A8.5 8.5 0 1 0 12.01 3.5ZM12 7.5V12L15 14";
pub const WARN: &str = "M12 4L21 19.5H3ZM12 10V14M12 16.8V17";
pub const DONE: &str = "M12 3.5A8.5 8.5 0 1 0 12.01 3.5ZM8 12.3L11 15.2L16.2 9.5";
pub const SEARCH: &str = "M10.5 4A6.5 6.5 0 1 0 10.51 4ZM15.3 15.3L20 20";
pub const CLOSE: &str = "M7 7L17 17M17 7L7 17";
pub const ROTATE: &str = "M19 12A7 7 0 1 1 16.5 6.6M17 3.5V7H13.5";

/// Draws icon `d` in a `size`×`size` box at (x, y).
pub fn draw(g: &Gfx, d: &'static str, x: f32, y: f32, size: f32, width: f32, c: Color) {
    let s = size / 24.0;
    g.push_transform(mul(scale(s), translate(x, y)));
    g.stroke_path(d, width / s, c);
    g.pop_transform();
}
