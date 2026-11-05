//! Flat UI surfaces: math layout + measured text + hit slots + theme icons.
//!
//! Custom GPUI Elements use this kit so chrome is ~1 layout node while still
//! matching div-chrome look (Fluent vs Phosphor, xs/sm type scale).

use crate::ui::icons::{windows_icon_glyph, AtlasIcon};
use gpui::AppContext as _;
use gpui::{
    point, px, size, transparent_black, Bounds, Corners, Edges, Font, FontFeatures, FontStyle,
    FontWeight, Hsla, Pixels, ShapedLine, SharedString, TextAlign, TextRun, Window,
};

/// Axis-aligned rect in surface-local pixels (origin = surface top-left).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Slot {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Slot {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, lx: f32, ly: f32) -> bool {
        lx >= self.x && lx < self.x + self.w && ly >= self.y && ly < self.y + self.h
    }

    pub fn to_bounds(&self, origin: gpui::Point<Pixels>) -> Bounds<Pixels> {
        Bounds {
            origin: point(origin.x + px(self.x), origin.y + px(self.y)),
            size: size(px(self.w.max(0.0)), px(self.h.max(0.0))),
        }
    }

    pub fn center_y(&self) -> f32 {
        self.y + self.h * 0.5
    }
}

// ── Chrome type scale (matches old .text_xs / .text_sm / .text_base) ─────────

/// A canvas has one native hitbox, so moving between its logical controls does
/// not invalidate it automatically. Repaint only when the logical target changes.
/// Capture also clears a stale hover when an overlay consumes the bubble event.
pub(crate) fn track_hover<K: Clone + PartialEq + 'static>(
    key: SharedString,
    hitbox: &gpui::Hitbox,
    origin: gpui::Point<Pixels>,
    hits: std::sync::Arc<Vec<(Slot, K)>>,
    window: &mut Window,
    cx: &mut gpui::App,
) -> Option<K> {
    let position = window.mouse_position();
    let current = hover_target(&hits, origin, position, hitbox.is_hovered(window));
    let state = window.use_keyed_state(key, cx, |_, _| None::<K>);
    state.update(cx, |last, _| *last = current.clone());
    let owner = window.current_view();
    let hitbox = hitbox.clone();
    window.on_mouse_event::<gpui::MouseMoveEvent>(move |event, phase, window, cx| {
        if phase != gpui::DispatchPhase::Capture {
            return;
        }
        let next = hover_target(&hits, origin, event.position, hitbox.is_hovered(window));
        let changed = state.update(cx, |last, _| {
            if *last == next {
                false
            } else {
                *last = next;
                true
            }
        });
        if changed {
            crate::perf::count("imm.hover.target_changed");
            cx.notify(owner);
        }
    });
    current
}

fn hover_target<K: Clone>(
    hits: &[(Slot, K)],
    origin: gpui::Point<Pixels>,
    position: gpui::Point<Pixels>,
    visible: bool,
) -> Option<K> {
    visible
        .then(|| {
            hit_first(
                hits,
                f32::from(position.x - origin.x),
                f32::from(position.y - origin.y),
            )
        })
        .flatten()
}

/// Tab label: `text_xs` (non-Windows) / `text_sm` (Windows).
pub(crate) fn chrome_font_size_tabs(window: &Window, windows_theme: bool) -> Pixels {
    let rem = f32::from(window.rem_size());
    px(rem * if windows_theme { 0.875 } else { 0.75 })
}

/// Breadcrumb path labels: `text_sm` / `text_base` (Windows).
pub(crate) fn chrome_font_size_crumbs(window: &Window, windows_theme: bool) -> Pixels {
    let rem = f32::from(window.rem_size());
    px(rem * if windows_theme { 1.0 } else { 0.875 })
}

/// Details column header: always `text_xs`.
pub(crate) fn chrome_font_size_header(window: &Window) -> Pixels {
    let rem = f32::from(window.rem_size());
    px(rem * 0.75)
}

/// UI font at a specific size (keeps family/weight from window style).
pub(crate) fn chrome_ui_font(window: &Window) -> Font {
    let mut font = window.text_style().font();
    if crate::ui::gridline::active() { font.family = "IBM Plex Mono".into(); }
    font
}

fn fluent_icons_font() -> Font {
    use std::sync::LazyLock;
    static FONT: LazyLock<Font> = LazyLock::new(|| Font {
        family: SharedString::from("Segoe Fluent Icons"),
        features: FontFeatures::default(),
        fallbacks: None,
        weight: FontWeight::default(),
        style: FontStyle::default(),
    });
    FONT.clone()
}

// ── Text ────────────────────────────────────────────────────────────────────

/// Shape a single-line label (measured width via [`line_width`]).
pub(crate) fn shape_text(
    text: impl Into<SharedString>,
    font: &Font,
    font_size: Pixels,
    color: Hsla,
    window: &Window,
) -> ShapedLine {
    let text_ss: SharedString = text.into();
    let run = TextRun {
        len: text_ss.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
        letter_spacing: None,
    };
    window
        .text_system()
        .shape_line(text_ss, font_size, &[run], None)
}

/// Shapes a single line of text with binary-search UTF-8 truncation ('…') if it exceeds `max_text_w`.
///
/// Implements the O(1) rect-slicing algorithm:
/// 1. Measures full string; returns immediately if it fits (0-allocation fast path).
/// 2. Measures ellipsis glyph '…'.
/// 3. Binary searches UTF-8 char boundary indices for the maximal fit.
pub(crate) fn shape_text_truncated(
    text: &str,
    font: &Font,
    font_size: Pixels,
    color: Hsla,
    max_text_w: f32,
    window: &Window,
) -> ShapedLine {
    let full = shape_text(text, font, font_size, color, window);
    if text.is_empty() || max_text_w <= 0.0 || line_width(&full) <= max_text_w {
        return full;
    }

    let ellipsis = "…";
    let el_line = shape_text(ellipsis, font, font_size, color, window);
    let el_w = line_width(&el_line);
    if el_w >= max_text_w {
        return el_line;
    }

    let target_w = max_text_w - el_w;
    let char_indices: Vec<usize> = text.char_indices().map(|(idx, _)| idx).collect();
    let mut low = 0;
    let mut high = char_indices.len();
    let mut best_byte = 0;

    while low <= high && high > 0 {
        let mid = low + (high - low) / 2;
        let byte_idx = if mid < char_indices.len() {
            char_indices[mid]
        } else {
            text.len()
        };
        let sub = &text[..byte_idx];
        let sub_line = shape_text(sub, font, font_size, color, window);
        if line_width(&sub_line) <= target_w {
            best_byte = byte_idx;
            low = mid + 1;
        } else {
            if mid == 0 {
                break;
            }
            high = mid - 1;
        }
    }

    let mut elided = text[..best_byte].to_string();
    elided.push('…');
    shape_text(&elided, font, font_size, color, window)
}

/// Greedy word-wrap into at most `max_lines` shaped lines. A word wider than
/// `max_w` and any overflow past the last line are ellipsized.
pub(crate) fn wrap_text_lines(
    text: &str,
    font: &Font,
    font_size: Pixels,
    color: Hsla,
    max_w: f32,
    max_lines: usize,
    window: &Window,
) -> Vec<ShapedLine> {
    let width_of = |t: &str| line_width(&shape_text(t.to_string(), font, font_size, color, window));
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if current.is_empty() || width_of(&candidate) <= max_w {
            current = candidate;
        } else {
            lines.push(std::mem::replace(&mut current, word.to_string()));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    let max_lines = max_lines.max(1);
    if lines.len() > max_lines {
        let tail = lines.split_off(max_lines - 1).join(" ");
        lines.push(tail);
    }
    lines
        .iter()
        .map(|line| shape_text_truncated(line, font, font_size, color, max_w, window))
        .collect()
}

#[inline]
pub(crate) fn line_width(line: &ShapedLine) -> f32 {
    f32::from(line.width())
}

/// Top of the paint box for shaped text inside a slot.
///
/// Pass this as `origin.y` to `ShapedLine::paint` and pass **`slot_h` as
/// `line_height`**. GPUI centers glyphs inside that box — do **not** also
/// subtract font_size/2 (that double-shifts labels up/down).
pub(crate) fn text_origin_y(slot_y: f32, zoom: f32) -> f32 {
    // Tiny optical nudge like Details rows (`ROW_TEXT_Y_NUDGE`).
    slot_y - 1.0 * zoom
}

// ── Hit testing ──────────────────────────────────────────────────────────────

/// First hit among slots (earlier entries win — put close/resize before body).
pub(crate) fn hit_first<T: Clone>(slots: &[(Slot, T)], lx: f32, ly: f32) -> Option<T> {
    for (slot, id) in slots {
        if slot.contains(lx, ly) {
            return Some(id.clone());
        }
    }
    None
}

// ── Paint primitives ─────────────────────────────────────────────────────────

pub(crate) fn paint_fill(window: &mut Window, bounds: Bounds<Pixels>, color: Hsla) {
    window.paint_quad(gpui::fill(bounds, color));
}

pub(crate) fn paint_rounded(window: &mut Window, bounds: Bounds<Pixels>, radius: f32, color: Hsla) {
    paint_rounded_corners(window, bounds, Corners::all(px(radius.max(0.0))), color);
}

pub(crate) fn paint_rounded_corners(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    corners: Corners<Pixels>,
    color: Hsla,
) {
    let corners = if crate::ui::gridline::active() { Corners::default() } else { corners };
    window.paint_quad(gpui::quad(
        bounds,
        corners,
        color,
        Edges::all(px(0.0)),
        transparent_black(),
        Default::default(),
    ));
}

pub(crate) fn paint_svg_icon(
    window: &mut Window,
    cx: &mut gpui::App,
    bounds: Bounds<Pixels>,
    path: &'static str,
    color: Hsla,
) {
    let _ = window.paint_svg(
        bounds,
        SharedString::from(path),
        None,
        Default::default(),
        color,
        cx,
    );
}

/// Same policy as `theme_icon`: Fluent glyph on Windows themes, Phosphor SVG otherwise.
pub(crate) fn paint_theme_icon(
    window: &mut Window,
    cx: &mut gpui::App,
    origin: gpui::Point<Pixels>,
    slot: Slot,
    kind: AtlasIcon,
    windows_theme: bool,
    // Icon pixel size already zoom-scaled (e.g. 15.0 * zoom).
    icon_px: f32,
    color: Hsla,
) {
    if slot.w < 8.0 || slot.h < 8.0 {
        return;
    }
    let sz = icon_px.max(1.0);
    let b = Bounds {
        origin: point(
            origin.x + px(slot.x + (slot.w - sz) * 0.5),
            origin.y + px(slot.y + (slot.h - sz) * 0.5),
        ),
        size: size(px(sz), px(sz)),
    };

    let use_fluent = windows_theme && kind.has_windows_glyph();
    if use_fluent {
        let glyph = windows_icon_glyph(kind);
        let font = fluent_icons_font();
        let line = shape_text(glyph, &font, px(sz), color, window);
        let w = line_width(&line);
        let x = f32::from(b.origin.x) + (sz - w) * 0.5;
        let _ = line.paint(
            point(px(x), b.origin.y),
            px(sz),
            TextAlign::Left,
            None,
            window,
            cx,
        );
    } else {
        paint_svg_icon(window, cx, b, kind.path(), color);
    }
}

/// Legacy helper: SVG-only icon as fraction of slot (prefer [`paint_theme_icon`]).
pub(crate) fn paint_icon_in_slot(
    window: &mut Window,
    cx: &mut gpui::App,
    origin: gpui::Point<Pixels>,
    slot: Slot,
    icon_frac: f32,
    path: &'static str,
    color: Hsla,
) {
    let sz = (slot.w.min(slot.h) * icon_frac).max(1.0);
    let b = Bounds {
        origin: point(
            origin.x + px(slot.x + (slot.w - sz) * 0.5),
            origin.y + px(slot.y + (slot.h - sz) * 0.5),
        ),
        size: size(px(sz), px(sz)),
    };
    paint_svg_icon(window, cx, b, path, color);
}

#[cfg(test)]
mod hover_tests {
    use super::*;

    #[test]
    fn chrome_targets_follow_offsets_priorities_disabled_gaps_and_occlusion() {
        // A moved/split pane at 200% zoom: specific chevrons must win over the
        // address background, and disabled navigation must not open the finder.
        let hits = vec![
            (Slot::new(0.0, 0.0, 52.0, 52.0), "back"),
            (Slot::new(140.0, 0.0, 28.0, 52.0), "chevron"),
            (Slot::new(104.0, 0.0, 100.0, 52.0), "segment"),
            (Slot::new(104.0, 0.0, 200.0, 52.0), "finder"),
        ];
        let origin = point(px(400.0), px(120.0));
        let at = |x: f32, visible| hover_target(&hits, origin, point(px(400.0 + x), px(130.0)), visible);
        assert_eq!(at(20.0, true), Some("back"));
        assert_eq!(at(70.0, true), None);
        assert_eq!(at(120.0, true), Some("segment"));
        assert_eq!(at(150.0, true), Some("chevron"));
        assert_eq!(at(240.0, true), Some("finder"));
        assert_eq!(at(304.0, true), None);
        assert_eq!(at(150.0, false), None);
    }
}
