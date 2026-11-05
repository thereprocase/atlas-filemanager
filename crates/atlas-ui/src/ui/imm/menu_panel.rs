//! Arithmetic popover chrome, IMM text input and scrollbar. Rows are one IMM leaf.
use crate::app::App as AtlasApp;
use crate::ui::imm::{menu::MenuCanvas, ImmTextInputState};
use crate::ui::icons::AtlasIcon;
use crate::ui::surface::{paint_fill, paint_rounded, paint_theme_icon, shape_text, Slot};
use gpui::{
    point, px, size, AnyElement, App, AvailableSpace, Bounds, BoxShadow, ColorExt, ContentMask,
    Corners, CursorStyle, DispatchPhase, Element, ElementId, ElementInputHandler, FocusHandle,
    GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollWheelEvent,
    ShapedLine, Style, TextAlign, WeakEntity, Window,
};

#[derive(Clone, Copy)]
pub(crate) struct PanelColors {
    pub background: Hsla,
    pub foreground: Hsla,
    pub muted: Hsla,
    pub border: Hsla,
    pub accent: Hsla,
    pub radius: f32,
}

/// Same geometry drives the visible thumb and pointer-to-content projection.
pub(crate) fn scrollbar_slot(
    viewport: Slot,
    content_h: f32,
    scroll: f32,
    zoom: f32,
) -> Option<Slot> {
    if content_h <= viewport.h || viewport.h <= 0.0 {
        return None;
    }
    let height = (viewport.h * viewport.h / content_h)
        .max(24.0 * zoom)
        .min(viewport.h);
    let max_scroll = content_h - viewport.h;
    let y = viewport.y + (viewport.h - height) * (scroll / max_scroll).clamp(0.0, 1.0);
    Some(Slot::new(
        viewport.x + viewport.w - 10.0 * zoom,
        y,
        6.0 * zoom,
        height,
    ))
}

pub(crate) struct MenuPanel {
    pub app: WeakEntity<AtlasApp>,
    pub menu_id: u64,
    pub focus: FocusHandle,
    pub input: ImmTextInputState,
    pub recording: bool,
    pub scroll: f32,
    pub content_h: f32,
    pub height: f32,
    pub colors: PanelColors,
    pub rows: AnyElement,
}

impl MenuPanel {
    pub fn rows(canvas: MenuCanvas) -> AnyElement {
        canvas.into_any_element()
    }
}

pub(crate) struct PanelPrepaint {
    hitbox: Hitbox,
    viewport: Bounds<Pixels>,
    search: Bounds<Pixels>,
    line: ShapedLine,
    placeholder: Option<ShapedLine>,
    scroll_x: f32,
    thumb: Option<Slot>,
    zoom: f32,
}

impl IntoElement for MenuPanel {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for MenuPanel {
    type RequestLayoutState = Style;
    type PrepaintState = PanelPrepaint;
    fn id(&self) -> Option<ElementId> {
        Some("ctx-menu-imm-panel".into())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.size.width = gpui::relative(1.0).into();
        style.size.height = px(self.height).into();
        (window.request_layout(style.clone(), [], cx), style)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _style: &mut Style,
        window: &mut Window,
        cx: &mut App,
    ) -> PanelPrepaint {
        let zoom = f32::from(window.rem_size()) / crate::ui::BASE_PX;
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::BlockMouse);
        let width = f32::from(bounds.size.width);
        let search = Slot::new(
            32.0 * zoom,
            4.0 * zoom,
            (width - 40.0 * zoom).max(0.0),
            32.0 * zoom,
        )
        .to_bounds(bounds.origin);
        let list_slot = Slot::new(
            zoom,
            40.0 * zoom,
            (width - 2.0 * zoom).max(0.0),
            (self.height - 41.0 * zoom).max(0.0),
        );
        let viewport = list_slot.to_bounds(bounds.origin);
        let font = window.text_style().font();
        let line = shape_text(
            &self.input.text,
            &font,
            px(14.0 * zoom),
            self.colors.foreground,
            window,
        );
        let placeholder = self.input.text.is_empty().then(|| {
            shape_text(
                rust_i18n::t!("palette_search_commands").to_string(),
                &font,
                px(14.0 * zoom),
                self.colors.muted,
                window,
            )
        });
        let cursor_x = f32::from(line.x_for_index(self.input.cursor));
        let prior = self
            .app
            .upgrade()
            .and_then(|app| {
                app.read(cx)
                    .ctx_menu
                    .as_ref()
                    .map(|menu| menu.input_scroll_x)
            })
            .unwrap_or(0.0);
        let available = f32::from(search.size.width).max(1.0);
        let scroll_x = prior
            .min(cursor_x)
            .max(cursor_x - available + 2.0 * zoom)
            .max(0.0)
            .min((f32::from(line.width()) - available + 2.0 * zoom).max(0.0));
        if let Some(app) = self.app.upgrade() {
            app.update(cx, |app, _| {
                if let Some(menu) = app
                    .ctx_menu
                    .as_mut()
                    .filter(|menu| menu.menu_id == self.menu_id)
                {
                    menu.input_bounds = Some(search);
                    menu.input_line = Some(line.clone());
                    menu.input_scroll_x = scroll_x;
                }
            });
        }
        let thumb = scrollbar_slot(list_slot, self.content_h, self.scroll, zoom);
        let gutter = if thumb.is_some() { 14.0 * zoom } else { 0.0 };
        window.with_content_mask(
            Some(ContentMask {
                bounds: viewport,
                fade_out: Default::default(),
            }),
            |window| {
                self.rows.prepaint_as_root(
                    point(viewport.left(), viewport.top() - px(self.scroll)),
                    size(
                        AvailableSpace::Definite((viewport.size.width - px(gutter)).max(px(0.0))),
                        AvailableSpace::Definite(px(self.content_h)),
                    ),
                    window,
                    cx,
                );
            },
        );
        PanelPrepaint {
            hitbox,
            viewport,
            search,
            line,
            placeholder,
            scroll_x,
            thumb,
            zoom,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _style: &mut Style,
        ink: &mut PanelPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let zoom = ink.zoom;
        let colors = self.colors;
        let corners = Corners::all(crate::ui::gridline::radius(colors.radius));
        if !crate::ui::gridline::active() { window.paint_drop_shadows(
            bounds,
            corners,
            &[
                BoxShadow::new(px(0.0), px(8.0 * zoom), gpui::black().opacity(0.28))
                    .blur_radius(px(24.0 * zoom)),
            ],
        );
        }
        if colors.background.alpha < 1.0 {
            window.paint_backdrop_filter(bounds, corners, &[gpui::Filter::Blur(px(16.0))]);
        }
        window.paint_quad(gpui::quad(
            bounds,
            corners,
            colors.background,
            gpui::Edges::all(px(zoom)),
            colors.border,
            Default::default(),
        ));
        paint_fill(
            window,
            Slot::new(
                zoom,
                39.0 * zoom,
                (f32::from(bounds.size.width) - 2.0 * zoom).max(0.0),
                zoom,
            )
            .to_bounds(bounds.origin),
            colors.border,
        );
        paint_theme_icon(
            window,
            cx,
            bounds.origin,
            Slot::new(8.0 * zoom, 13.0 * zoom, 14.0 * zoom, 14.0 * zoom),
            AtlasIcon::Search,
            false,
            14.0 * zoom,
            colors.muted,
        );
        window.with_content_mask(
            Some(ContentMask {
                bounds: ink.search,
                fade_out: Default::default(),
            }),
            |window| {
                let x = ink.search.left() - px(ink.scroll_x);
                let (start, end) = self.input.selection_range();
                if start < end {
                    paint_fill(
                        window,
                        Bounds {
                            origin: point(
                                x + ink.line.x_for_index(start),
                                ink.search.top() + px(5.0 * zoom),
                            ),
                            size: size(
                                ink.line.x_for_index(end) - ink.line.x_for_index(start),
                                px(22.0 * zoom),
                            ),
                        },
                        colors.accent.opacity(0.3),
                    );
                }
                let line = ink.placeholder.as_ref().unwrap_or(&ink.line);
                let _ = line.paint(
                    point(x, ink.search.top()),
                    ink.search.size.height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                if let Some(marked) = &self.input.marked {
                    paint_fill(
                        window,
                        Bounds {
                            origin: point(
                                x + ink.line.x_for_index(marked.start),
                                ink.search.bottom() - px(6.0 * zoom),
                            ),
                            size: size(
                                ink.line.x_for_index(marked.end)
                                    - ink.line.x_for_index(marked.start),
                                px(zoom),
                            ),
                        },
                        colors.foreground,
                    );
                }
                if !self.recording && (self.input.blink_epoch.elapsed().as_millis() / 530) % 2 == 0
                {
                    paint_fill(
                        window,
                        Bounds {
                            origin: point(
                                x + ink.line.x_for_index(self.input.cursor),
                                ink.search.top() + px(7.0 * zoom),
                            ),
                            size: size(px(zoom), px(18.0 * zoom)),
                        },
                        colors.foreground,
                    );
                }
            },
        );
        window.with_content_mask(
            Some(ContentMask {
                bounds: ink.viewport,
                fade_out: Default::default(),
            }),
            |window| self.rows.paint(window, cx),
        );
        if let Some(thumb) = ink.thumb {
            paint_rounded(
                window,
                thumb.to_bounds(bounds.origin),
                3.0 * zoom,
                colors.muted.opacity(0.55),
            );
        }
        if ink.hitbox.is_hovered(window) && ink.search.contains(&window.mouse_position()) {
            window.set_cursor_style(CursorStyle::IBeam, &ink.hitbox);
        }
        let Some(app) = self.app.upgrade() else {
            return;
        };
        if !self.recording {
            window.handle_input(
                &self.focus,
                ElementInputHandler::new(ink.search, app.clone()),
                cx,
            );
        }
        let menu_id = self.menu_id;
        let hitbox = ink.hitbox.clone();
        let view = app.clone();
        let viewport = ink.viewport;
        let max_scroll = (self.content_h - f32::from(viewport.size.height)).max(0.0);
        window.on_mouse_event::<ScrollWheelEvent>(move |ev, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                return;
            }
            view.update(cx, |app, cx| {
                if let Some(menu) = app.ctx_menu.as_mut().filter(|menu| menu.menu_id == menu_id) {
                    menu.scroll_top = (menu.scroll_top
                        - f32::from(ev.delta.pixel_delta(px(30.0 * zoom)).y))
                    .clamp(0.0, max_scroll);
                    cx.notify();
                }
            });
            cx.stop_propagation();
        });
        let view = app.clone();
        let hitbox = ink.hitbox.clone();
        let search = ink.search;
        let line = ink.line.clone();
        let scroll_x = ink.scroll_x;
        let thumb = ink.thumb.map(|slot| slot.to_bounds(bounds.origin));
        let scroll = self.scroll;
        let focus = self.focus.clone();
        window.on_mouse_event::<MouseDownEvent>(move |ev, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !hitbox.is_hovered(window) {
                return;
            }
            let scrollbar_hit = thumb.is_some_and(|thumb| {
                ev.position.x >= thumb.left() - px(4.0 * zoom) && viewport.contains(&ev.position)
            });
            if ev.button == MouseButton::Left && viewport.contains(&ev.position) && !scrollbar_hit {
                // The IMM row leaf owns this region and its badge/action targets.
                return;
            }
            if ev.button == MouseButton::Left {
                window.focus(&focus, cx);
                view.update(cx, |app, cx| {
                    if app
                        .ctx_menu
                        .as_ref()
                        .is_none_or(|menu| menu.menu_id != menu_id)
                    {
                        return;
                    }
                    app.cancel_command_shortcut_capture();
                    if let Some(menu) = app.ctx_menu.as_mut().filter(|menu| menu.menu_id == menu_id)
                    {
                        if search.contains(&ev.position) {
                            menu.input.cursor = line
                                .closest_index_for_x(ev.position.x - search.left() + px(scroll_x));
                            if !ev.modifiers.shift {
                                menu.input.anchor = menu.input.cursor;
                            }
                            if ev.click_count >= 3 {
                                menu.input.select_all();
                            } else if ev.click_count == 2 {
                                menu.input.select_word();
                            }
                            menu.input.blink_epoch = std::time::Instant::now();
                            menu.input_drag = true;
                        } else if let Some(thumb) = thumb {
                            if ev.position.x >= thumb.left() - px(4.0 * zoom)
                                && viewport.contains(&ev.position)
                            {
                                let grab_y = if thumb.contains(&ev.position) {
                                    f32::from(ev.position.y - thumb.top())
                                } else {
                                    f32::from(thumb.size.height) * 0.5
                                };
                                menu.scroll_drag = Some(grab_y);
                                let travel = f32::from(viewport.size.height - thumb.size.height);
                                menu.scroll_top = if travel > 0.0 {
                                    ((f32::from(ev.position.y - viewport.top()) - grab_y) / travel
                                        * max_scroll)
                                        .clamp(0.0, max_scroll)
                                } else {
                                    scroll
                                };
                            }
                        }
                        cx.notify();
                    }
                });
            }
            cx.stop_propagation();
        });
        let view = app.clone();
        let line = ink.line.clone();
        window.on_mouse_event::<MouseMoveEvent>(move |ev, phase, _window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            view.update(cx, |app, cx| {
                if let Some(menu) = app.ctx_menu.as_mut().filter(|menu| menu.menu_id == menu_id) {
                    if menu.input_drag {
                        menu.input.cursor =
                            line.closest_index_for_x(ev.position.x - search.left() + px(scroll_x));
                        menu.input.blink_epoch = std::time::Instant::now();
                        cx.notify();
                    }
                    if let (Some(grab_y), Some(thumb)) = (menu.scroll_drag, thumb) {
                        let travel = f32::from(viewport.size.height - thumb.size.height);
                        if travel > 0.0 {
                            menu.scroll_top =
                                ((f32::from(ev.position.y - viewport.top()) - grab_y) / travel
                                    * max_scroll)
                                    .clamp(0.0, max_scroll);
                            cx.notify();
                        }
                    }
                }
            });
        });
        window.on_mouse_event::<MouseUpEvent>(move |ev, phase, _window, cx| {
            if phase != DispatchPhase::Bubble || ev.button != MouseButton::Left {
                return;
            }
            app.update(cx, |app, _| {
                if let Some(menu) = app.ctx_menu.as_mut().filter(|menu| menu.menu_id == menu_id) {
                    menu.input_drag = false;
                    menu.scroll_drag = None;
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scrollbar_and_drag_projection_agree_at_both_ends_and_zoom() {
        for zoom in [0.75, 1.0, 1.5, 2.0] {
            let viewport = Slot::new(0.0, 40.0 * zoom, 340.0 * zoom, 200.0 * zoom);
            let content_h = 900.0 * zoom;
            let start = scrollbar_slot(viewport, content_h, 0.0, zoom).unwrap();
            let end = scrollbar_slot(viewport, content_h, content_h - viewport.h, zoom).unwrap();
            assert!((start.y - viewport.y).abs() < 0.001);
            assert!((end.y + end.h - viewport.y - viewport.h).abs() < 0.001);
            assert!(scrollbar_slot(viewport, viewport.h, 0.0, zoom).is_none());
            let short = Slot::new(0.0, 0.0, 200.0, 12.0);
            let thumb = scrollbar_slot(short, 1000.0, 0.0, zoom).unwrap();
            assert!(thumb.h <= short.h);
        }
    }
}
