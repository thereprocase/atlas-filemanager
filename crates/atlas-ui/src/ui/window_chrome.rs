//! Neutral window controls and a native caption area behind the top row.
//! This layer floats over the shell and never changes the workspace's insets.

use crate::app::App;
use crate::ui::glass::control_colors;
#[cfg(not(windows))]
use crate::ui::icons::{icon, AtlasIcon};
use crate::ui::motion::MotionFeedbackExt as _;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::{
    div, px, AnyElement, Bounds, Context, Hsla, MouseButton, Pixels, Window, WindowControlArea,
};
use gpui_component::{h_flex, tooltip::Tooltip, ActiveTheme, InteractiveElementExt};

const CONTROL_SIZE: f32 = 28.0;
const GAP: f32 = 4.0;
const WIDTH: f32 = CONTROL_SIZE * 3.0 + GAP * 2.0;
pub(crate) const HEIGHT: f32 = 30.0;
const INSET: f32 = 16.0;

pub(crate) fn controls_top(row_height: Pixels, workspace_inset: Pixels, zoom: f32) -> Pixels {
    workspace_inset + ((row_height - px(HEIGHT * zoom)) / 2.0).max(px(0.0))
}

/// Only chrome that actually reaches the floating corner gives up horizontal
/// space. Other split panes keep their entire tab strip.
pub(crate) fn unobstructed_width(
    bounds: Bounds<Pixels>,
    viewport_width: Pixels,
    controls_top: Pixels,
    zoom: f32,
) -> Pixels {
    if bounds.top() >= controls_top + px(HEIGHT * zoom) || bounds.bottom() <= controls_top {
        return bounds.size.width;
    }
    let corner_left = viewport_width - px((INSET + WIDTH + GAP) * zoom);
    bounds
        .size
        .width
        .min((corner_left - bounds.left()).max(px(0.0)))
}

/// Measures the real div-based tab strip after its window-space bounds are
/// known, as GPUI's virtual lists do. This avoids reserving space in every pane
/// or estimating a split's position from last frame's cached bounds.
pub(crate) struct CornerTabStrip {
    strip: AnyElement,
    height: Pixels,
    controls_top: Pixels,
}

impl CornerTabStrip {
    pub(crate) fn new(strip: AnyElement, height: Pixels, controls_top: Pixels) -> Self {
        Self {
            strip,
            height,
            controls_top,
        }
    }
}

impl IntoElement for CornerTabStrip {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for CornerTabStrip {
    type RequestLayoutState = ();
    type PrepaintState = Bounds<Pixels>;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> (gpui::LayoutId, ()) {
        let mut style = gpui::Style::default();
        style.size.width = gpui::relative(1.0).into();
        style.size.height = self.height.into();
        style.flex_shrink = 0.0;
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        mut bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> Bounds<Pixels> {
        bounds.size.width = unobstructed_width(
            bounds,
            window.viewport_size().width,
            self.controls_top,
            f32::from(window.rem_size()) / crate::ui::BASE_PX,
        );
        self.strip.layout_as_root(
            gpui::size(
                gpui::AvailableSpace::Definite(bounds.size.width),
                gpui::AvailableSpace::Definite(bounds.size.height),
            ),
            window,
            cx,
        );
        window.with_content_mask(
            Some(gpui::ContentMask {
                bounds,
                ..Default::default()
            }),
            |window| {
                self.strip.prepaint_at(bounds.origin, window, cx);
            },
        );
        bounds
    }

    fn paint(
        &mut self,
        _id: Option<&gpui::GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        bounds: &mut Bounds<Pixels>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        window.with_content_mask(
            Some(gpui::ContentMask {
                bounds: *bounds,
                ..Default::default()
            }),
            |window| {
                self.strip.paint(window, cx);
            },
        );
    }
}

#[derive(Clone, Copy)]
enum Control {
    Minimize,
    Maximize,
    Close,
}

/// A real caption hitbox, separate from tabs and window buttons. Windows handles
/// dragging, double-click maximize/restore and the caption menu natively.
pub(crate) fn drag_region(height: Pixels) -> impl IntoElement {
    let hint = rust_i18n::t!("window_drag_hint").to_string();
    div()
        .id("window-drag-region")
        .absolute()
        .top_0()
        .left_0()
        .w_full()
        .h(height)
        .window_control_area(WindowControlArea::Drag)
        .when(!cfg!(windows), |s| {
            s.on_double_click(|_, window, _| window.titlebar_double_click())
                .on_mouse_move(|ev, window, _| {
                    if ev.pressed_button == Some(MouseButton::Left) {
                        window.start_window_move();
                    }
                })
        })
        .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
}

pub(crate) fn corner_titlebar(
    top: Pixels,
    window: &mut Window,
    cx: &mut Context<App>,
) -> AnyElement {
    let colors = control_colors(cx.theme(), false);
    let foreground = colors.foreground;
    let hover = colors.hover;
    let maximized = window.is_maximized();

    h_flex()
        .id("titlebar")
        .absolute()
        .top(top)
        .right(crate::ui::rem_px(INSET))
        .w(crate::ui::rem_px(WIDTH))
        .h(crate::ui::rem_px(HEIGHT))
        .gap(crate::ui::rem_px(GAP))
        .items_center()
        .child(control(Control::Minimize, maximized, foreground, hover))
        .child(control(Control::Maximize, maximized, foreground, hover))
        .child(control(Control::Close, maximized, foreground, hover))
        .into_any_element()
}

fn control(action: Control, maximized: bool, foreground: Hsla, hover: Hsla) -> impl IntoElement {
    let (id, area, label) = match action {
        Control::Minimize => ("wc-min", WindowControlArea::Min, "window_minimize"),
        Control::Maximize => (
            "wc-max",
            WindowControlArea::Max,
            if maximized {
                "window_restore"
            } else {
                "window_maximize"
            },
        ),
        Control::Close => ("wc-close", WindowControlArea::Close, "window_close"),
    };
    let label = rust_i18n::t!(label).to_string();
    // Caption glyphs use the Windows icon font rather than heavier toolbar SVGs.
    #[cfg(windows)]
    let symbol = div()
        .size(crate::ui::rem_px(10.0))
        .flex_none()
        .font_family("Segoe MDL2 Assets")
        .text_size(crate::ui::rem_px(10.0))
        .line_height(crate::ui::rem_px(10.0))
        .child(match action {
            Control::Minimize => "\u{E921}",
            Control::Maximize if maximized => "\u{E923}",
            Control::Maximize => "\u{E922}",
            Control::Close => "\u{E8BB}",
        })
        .into_any_element();
    #[cfg(not(windows))]
    let symbol = icon(
        match action {
            Control::Minimize => AtlasIcon::Minus,
            Control::Maximize if maximized => AtlasIcon::Copy,
            Control::Maximize => AtlasIcon::Square,
            Control::Close => AtlasIcon::X,
        },
        10.0,
        foreground.opacity(0.72),
    );
    div()
        .id(id)
        .size(crate::ui::rem_px(CONTROL_SIZE))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(crate::ui::rem_px(if crate::ui::gridline::active() { 0.0 } else { 8.0 }))
        .block_mouse_except_scroll()
        .bg(hover.opacity(0.0))
        .text_color(foreground.opacity(0.72))
        .motion_feedback()
        .hover(move |s| s.bg(hover).text_color(foreground))
        // Windows owns these non-client controls, including maximize's Snap UI.
        .when(cfg!(windows), |s| s.window_control_area(area))
        .when(!cfg!(windows), |s| {
            s.on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                match action {
                    Control::Minimize => window.minimize_window(),
                    Control::Maximize => window.zoom_window(),
                    Control::Close => window.remove_window(),
                }
            })
        })
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
        .child(symbol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    #[test]
    fn corner_reserves_space_only_in_the_overlapping_top_pane() {
        let viewport_width = px(1280.0);
        let left = Bounds::new(point(px(300.0), px(10.0)), size(px(320.0), px(46.0)));
        let right = Bounds::new(point(px(958.0), px(10.0)), size(px(312.0), px(46.0)));
        let below = Bounds::new(point(px(958.0), px(340.0)), size(px(312.0), px(46.0)));
        let top = controls_top(px(34.0), px(6.0), 1.0);
        assert_eq!(
            unobstructed_width(left, viewport_width, top, 1.0),
            left.size.width
        );
        assert_eq!(
            unobstructed_width(right, viewport_width, top, 1.0),
            px(210.0)
        );
        assert_eq!(
            unobstructed_width(below, viewport_width, top, 1.0),
            below.size.width
        );
    }

    #[test]
    fn controls_scale_and_narrow_tabs_never_have_negative_width() {
        let top = controls_top(px(68.0), px(6.0), 2.0);
        assert_eq!(top, px(10.0));
        let right = Bounds::new(point(px(958.0), px(6.0)), size(px(312.0), px(68.0)));
        assert_eq!(unobstructed_width(right, px(1280.0), top, 2.0), px(98.0));
        let narrow = Bounds::new(point(px(1250.0), px(6.0)), size(px(20.0), px(68.0)));
        assert_eq!(unobstructed_width(narrow, px(1280.0), top, 2.0), px(0.0));
    }
}
