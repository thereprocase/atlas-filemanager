//! Phase5 — Inspector / preview panel (gpui port of the egui `src/ui/inspector.rs`).
//!
//! Renders the right-hand preview panel for the focused tab's selection:
//! single-file metadata + content preview (text/markdown/image/pdf/dir/binary),
//! or a multi-selection summary with a thumbnail stack.
//!
//! Framework split (per the migration rules):
//! - `state::preview` (neutral) holds the decoded RGBA (`PreviewResult` / `ImageData`).
//! - The GPU textures (`Arc<RenderImage>`) are cached HERE, on the UI side, in new
//!   `App` fields (`preview_textures` / `multi_textures`). `state` never holds a
//!   renderer handle.
//!
//! The egui version drove preview loading through `sync_active_preview(ctx)` which
//! needed an `egui::Context` for `request_repaint`. Under gpui the ~60 Hz drain loop
//! already repaints when background results land, so the gpui variants drop `ctx`
//! entirely (`sync_active_preview_gpui` / `load_preview_gpui` / `load_multi_preview_gpui`).

use crate::app::App;
use crate::input::action::Action;
use crate::ui::drag::FileDragSource as _;
use crate::ui::glass::{backdrop_tint, layer_color, GlassExt};
use crate::ui::icons::{file_type_icon, icon, AtlasIcon};
use crate::ui::root::DraggedFiles;
use atlas_core::util::display::{format_date, format_size};
use atlas_model::file_entry::FileKind;
use atlas_model::layout_tree::TileId;
use atlas_model::preview_cache::{
    MultiPreviewState, PreviewRequest, PreviewResult, DIR_PREVIEW_LIMIT,
};
use atlas_model::settings::ViewMode;
use atlas_model::tab::TabState;
use gpui::prelude::*;
use gpui::ColorExt as _;
use gpui::SharedString;
use gpui::{
    div, img, px, AnyElement, ClickEvent, Context, Entity, Hsla, MouseButton, RenderImage,
    WeakEntity, Window,
};
use gpui_component::button::ButtonVariants as _;
use gpui_component::input::InputEvent;
use gpui_component::scroll::ScrollableElement;
use gpui_component::ElementExt;
use gpui_component::{h_flex, v_flex, ActiveTheme, Sizable as _};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Retained host for the preview body. The root still owns the panel shell and
/// resize/close animation, while this entity only re-renders when the focused
/// preview's visual fingerprint changes.
pub(crate) struct InspectorHost {
    app: WeakEntity<App>,
    fingerprint: u64,
    is_rendering: bool,
}

impl InspectorHost {
    pub(crate) fn new(app: Entity<App>, cx: &mut Context<Self>) -> Self {
        cx.observe(&app, |this: &mut Self, app, cx| {
            if this.is_rendering {
                return;
            }
            let next = preview_fingerprint(app.read(cx));
            if this.fingerprint != next {
                this.fingerprint = next;
                crate::perf::count("inspector.invalidate.changed");
                cx.notify();
            } else {
                crate::perf::count("inspector.invalidate.skipped");
            }
        })
        .detach();
        Self {
            app: app.downgrade(),
            fingerprint: 0,
            is_rendering: false,
        }
    }
}

impl Render for InspectorHost {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.is_rendering = true;
        let el = match self.app.upgrade() {
            Some(app) => app.update(cx, |app, cx| {
                app.pending_drag_hover_targets.retain(|target| {
                    !matches!(target.id, crate::ui::drag_hover::DragHoverId::Inspector(_))
                });
                crate::perf::count("render.inspector_body");
                app.render_inspector(window, cx)
            }),
            None => div().into_any_element(),
        };
        self.is_rendering = false;
        el
    }
}

/// The inspector only depends on the focused tab's selection/preview result,
/// display knobs, and its image/text interaction state. Hashing metadata and
/// lengths instead of RGBA/text contents keeps the observer cheap while still
/// invalidating on each meaningful result transition.
fn preview_fingerprint(app: &App) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    app.drag_hover_maybe_active().hash(&mut hasher);
    let tab = app.state.focused_tile;
    tab.hash(&mut hasher);
    app.state.settings.show_inspector.hash(&mut hasher);
    app.state.settings.show_titlebar.hash(&mut hasher);
    app.state.profiles.active().theme.hash(&mut hasher);
    app.state.profiles.active().name.hash(&mut hasher);
    app.state.settings.global_zoom.to_bits().hash(&mut hasher);
    for tag in app
        .state
        .tag_cache
        .visible_defs(&app.state.profiles.active().name)
    {
        tag.id.hash(&mut hasher);
        tag.name.hash(&mut hasher);
        tag.color.hash(&mut hasher);
        tag.profile.hash(&mut hasher);
    }
    if app.preview_visible_for_focused() {
        for path in app.selected_paths() {
            path.hash(&mut hasher);
            app.state.tag_cache.tags_of(&path).hash(&mut hasher);
        }
    }
    app.state.settings.glass_opacity.to_bits().hash(&mut hasher);
    app.state
        .settings
        .inspector_width
        .to_bits()
        .hash(&mut hasher);
    app.preview_visible_for_focused().hash(&mut hasher);
    if app.preview_visible_for_focused() {
        app.state.active_preview_request().hash(&mut hasher);
    }

    if let Some(tab_data) = app.state.active_tab_data() {
        tab_data.preview_enabled.hash(&mut hasher);
    }
    if let Some(tab_state) = app.state.tab_states.get(&tab) {
        tab_state.selection.selected.len().hash(&mut hasher);
        tab_state.selection.keyboard_cursor.hash(&mut hasher);
    }

    if let Some(path) = app.state.preview.paths.get(&tab) {
        1u8.hash(&mut hasher);
        path.hash(&mut hasher);
    } else {
        0u8.hash(&mut hasher);
    }

    hash_preview_result(app.state.preview.results.get(&tab), &mut hasher);
    if let Some(PreviewResult::Dir { entries, .. }) = app.state.preview.results.get(&tab) {
        for path in dir_media_paths(entries) {
            app.grid_thumbnail(path).is_some().hash(&mut hasher);
        }
    }
    app.state.preview.metadata.get(&tab).hash(&mut hasher);

    if let Some(multi) = app.state.preview.multi.get(&tab) {
        1u8.hash(&mut hasher);
        multi.key.fingerprint.hash(&mut hasher);
        multi.key.selected_count.hash(&mut hasher);
        multi.images.len().hash(&mut hasher);
        for (path, image) in &multi.images {
            path.hash(&mut hasher);
            image.width.hash(&mut hasher);
            image.height.hash(&mut hasher);
            image.rgba.len().hash(&mut hasher);
        }
    } else {
        0u8.hash(&mut hasher);
    }

    if let Some(view) = app.preview_view.get(&tab) {
        view.zoom.to_bits().hash(&mut hasher);
        view.pan_x.to_bits().hash(&mut hasher);
        view.pan_y.to_bits().hash(&mut hasher);
        view.expanded.hash(&mut hasher);
        view.details_open.hash(&mut hasher);
        view.stack_index.hash(&mut hasher);
        view.stack_drag.map(f32::to_bits).hash(&mut hasher);
        view.stack_dx.to_bits().hash(&mut hasher);
        view.viewport
            .map(|bounds| {
                (
                    f32::from(bounds.size.width).to_bits(),
                    f32::from(bounds.size.height).to_bits(),
                )
            })
            .hash(&mut hasher);
    }

    if let Some(editor) = app.preview_editor.as_ref() {
        editor.path.hash(&mut hasher);
        editor.dirty.hash(&mut hasher);
        editor.editing.hash(&mut hasher);
        editor.line_numbers.hash(&mut hasher);
        editor.ln_needs_sync.hash(&mut hasher);
    }
    hasher.finish()
}

fn hash_preview_result(
    result: Option<&PreviewResult>,
    hasher: &mut std::collections::hash_map::DefaultHasher,
) {
    match result {
        None => 0u8.hash(hasher),
        Some(PreviewResult::Text { lines, text }) => {
            1u8.hash(hasher);
            lines.len().hash(hasher);
            text.len().hash(hasher);
        }
        Some(PreviewResult::Markdown {
            blocks,
            source,
            base_uri,
        }) => {
            2u8.hash(hasher);
            blocks.len().hash(hasher);
            source.len().hash(hasher);
            base_uri.hash(hasher);
        }
        Some(PreviewResult::Image {
            rgba,
            width,
            height,
            original_width,
            original_height,
            exif,
        }) => {
            3u8.hash(hasher);
            rgba.len().hash(hasher);
            width.hash(hasher);
            height.hash(hasher);
            original_width.hash(hasher);
            original_height.hash(hasher);
            exif.len().hash(hasher);
        }
        Some(PreviewResult::Pdf {
            rgba,
            width,
            height,
        }) => {
            4u8.hash(hasher);
            rgba.len().hash(hasher);
            width.hash(hasher);
            height.hash(hasher);
        }
        Some(PreviewResult::Table {
            headers,
            rows,
            truncated,
        }) => {
            5u8.hash(hasher);
            headers.len().hash(hasher);
            rows.len().hash(hasher);
            truncated.hash(hasher);
        }
        Some(PreviewResult::Document {
            title,
            blocks,
            truncated,
        }) => {
            6u8.hash(hasher);
            title.hash(hasher);
            blocks.len().hash(hasher);
            truncated.hash(hasher);
        }
        Some(PreviewResult::Presentation { slides, truncated }) => {
            7u8.hash(hasher);
            slides.len().hash(hasher);
            slides
                .iter()
                .map(|slide| slide.body.len())
                .sum::<usize>()
                .hash(hasher);
            truncated.hash(hasher);
        }
        Some(PreviewResult::Audio {
            title,
            artist,
            album,
            duration_secs,
            bitrate,
            cover,
        }) => {
            8u8.hash(hasher);
            title.hash(hasher);
            artist.hash(hasher);
            album.hash(hasher);
            duration_secs.hash(hasher);
            bitrate.hash(hasher);
            cover
                .as_ref()
                .map(|image| (image.width, image.height, image.rgba.len()))
                .hash(hasher);
        }
        Some(PreviewResult::Dir {
            entries,
            total_count,
            total_size,
        }) => {
            9u8.hash(hasher);
            entries.len().hash(hasher);
            total_count.hash(hasher);
            total_size.hash(hasher);
        }
        Some(PreviewResult::Binary { size }) => {
            10u8.hash(hasher);
            size.hash(hasher);
        }
        Some(PreviewResult::Error(message)) => {
            11u8.hash(hasher);
            message.hash(hasher);
        }
    }
}

impl App {
    /// Get-or-create the retained preview body host. The surrounding panel
    /// remains in `root.rs` so width and exit animations continue to work.
    pub(crate) fn ensure_inspector_host(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Entity<InspectorHost> {
        if let Some(host) = &self.inspector_host {
            return host.clone();
        }
        let app = cx.entity();
        let host = cx.new(|cx| InspectorHost::new(app, cx));
        self.inspector_host = Some(host.clone());
        host
    }
}

fn preview_source_name(path: &Path) -> PathBuf {
    #[cfg(feature = "provider-host")]
    if path.to_string_lossy().starts_with("remote-preview:") {
        return atlas_model::remote_path::file_name(&path.to_string_lossy()).into();
    }
    path.to_path_buf()
}

fn preview_language_for_path(path: &Path) -> &'static str {
    let display_path = preview_source_name(path);
    let path = display_path.as_path();
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| {
            path.file_name()
                .and_then(|f| f.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase()
        });
    match ext.as_str() {
        "rs" => "rust",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "py" | "pyw" => "python",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" => "cpp",
        "cs" | "csx" => "c_sharp",
        "rb" | "rake" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "sh" | "bash" | "zsh" => "bash",
        "ps1" => "powershell",
        "html" | "htm" | "xhtml" => "html",
        "css" | "scss" | "sass" | "less" => "css",
        "json" | "jsonc" | "json5" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "xml" | "svg" => "xml",
        "md" | "markdown" | "mdx" => "markdown",
        "sql" => "sql",
        "zig" => "zig",
        "lua" => "lua",
        "dart" => "dart",
        "vue" => "vue",
        "svelte" => "svelte",
        "astro" => "astro",
        "dockerfile" | "containerfile" => "dockerfile",
        "makefile" => "makefile",
        _ => "text",
    }
}

/// Upload tightly-packed RGBA8 (`width*height*4`) to a gpui `RenderImage`. gpui's
/// renderer expects BGRA, so we swap R/B per pixel (same as gpui's own decoder).
pub fn rgba_to_render_image(rgba: &[u8], width: usize, height: usize) -> Option<Arc<RenderImage>> {
    if width == 0 || height == 0 || rgba.len() < width * height * 4 {
        return None;
    }
    let mut buf = rgba[..width * height * 4].to_vec();
    for pixel in buf.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let image = image::RgbaImage::from_raw(width as u32, height as u32, buf)?;
    let frame = image::Frame::new(image);
    Some(Arc::new(RenderImage::new(vec![frame])))
}

impl App {
    // ── Code editor lifecycle ────────────────────────────────────────────────

    /// Ensure `self.preview_editor` exists and matches `path`/`lang`. If the path or
    /// language changed (new file selected), the old editor is dropped and a fresh
    /// `InputState` is created. Returns `true` when the editor is ready.
    fn ensure_preview_editor(
        &mut self,
        path: PathBuf,
        text: &str,
        lang: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let same = self.preview_editor.as_ref().is_some_and(|e| e.path == path);
        if same {
            return true;
        }
        let display_path = preview_source_name(&path);
        let ext = display_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                path.file_name()
                    .and_then(|f| f.to_str())
                    .unwrap_or("")
                    .to_string()
            });
        let show_ln = self
            .state
            .profiles
            .active()
            .scoped::<atlas_model::profiles::PreviewFeature>()
            .show_line_numbers;
        // New file — build a fresh code editor with syntax highlighting.
        let state_entity = crate::ui::text_input::code_editor(
            window,
            cx,
            gpui::SharedString::from(lang.to_string()),
            text,
            show_ln,
        );
        let syntax_provider = std::rc::Rc::new(
            crate::ui::syntax_provider::AtlasSyntaxTokensProvider::new(&ext),
        );
        state_entity.update(cx, |state, _cx| {
            state.lsp.semantic_tokens_provider = Some(syntax_provider);
        });
        let text_str = text.to_string();
        let sub = cx.subscribe_in(
            &state_entity,
            window,
            |this, st, ev: &InputEvent, _w, cx| {
                if matches!(ev, InputEvent::Change) {
                    let new_text = st.read(cx).value().to_string();
                    if let Some(editor) = &mut this.preview_editor {
                        editor.cached_text = new_text;
                        editor.dirty = true;
                    }
                    cx.notify();
                }
            },
        );
        self.preview_editor = Some(crate::app::PreviewEditor {
            state: state_entity,
            path,
            cached_text: text_str,
            dirty: false,
            editing: true,
            line_numbers: show_ln,
            ln_needs_sync: false,
            _sub: sub,
        });
        true
    }

    // ── Preview loading (gpui variants, no egui ctx) ─────────────────────────

    /// gpui replacement for `sync_active_preview(ctx)`: decides what (if anything)
    /// should be previewed for the focused tab and kicks off a debounced load. The
    /// drain loop handles repaints, so no `ctx.request_repaint` is needed.
    pub(crate) fn sync_active_preview_gpui(&mut self) {
        let tab = self.state.focused_tile;
        if !self.preview_visible_for_focused() {
            self.cancel_pending_preview();
            return;
        }
        let request = self.state.active_preview_request();
        let Some(request) = request else {
            self.cancel_pending_preview();
            self.state.clear_preview(tab);
            return;
        };

        let already_loaded = match &request {
            PreviewRequest::Single(path) => self.state.preview.paths.get(&tab) == Some(path),
            PreviewRequest::ArchiveSingle { inner, .. } => {
                self.state.preview.paths.get(&tab).map(PathBuf::as_path) == Some(inner.as_path())
            }
            PreviewRequest::Multiple { key, .. } => self
                .state
                .preview
                .multi
                .get(&tab)
                .is_some_and(|multi| &multi.key == key),
        };
        if already_loaded {
            self.cancel_pending_preview();
            return;
        }
        self.preview_delay
            .schedule(tab, request, &self.rt, self.tx.clone());
    }

    fn cancel_pending_preview(&mut self) {
        self.preview_delay.cancel();
    }

    pub(crate) fn load_delayed_preview(
        &mut self,
        tab: TileId,
        request: PreviewRequest,
        generation: u64,
    ) {
        if !self.preview_delay.accepts(tab, &request, generation) {
            return;
        }
        if self.state.focused_tile != tab
            || !self.preview_visible_for_focused()
            || self.state.active_preview_request().as_ref() != Some(&request)
        {
            self.cancel_pending_preview();
            return;
        }
        self.cancel_pending_preview();
        match request {
            PreviewRequest::Single(path) => self.load_preview_gpui(tab, path),
            PreviewRequest::ArchiveSingle { archive, inner } => {
                self.load_archive_preview_gpui(tab, archive, inner);
            }
            PreviewRequest::Multiple { key, paths } => {
                self.load_multi_preview_gpui(tab, key, paths);
            }
        }
    }

    /// Is the preview panel visible? It is window-level and always previews the
    /// focused pane, so moving focus between panes never opens or closes it.
    pub(crate) fn preview_visible_for_focused(&self) -> bool {
        self.state.settings.show_inspector
    }

    pub(crate) fn load_preview_gpui(&mut self, tab: TileId, path: PathBuf) {
        #[cfg(feature = "provider-host")]
        if self.load_cloud_preview_for_selection() { return; }
        if self.state.preview.paths.get(&tab).map(PathBuf::as_path) == Some(path.as_path()) {
            return;
        }
        // New selection: reset zoom/pan (keep `expanded` so browsing in the big view
        // stays open).
        if let Some(v) = self.preview_view.get_mut(&tab) {
            v.zoom = 1.0;
            v.pan_x = 0.0;
            v.pan_y = 0.0;
        }
        self.preview_drag = None;
        // `results` stays until the new one lands so the panel never flashes
        // empty; `result_paths` tells the renderers which file it belongs to.
        self.state.preview.paths.insert(tab, path.clone());
        self.state.preview.metadata.remove(&tab);
        self.state.preview.multi.remove(&tab);
        let tx = self.tx.clone();
        if atlas_core::vault::live::is_private(&path) {
            self.rt.spawn(async move { atlas_core::fs::preview::load_preview(tab, path, tx).await; });
            return;
        }
        if atlas_core::fs::preview::is_cloud_placeholder(&path) {
            let _ = tx.send(atlas_model::app_state::BgResult::PreviewReady {
                tab,
                path,
                result: PreviewResult::Error(rust_i18n::t!("preview_cloud_offline").to_string()),
            });
            return;
        }
        let metadata_path = path.clone();
        let metadata_tx = tx.clone();
        self.rt.spawn(async move {
            atlas_core::fs::preview::metadata::load(tab, metadata_path, metadata_tx).await;
        });
        #[cfg(windows)]
        if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")) {
            self.rt.spawn(async move {
                let p = path.clone();
                let result = tokio::task::spawn_blocking(move || atlas_core::fs::preview::preview_pdf_public(&p)).await
                    .unwrap_or_else(|error| PreviewResult::Error(error.to_string()));
                let _ = tx.send(atlas_model::app_state::BgResult::PreviewReady { tab, path, result });
            });
            return;
        }
        let previewer = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(|ext| self.ext_registry.previewer_for(ext));
        if let Some(cmd) = previewer {
            atlas_core::ext::runner::run_preview(&self.rt, tx, cmd, tab, path);
        } else if let Some((extension, missing)) = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(|ext| self.ext_registry.missing_preview_tools_for(ext))
        {
            let installing = if let Some(request) = path
                .extension()
                .and_then(|e| e.to_str())
                .and_then(|ext| self.ext_registry.missing_preview_tool_install_for(ext))
            {
                let tx2 = tx.clone();
                let retry_path = path.clone();
                let tool = request.tool.name.clone();
                self.rt.spawn(async move {
                    let result = atlas_core::ext::marketplace::install_tool(
                        request.provider_dir,
                        request.tool,
                    )
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string());
                    let _ = tx2.send(atlas_model::app_state::BgResult::ExtToolInstalled {
                        tool,
                        tab,
                        path: retry_path,
                        result,
                    });
                });
                true
            } else {
                false
            };
            let tools = missing
                .iter()
                .map(|name| tool_label(name))
                .collect::<Vec<_>>()
                .join(", ");
            let result = PreviewResult::Error(if installing {
                format!("Installing {tools} for {extension}...")
            } else {
                format!("{extension} needs {tools}. Update Atlas Runtime Tools.")
            });
            let _ = tx.send(atlas_model::app_state::BgResult::PreviewReady { tab, path, result });
        } else {
            #[cfg(windows)]
            if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| atlas_model::preview_cache::is_video_ext(&e.to_lowercase()))
            {
                // Explorer's thumbnail cache is near-instant; ffmpeg is the fallback.
                self.rt.spawn(async move {
                    let p = path.clone();
                    let fast = tokio::task::spawn_blocking(move || {
                        let probe_path = p.clone();
                        let probe = std::thread::spawn(move || atlas_core::fs::preview::video_exif(&probe_path));
                        let thumb = crate::shell_thumb::shell_thumbnail(&p, 1024);
                        (thumb, probe.join().unwrap_or_default())
                    })
                    .await
                    .ok();
                    if let Some((Some((rgba, width, height)), exif)) = fast {
                        let result = PreviewResult::Image {
                            rgba,
                            width,
                            height,
                            original_width: width,
                            original_height: height,
                            exif,
                        };
                        let _ = tx.send(atlas_model::app_state::BgResult::PreviewReady { tab, path, result });
                    } else {
                        atlas_core::fs::preview::load_preview(tab, path, tx).await;
                    }
                });
                return;
            }
            self.rt.spawn(async move {
                atlas_core::fs::preview::load_preview(tab, path, tx).await;
            });
        }
    }

    fn load_archive_preview_gpui(&mut self, tab: TileId, archive: PathBuf, inner: PathBuf) {
        if self.state.preview.paths.get(&tab).map(PathBuf::as_path) == Some(inner.as_path()) {
            return;
        }
        if let Some(v) = self.preview_view.get_mut(&tab) {
            v.zoom = 1.0;
            v.pan_x = 0.0;
            v.pan_y = 0.0;
        }
        self.preview_drag = None;
        self.state.preview.paths.insert(tab, inner.clone());
        self.state.preview.multi.remove(&tab);
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            atlas_core::fs::preview::load_archive_preview(tab, archive, inner, tx).await;
        });
    }

    fn load_multi_preview_gpui(
        &mut self,
        tab: TileId,
        key: atlas_model::preview_cache::MultiPreviewKey,
        paths: Vec<PathBuf>,
    ) {
        if self
            .state
            .preview
            .multi
            .get(&tab)
            .is_some_and(|multi| multi.key == key)
        {
            return;
        }
        self.state.clear_preview(tab);
        self.preview_textures.remove(&tab);
        self.multi_textures.remove(&tab);
        if let Some(view) = self.preview_view.get_mut(&tab) {
            view.stack_index = 0;
            view.stack_drag = None;
            view.stack_dx = 0.0;
        }
        self.state.preview.multi.insert(
            tab,
            MultiPreviewState {
                key: key.clone(),
                paths: paths.clone(),
                images: Vec::with_capacity(paths.len()),
            },
        );
        for (index, path) in paths.into_iter().enumerate() {
            let tx = self.tx.clone();
            let key = key.clone();
            self.rt.spawn(async move {
                atlas_core::fs::preview::load_multi_thumbnail(tab, key, index, path, tx).await;
            });
        }
    }

    /// Path the stored preview result belongs to; during a load this is still
    /// the previous file while `paths` already names the next one.
    pub(crate) fn shown_preview_path(&self, tab: TileId) -> Option<PathBuf> {
        let preview = &self.state.preview;
        preview
            .result_paths
            .get(&tab)
            .or_else(|| preview.paths.get(&tab))
            .cloned()
    }

    #[cfg(feature = "ui-lab")]
    pub(crate) fn preview_is_stale_for_lab(&self, tab: TileId) -> bool {
        self.preview_is_stale(tab)
    }

    /// True while the visible result is for a file other than the requested one.
    fn preview_is_stale(&self, tab: TileId) -> bool {
        let preview = &self.state.preview;
        preview.results.contains_key(&tab)
            && preview.paths.get(&tab) != preview.result_paths.get(&tab)
    }

    /// Get-or-build the cached single-preview texture for the focused tab from the
    /// decoded RGBA in `state.preview.results` (Image / Pdf variants). Keyed by path
    /// so a new selection invalidates the old texture.
    fn focused_preview_texture(&mut self) -> Option<Arc<RenderImage>> {
        let tab = self.state.focused_tile;
        let path = self.shown_preview_path(tab)?;
        if let Some((cached_path, tex)) = self.preview_textures.get(&tab) {
            if *cached_path == path {
                return Some(tex.clone());
            }
        }
        let (rgba, w, h) = match self.state.preview.results.get(&tab)? {
            PreviewResult::Image {
                rgba,
                width,
                height,
                ..
            }
            | PreviewResult::Pdf {
                rgba,
                width,
                height,
                ..
            } => (rgba, *width, *height),
            PreviewResult::Audio {
                cover: Some(cover), ..
            } => (&cover.rgba, cover.width, cover.height),
            _ => return None,
        };
        let tex = rgba_to_render_image(rgba, w, h)?;
        self.preview_textures.insert(tab, (path, tex.clone()));
        Some(tex)
    }

    /// Get-or-build the cached multi-selection thumbnail textures for the focused tab.
    fn focused_multi_textures(&mut self) -> Vec<(PathBuf, Arc<RenderImage>)> {
        let tab = self.state.focused_tile;
        let Some(multi) = self.state.preview.multi.get(&tab) else {
            return Vec::new();
        };
        // Rebuild only when the set of decoded images grew.
        let need = multi.images.len();
        let have = self.multi_textures.get(&tab).map_or(0, std::vec::Vec::len);
        if have != need {
            let built: Vec<(PathBuf, Arc<RenderImage>)> = multi
                .images
                .iter()
                .filter_map(|(p, img)| {
                    rgba_to_render_image(&img.rgba, img.width, img.height).map(|t| (p.clone(), t))
                })
                .collect();
            self.multi_textures.insert(tab, built);
        }
        self.multi_textures.get(&tab).cloned().unwrap_or_default()
    }

    /// Toggle the expanded (near-full-window) preview for the focused tab. Bound to
    /// Space. Resets zoom/pan so the new size starts fitted.
    pub(crate) fn toggle_preview_expanded(&mut self) {
        let tab = self.state.focused_tile;
        let v = self.preview_view.entry(tab).or_default();
        v.expanded = !v.expanded;
        v.zoom = 1.0;
        v.pan_x = 0.0;
        v.pan_y = 0.0;
        self.preview_drag = None;
    }

    /// Collapse the expanded preview for the focused tab (Esc / `CloseOverlays`).
    pub(crate) fn collapse_preview_expanded(&mut self) -> bool {
        let tab = self.state.focused_tile;
        match self.preview_view.get_mut(&tab) {
            Some(v) if v.expanded => {
                v.expanded = false;
                v.zoom = 1.0;
                v.pan_x = 0.0;
                v.pan_y = 0.0;
                true
            }
            _ => false,
        }
    }

    fn preview_view_for_focused(&self) -> crate::app::PreviewView {
        self.preview_view
            .get(&self.state.focused_tile)
            .copied()
            .unwrap_or_default()
    }

    // ── Rendering ────────────────────────────────────────────────────────────

    /// The retained preview keeps layout, focus and controls in native elements.
    pub(crate) fn render_inspector(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.sync_active_preview_gpui();
        self.sync_selection_tags();
        // Same material as the file panes (`dock/render.rs`): the list colour on
        // glass/Windows themes, frosted the same way, so the two read as one surface.
        let active_theme = self.state.profiles.active().theme.clone();
        let is_glass = active_theme.ends_with(" Glass");
        let (fg, muted, border, panel_bg) = {
            let theme = cx.theme();
            (
                theme.foreground,
                theme.muted_foreground,
                theme.border,
                if is_glass || crate::ui::icons::is_windows_theme(&active_theme) {
                    theme.colors.list
                } else {
                    theme.sidebar
                },
            )
        };
        let expanded = self.preview_view_for_focused().expanded;
        let summary = self
            .state
            .active_tab_state()
            .zip(self.state.active_tab_data())
            .map(|(tab, data)| selection_summary(tab, &data.view_mode))
            .unwrap_or_default();
        // Nothing selected: the panel describes the folder being browsed
        // instead of showing an empty state.
        let summary = if summary.count == 0 {
            self.folder_summary().unwrap_or(summary)
        } else {
            summary
        };
        let paths = match (&summary.single, summary.count) {
            (Some(entry), 1) => vec![entry.path.clone()],
            _ => self.selected_paths(),
        };
        let title = summary.single.as_ref().map_or_else(
            || {
                if summary.count > 1 {
                    rust_i18n::t!("inspector_elements_selected", count = summary.count).to_string()
                } else {
                    rust_i18n::t!("inspector_title").to_string()
                }
            },
            |entry| entry.name.clone(),
        );
        let locked_path = paths.first().filter(|path| paths.len() == 1 && atlas_core::vault::is_private_path(path)).cloned();
        let subtitle = if locked_path.is_some() {
            rust_i18n::t!("vault_locked_subtitle").to_string()
        } else { preview_summary(
            &summary,
            self.state.preview.results.get(&self.state.focused_tile),
        ) };
        // Quick Look-style header: the title leads; actions stay quiet until the
        // panel is hovered so they never compete with the window caption buttons.
        let mut actions = h_flex()
            .flex_shrink_0()
            .gap_0p5()
            .opacity(0.)
            .group_hover("preview-panel", |s| s.opacity(1.));
        if let Some(entry) = summary.single.as_ref().filter(|_| locked_path.is_none()) {
            let path = entry.path.clone();
            actions = actions.child(
                preview_icon_button(
                    "preview-open-external",
                    gpui_component::IconName::ExternalLink,
                    rust_i18n::t!("inspector_open_external").to_string(),
                    fg,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.process_actions(vec![Action::Open(vec![path.clone()])]);
                    cx.notify();
                })),
            );
        }
        actions = actions
            .child(
                preview_icon_button(
                    "preview-expand",
                    if expanded {
                        gpui_component::IconName::Minimize
                    } else {
                        gpui_component::IconName::Maximize
                    },
                    rust_i18n::t!(if expanded {
                        "inspector_collapse"
                    } else {
                        "inspector_expand"
                    })
                    .to_string(),
                    fg,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.toggle_preview_expanded();
                    cx.notify();
                })),
            )
            .child(
                preview_icon_button(
                    "preview-close",
                    gpui_component::IconName::PanelRightClose,
                    rust_i18n::t!("inspector_close").to_string(),
                    muted,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.process_actions(vec![Action::ToggleInspector]);
                    this.preview_drag = None;
                    cx.notify();
                })),
            );
        let header = h_flex()
            .flex_shrink_0()
            .min_w(px(0.))
            .items_start()
            .gap_2()
            .pb_3()
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .gap_0p5()
                    .child(
                        div()
                            .truncate()
                            .text_sm()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .when(!subtitle.is_empty(), |col| {
                        col.child(div().truncate().text_xs().text_color(muted).child(subtitle))
                    }),
            )
            .child(actions);
        let mut panel = v_flex()
            .id("native-preview-panel")
            .group("preview-panel")
            .size_full()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_hidden()
            .text_color(fg)
            .when(is_glass, |s| s.glass_surface(panel_bg))
            .when(!is_glass, |s| s.bg(panel_bg).rounded(px(8.)))
            .px_3()
            .py_3()
            .when(!expanded && self.state.settings.show_titlebar, |panel| {
                panel.pt(crate::ui::rem_px(
                    if crate::ui::icons::is_windows_theme(
                        self.state.profiles.active().theme.as_str(),
                    ) {
                        46.
                    } else {
                        34.
                    },
                ))
            })
            .child(header);
        if let Some(path) = locked_path {
            return panel.child(self.render_locked_vault_preview(path, cx)).into_any_element();
        }
        if summary.count == 0 {
            return panel
                .child(
                    v_flex()
                        .flex_1()
                        .min_h(px(0.))
                        .items_center()
                        .justify_center()
                        .gap_2()
                        .px_3()
                        .text_center()
                        .text_color(muted)
                        .child(icon(AtlasIcon::Eye, 24., muted))
                        .child(
                            div()
                                .text_sm()
                                .child(rust_i18n::t!("inspector_no_selection").to_string()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .child(rust_i18n::t!("inspector_empty_hint").to_string()),
                        ),
                )
                .into_any_element();
        }
        let body = if summary.count > 1 {
            self.render_multi(&summary, muted, window, cx)
        } else {
            self.render_preview_body(muted, cx.theme().danger, window, cx)
        };
        panel = panel.child(
            v_flex()
                .relative()
                .flex_1()
                .min_h(px(0.))
                .min_w(px(0.))
                .overflow_hidden()
                .child(body),
        );
        // Always reserve a reachable footer for tags; details never push it offscreen.
        let tags = if paths.iter().any(|path| atlas_core::vault::live::is_private(path)) {
            div().into_any_element()
        } else if summary.count > 1 {
            self.render_multi_tags(&paths, muted, cx)
        } else if let Some(path) = paths.first() {
            self.render_inspector_tags(path, muted, cx)
        } else {
            div().into_any_element()
        };
        panel = panel.child(
            v_flex()
                .flex_shrink_0()
                .min_w(px(0.))
                .gap_2()
                .pt_2()
                .child(
                    div()
                        .id("preview-tags")
                        .max_h(crate::ui::rem_px(64.))
                        .overflow_y_scroll()
                        .child(tags),
                )
                .child(
                    h_flex()
                        .justify_between()
                        .gap_2()
                        .min_w(px(0.))
                        .child(self.render_tag_section(paths, muted, cx))
                        .child(
                            gpui_component::button::Button::new("preview-details")
                                .small()
                                .compact()
                                .ghost()
                                .map(|b| { use gpui_component::Selectable as _; b.selected(self.preview_view_for_focused().details_open) })
                                .tooltip(rust_i18n::t!("inspector_details").to_string())
                                .child(
                                    gpui_component::Icon::new(gpui_component::IconName::Info)
                                        .with_size(gpui_component::Size::Small)
                                        .text_color(muted),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let state = this
                                        .preview_view
                                        .entry(this.state.focused_tile)
                                        .or_default();
                                    state.details_open = !state.details_open;
                                    cx.notify();
                                }))
                                .map(|button| {
                                    accessible_button(
                                        "preview-details",
                                        rust_i18n::t!("inspector_details").to_string(),
                                        button,
                                    )
                                }),
                        ),
                ),
        );
        if self.preview_view_for_focused().details_open {
            let details = self.render_preview_details(&summary, muted, window, cx);
            panel = panel.child(
                div()
                    .flex_shrink_0()
                    .mt_2()
                    .pt_2()
                    .border_t_1()
                    .border_color(border.opacity(0.35))
                    .child(details),
            );
        }
        panel.into_any_element()
    }

    pub(crate) fn render_expanded_preview(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let top = if self.state.settings.show_titlebar {
            if crate::ui::icons::is_windows_theme(self.state.profiles.active().theme.as_str()) {
                46.
            } else {
                34.
            }
        } else {
            0.
        };
        div()
            .absolute()
            .inset_0()
            .bg(gpui::hsla(0., 0., 0., 0.36))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.collapse_preview_expanded();
                    cx.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .left_3()
                    .right_3()
                    .bottom_3()
                    .top(crate::ui::rem_px(top + 8.))
                    .overflow_hidden()
                    .rounded(px(10.))
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(self.ensure_inspector_host(cx)),
            )
            .into_any_element()
    }

    fn render_preview_details(
        &self,
        summary: &SelectionSummary,
        muted: Hsla,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let mut properties = Vec::new();
        if let Some(entry) = &summary.single {
            properties.push((
                rust_i18n::t!("inspector_location").to_string(),
                entry.path.display().to_string(),
            ));
            if let Some(modified) = entry.modified {
                properties.push((
                    rust_i18n::t!("inspector_modified_label").to_string(),
                    format_date(modified),
                ));
            }
            if let Some(ext) = &entry.extension {
                properties.push((
                    rust_i18n::t!("inspector_type_label").to_string(),
                    ext.to_uppercase(),
                ));
            }
            if let Some((path, metadata)) =
                self.state.preview.metadata.get(&self.state.focused_tile)
            {
                if path == &entry.path {
                    properties.extend(metadata.iter().cloned());
                }
            }
        }
        if let Some(PreviewResult::Image { exif, .. }) =
            self.state.preview.results.get(&self.state.focused_tile)
        {
            if !self.preview_is_stale(self.state.focused_tile) {
                properties.extend(exif.iter().take(30).cloned());
            }
        }
        if summary.count > 1 {
            properties.extend(self.selected_paths().into_iter().take(20).map(|path| {
                (
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    path.display().to_string(),
                )
            }));
        }
        let mut seen = std::collections::HashSet::new();
        properties.retain(|(key, _)| seen.insert(key.to_lowercase()));
        let foreground = cx.theme().foreground;
        let max_h = (f32::from(window.viewport_size().height) * 0.24).clamp(70., 190.);
        div()
            .id("preview-metadata")
            .max_h(px(max_h))
            .overflow_y_scroll()
            .child(
                v_flex()
                    .gap_2()
                    .children(properties.into_iter().map(|(label, value)| {
                        let tooltip = value.clone();
                        h_flex()
                            .gap_2()
                            .min_w(px(0.))
                            .text_xs()
                            .child(
                                div()
                                    .w(crate::ui::rem_px(76.))
                                    .flex_shrink_0()
                                    .text_color(muted)
                                    .truncate()
                                    .child(label.clone()),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("preview-property-{label}")))
                                    .flex_1()
                                    .min_w(px(0.))
                                    .text_color(foreground)
                                    .truncate()
                                    .child(value)
                                    .tooltip(move |window, cx| {
                                        gpui_component::tooltip::Tooltip::new(tooltip.clone())
                                            .build(window, cx)
                                    }),
                            )
                    })),
            )
            .into_any_element()
    }

    fn render_floating_preview_controls(
        &mut self,
        kind: PreviewKind,
        copy_text: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let is_image = matches!(kind, PreviewKind::Image | PreviewKind::Pdf);
        if !is_image && copy_text.is_none() {
            return div().into_any_element();
        }

        let (surface, border, fg, muted) = {
            let t = cx.theme();
            (t.secondary, t.border, t.foreground, t.muted_foreground)
        };
        let view = cx.entity();

        let mut bar = h_flex()
            .absolute()
            .top(px(10.))
            .right(px(10.))
            .gap_1()
            .items_center()
            .p(px(3.))
            .rounded(px(999.))
            .bg(layer_color(surface, 0.06, 0.72))
            .border_1()
            .border_color(border.opacity(0.24))
            .raised_surface()
            .invisible()
            .group_hover("preview-surface", gpui::Styled::visible);

        if is_image {
            let v_zo = view.clone();
            bar = bar.child(
                floating_icon_button(
                    "float-zout",
                    gpui_component::IconName::Minus,
                    rust_i18n::t!("inspector_preview_zoom_out").to_string(),
                    false,
                    fg,
                    muted,
                )
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    v_zo.update(cx, |this, _cx| {
                        this.process_actions(vec![Action::PreviewZoomOut]);
                    });
                }),
            );

            let v_fit = view.clone();
            bar = bar.child(
                floating_icon_button(
                    "float-fit",
                    gpui_component::IconName::Maximize,
                    rust_i18n::t!("inspector_preview_zoom_fit").to_string(),
                    false,
                    fg,
                    muted,
                )
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    v_fit.update(cx, |this, _cx| {
                        this.process_actions(vec![Action::PreviewZoomFit]);
                    });
                }),
            );

            let v_zi = view.clone();
            bar = bar.child(
                floating_icon_button(
                    "float-zin",
                    gpui_component::IconName::Plus,
                    rust_i18n::t!("inspector_preview_zoom_in").to_string(),
                    false,
                    fg,
                    muted,
                )
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    v_zi.update(cx, |this, _cx| {
                        this.process_actions(vec![Action::PreviewZoomIn]);
                    });
                }),
            );
        }

        if let Some(text) = copy_text {
            bar = bar.child(
                gpui_component::clipboard::Clipboard::new("preview-copy")
                    .value(text)
                    .tooltip(rust_i18n::t!("inspector_preview_copy").to_string()),
            );
        }

        bar.into_any_element()
    }

    fn render_multi_tags(
        &mut self,
        paths: &[PathBuf],
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_tag_chips(paths, muted, cx)
    }

    fn render_inspector_tags(
        &mut self,
        path: &Path,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.render_tag_chips(&[path.to_path_buf()], muted, cx)
    }

    fn render_tag_chips(&self, paths: &[PathBuf], muted: Hsla, cx: &Context<Self>) -> AnyElement {
        let profile = &self.state.profiles.active().name;
        let tags: Vec<_> = self
            .state
            .tag_cache
            .visible_defs(profile)
            .filter(|tag| {
                !paths.is_empty()
                    && paths
                        .iter()
                        .all(|path| self.state.tag_cache.tags_of(path).contains(&tag.id))
            })
            .cloned()
            .collect();
        let app = cx.entity();
        h_flex()
            .flex_wrap()
            .gap_1()
            .min_w(px(0.))
            .children(tags.into_iter().map(|tag| {
                let [r, g, b] = tag.color;
                let swatch = gpui::rgb((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b));
                let paths = paths.to_vec();
                let app = app.clone();
                let name = tag.name.clone();
                let remove_label =
                    rust_i18n::t!("inspector_remove_tag", name = tag.name).to_string();
                h_flex()
                    .gap_1()
                    .pl_2()
                    .rounded(px(5.))
                    .bg(muted.opacity(0.08))
                    .child(div().size(px(6.)).rounded_full().bg(swatch))
                    .child(
                        div()
                            .text_xs()
                            .max_w(crate::ui::rem_px(108.))
                            .truncate()
                            .child(name),
                    )
                    .child(accessible_button(
                        format!("preview-remove-tag-{}", tag.id.0),
                        remove_label.clone(),
                        gpui_component::button::Button::new((
                            "preview-remove-tag",
                            tag.id.0 as u64,
                        ))
                        .tooltip(remove_label)
                        .xsmall()
                        .compact()
                        .ghost()
                        .child(icon(AtlasIcon::X, 10., muted))
                        .on_click(move |_, _, cx| {
                            app.update(cx, |this, cx| {
                                this.process_actions(vec![Action::UnassignTagFromFiles {
                                    tag: tag.id,
                                    paths: paths.clone(),
                                }]);
                                cx.notify();
                            });
                        }),
                    ))
            }))
            .into_any_element()
    }

    /// Tags stay reachable for every preview type and accept keyboard activation.
    fn render_tag_section(
        &mut self,
        paths: Vec<PathBuf>,
        muted: Hsla,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if paths.iter().any(|path| atlas_core::vault::live::is_private(path)) {
            return div().into_any_element();
        }
        gpui_component::button::Button::new("preview-add-tag")
            .small()
            .compact()
            .ghost()
            .tooltip(rust_i18n::t!("inspector_add_tag").to_string())
            .child(
                h_flex()
                    .gap_0p5()
                    .items_center()
                    .child(icon(AtlasIcon::Tag, 14., muted))
                    .child(icon(AtlasIcon::Plus, 10., muted)),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.open_tag_picker(event.position(), paths.clone(), window, cx);
                cx.notify();
            }))
            .map(|button| {
                accessible_button(
                    "preview-add-tag",
                    rust_i18n::t!("inspector_add_tag").to_string(),
                    button,
                )
            })
            .into_any_element()
    }

    /// The content-preview portion for a single selection. While the next file
    /// loads the previous result stays up, dimmed, instead of flashing blank.
    fn render_preview_body(
        &mut self,
        muted: Hsla,
        danger: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let stale = self.preview_is_stale(self.state.focused_tile);
        let body = self.render_preview_content(muted, danger, window, cx);
        if !stale {
            return body;
        }
        v_flex()
            .flex_1()
            .w_full()
            .min_h(px(0.))
            .opacity(0.55)
            .child(body)
            .into_any_element()
    }

    fn render_preview_content(
        &mut self,
        muted: Hsla,
        danger: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tab = self.state.focused_tile;
        // Borrow the result immutably only to branch; texture build needs &mut self,
        // so resolve the texture first where needed.
        let kind = match self.state.preview.results.get(&tab) {
            None => PreviewKind::Loading,
            Some(PreviewResult::Text { .. }) => PreviewKind::Text,
            Some(PreviewResult::Markdown { .. }) => PreviewKind::Markdown,
            Some(PreviewResult::Image { .. }) => PreviewKind::Image,
            Some(PreviewResult::Pdf { .. }) => PreviewKind::Pdf,
            Some(PreviewResult::Table { .. }) => PreviewKind::Table,
            Some(PreviewResult::Document { .. }) => PreviewKind::Document,
            Some(PreviewResult::Presentation { .. }) => PreviewKind::Presentation,
            Some(PreviewResult::Audio { .. }) => PreviewKind::Audio,
            Some(PreviewResult::Dir { .. }) => PreviewKind::Dir,
            Some(PreviewResult::Binary { .. }) => PreviewKind::Binary,
            Some(PreviewResult::Error(_)) => PreviewKind::Error,
        };

        match kind {
            PreviewKind::Loading => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(muted)
                .child(rust_i18n::t!("inspector_loading").to_string())
                .into_any_element(),
            PreviewKind::Text => self.render_text_preview(muted, window, cx),
            PreviewKind::Markdown => self.render_markdown_preview(muted, window, cx),
            PreviewKind::Image | PreviewKind::Pdf => self.render_image_preview(muted, cx),
            PreviewKind::Table => self.render_table_preview(muted, cx),
            PreviewKind::Document => self.render_document_preview(muted),
            PreviewKind::Presentation => self.render_presentation_preview(muted),
            PreviewKind::Audio => self.render_audio_preview(muted),
            PreviewKind::Dir => self.render_dir_preview(muted, cx),
            PreviewKind::Binary => {
                let size = match self.state.preview.results.get(&tab) {
                    Some(PreviewResult::Binary { size }) => *size,
                    _ => 0,
                };
                div()
                    .flex_1()
                    .flex()
                    .text_color(muted)
                    .items_center()
                    .justify_center()
                    .child(rust_i18n::t!("inspector_binary", size = format_size(size)).to_string())
                    .into_any_element()
            }
            PreviewKind::Error => {
                let msg = match self.state.preview.results.get(&tab) {
                    Some(PreviewResult::Error(e)) => e.clone(),
                    _ => String::new(),
                };
                let lines = wrap_preview_error(&msg);
                v_flex()
                    .flex_1()
                    .min_w(px(0.))
                    .p_2()
                    .gap_1()
                    .text_color(danger)
                    .items_center()
                    .justify_center()
                    .children(lines.into_iter().map(|line| {
                        div()
                            .w_full()
                            .min_w(px(0.))
                            .text_center()
                            .child(line)
                            .into_any_element()
                    }))
                    .into_any_element()
            }
        }
    }

    /// Code/text preview with real syntax highlighting via gpui-component's code editor.
    fn render_text_preview(
        &mut self,
        _muted: Hsla,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tab = self.state.focused_tile;
        let Some(PreviewResult::Text { text, .. }) = self.state.preview.results.get(&tab) else {
            return div().into_any_element();
        };
        let raw = text.clone();
        let path = self
            .shown_preview_path(tab)
            .unwrap_or_else(|| PathBuf::from("<preview>"));
        let lang = preview_language_for_path(&path);

        // Ensure the editor entity exists (creates lazily or on file change).
        self.ensure_preview_editor(path, &raw, lang, window, cx);

        let Some(editor) = self.preview_editor.as_ref() else {
            return div().into_any_element();
        };

        let state_ref = editor.state.clone();
        let (smooth_ease, smooth_boost) =
            crate::app::scroll_smooth_params(self.state.settings.scroll_smoothness);
        state_ref.update(cx, |state, _| {
            state.set_scroll_smooth(smooth_ease, smooth_boost);
        });
        let raw_for_copy = raw.clone();

        let (surface, border) = {
            let t = cx.theme();
            (t.secondary, t.border)
        };

        v_flex()
            .id("inspector-text")
            .group("preview-surface")
            .relative()
            .flex_1()
            .min_h(px(0.))
            .gap_0()
            .bg(layer_color(surface, -0.02, 0.30))
            .border_1()
            .border_color(border.opacity(0.18))
            .rounded(px(10.))
            .overflow_hidden()
            .child(
                gpui_component::input::Input::new(&state_ref)
                    .disabled(editor.path.to_string_lossy().starts_with("remote-preview:"))
                    .h_full()
                    .appearance(false),
            )
            .child(self.render_floating_preview_controls(PreviewKind::Text, Some(raw_for_copy), cx))
            .into_any_element()
    }

    fn render_markdown_preview(
        &mut self,
        _muted: Hsla,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tab = self.state.focused_tile;
        let source = match self.state.preview.results.get(&tab) {
            Some(PreviewResult::Markdown { source, .. }) => source.clone(),
            _ => return div().into_any_element(),
        };
        let (smooth_ease, smooth_boost) =
            crate::app::scroll_smooth_params(self.state.settings.scroll_smoothness);

        v_flex()
            .flex_1()
            .min_h(px(0.))
            .child(
                div()
                    .id("inspector-md")
                    .group("preview-surface")
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        gpui_component::text::TextView::markdown(
                            gpui::SharedString::from(format!("preview-md-{:?}", tab)),
                            source.clone(),
                        )
                        .selectable(true)
                        .scrollable(true)
                        .scroll_smooth(smooth_ease, smooth_boost)
                        .h_full()
                        .p_0(),
                    )
                    .child(self.render_floating_preview_controls(
                        PreviewKind::Markdown,
                        Some(source),
                        cx,
                    )),
            )
            .into_any_element()
    }

    /// Flexible image stage; transparent pixels use the quiet theme surface.
    fn render_image_preview(&mut self, muted: Hsla, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.state.focused_tile;
        let source = match self.state.preview.results.get(&tab) {
            Some(PreviewResult::Image {
                original_width,
                original_height,
                ..
            }) => (*original_width, *original_height),
            Some(PreviewResult::Pdf { width, height, .. }) => (*width, *height),
            _ => return div().into_any_element(),
        };
        let Some(texture) = self.focused_preview_texture() else {
            return div()
                .flex_1()
                .text_color(muted)
                .child(rust_i18n::t!("inspector_loading").to_string())
                .into_any_element();
        };
        let state = self.preview_view_for_focused();
        let geometry = state
            .viewport
            .map(|viewport| crate::ui::preview_image::ImageGeometry::new(viewport, source, state));
        let percent = geometry.map_or(100., |g| g.scale * 100.);
        let fg = cx.theme().foreground;
        let well = layer_color(cx.theme().background, -0.015, 1.0);
        let fit = gpui_component::button::Button::new("preview-zoom-fit")
            .small()
            .compact()
            .ghost()
            .tooltip(rust_i18n::t!("inspector_preview_zoom_fit").to_string())
            .child(
                gpui_component::Icon::new(gpui_component::IconName::Frame)
                    .with_size(gpui_component::Size::Small)
                    .text_color(fg),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.process_actions(vec![Action::PreviewZoomFit]);
                cx.notify();
            }))
            .map(|button| {
                accessible_button(
                    "preview-zoom-fit",
                    rust_i18n::t!("inspector_preview_zoom_fit").to_string(),
                    button,
                )
            });
        let actual = gpui_component::button::Button::new("preview-zoom-actual")
            .small()
            .compact()
            .ghost()
            .label("1:1")
            .tooltip(rust_i18n::t!("inspector_actual_size").to_string())
            .on_click(cx.listener(move |this, _, _, cx| {
                let state = this
                    .preview_view
                    .entry(this.state.focused_tile)
                    .or_default();
                if let Some(viewport) = state.viewport {
                    let geometry =
                        crate::ui::preview_image::ImageGeometry::new(viewport, source, *state);
                    state.zoom = (1. / geometry.fit_scale).clamp(0.1, 48.);
                    state.pan_x = 0.;
                    state.pan_y = 0.;
                }
                cx.notify();
            }));
        let actual = accessible_button(
            "preview-zoom-actual",
            rust_i18n::t!("inspector_actual_size").to_string(),
            actual,
        );
        let zoomed = (state.zoom - 1.0).abs() > 0.001;
        let (surface, border) = (cx.theme().popover, cx.theme().border);
        // Floating glass pill (Quick Look): appears on hover, stays while zoomed.
        let pill = h_flex()
            .absolute()
            .bottom(px(12.))
            .left_0()
            .right_0()
            .justify_center()
            .child(
                h_flex()
                    .items_center()
                    .gap_0p5()
                    .px_1()
                    .py(px(2.))
                    .rounded(px(999.))
                    .bg(layer_color(surface, 0.0, 0.86))
                    .border_1()
                    .border_color(border.opacity(0.30))
                    .raised_surface()
                    .occlude()
                    .child(
                        preview_button(
                            "preview-zoom-out",
                            AtlasIcon::Minus,
                            rust_i18n::t!("inspector_preview_zoom_out").to_string(),
                            fg,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.process_actions(vec![Action::PreviewZoomOut]);
                            cx.notify();
                        })),
                    )
                    .child(
                        div()
                            .min_w(crate::ui::rem_px(40.))
                            .text_center()
                            .text_xs()
                            .text_color(muted)
                            .child(format!("{percent:.0}%")),
                    )
                    .child(
                        preview_button(
                            "preview-zoom-in",
                            AtlasIcon::Plus,
                            rust_i18n::t!("inspector_preview_zoom_in").to_string(),
                            fg,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.process_actions(vec![Action::PreviewZoomIn]);
                            cx.notify();
                        })),
                    )
                    .child(div().w(px(1.)).h(px(14.)).mx_0p5().bg(border.opacity(0.5)))
                    .child(fit)
                    .child(actual),
            )
            .opacity(if zoomed { 1. } else { 0. })
            .group_hover("preview-image-stage", |s| s.opacity(1.));
        div()
            .id("preview-image")
            .group("preview-image-stage")
            .relative()
            .size_full()
            .min_h(px(0.))
            .min_w(px(0.))
            .overflow_hidden()
            .rounded(px(10.))
            .bg(well)
            .child(crate::ui::preview_image::image_canvas(
                cx.entity(),
                tab,
                texture,
                source,
                state,
                cx,
            ))
            .child(pill)
            .into_any_element()
    }

    fn render_table_preview(&mut self, muted: Hsla, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.state.focused_tile;
        let Some(PreviewResult::Table {
            headers,
            rows,
            truncated,
        }) = self.state.preview.results.get(&tab)
        else {
            return div().into_any_element();
        };
        let is_glass = self.state.profiles.active().theme.ends_with(" Glass");
        let (border, stripe, header_bg, fg, surface) = {
            let t = cx.theme();
            (
                t.border,
                if is_glass {
                    t.list_even
                } else {
                    backdrop_tint(t.list_even, 0.08)
                },
                if is_glass {
                    t.list_head
                } else {
                    layer_color(t.secondary, 0.04, 0.88)
                },
                t.foreground,
                t.secondary,
            )
        };
        let cols = headers
            .len()
            .max(rows.iter().map(Vec::len).max().unwrap_or(0))
            .min(50);
        let shown = rows.len().min(200);
        let widths = table_column_widths(headers, &rows[..shown], cols);
        let index_w = 24. + 7. * (shown.max(1).to_string().len() as f32);
        let total_w = index_w + widths.iter().sum::<f32>();
        let cell = |width: f32, text: String, color: Hsla| {
            div()
                .w(px(width))
                .flex_shrink_0()
                .px_2()
                .truncate()
                .text_color(color)
                .child(text)
        };
        let mut table = v_flex().w(px(total_w)).text_xs();
        if !headers.is_empty() {
            let mut row = h_flex()
                .h(px(28.))
                .bg(header_bg)
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(cell(index_w, String::new(), muted));
            for (col, width) in widths.iter().enumerate() {
                row = row.child(cell(*width, headers.get(col).cloned().unwrap_or_default(), fg));
            }
            table = table.child(row);
        }
        for (i, cells) in rows.iter().take(shown).enumerate() {
            let mut row = h_flex()
                .h(px(24.))
                .border_t_1()
                .border_color(border.opacity(0.22))
                .when(i % 2 == 1, |s| s.bg(stripe))
                .child(cell(index_w, (i + 1).to_string(), muted));
            for (col, width) in widths.iter().enumerate() {
                row = row.child(cell(*width, cells.get(col).cloned().unwrap_or_default(), fg));
            }
            table = table.child(row);
        }
        let shape = rust_i18n::t!(
            "inspector_table_shape",
            rows = if *truncated {
                format!("{shown}+")
            } else {
                shown.to_string()
            },
            cols = cols
        )
        .to_string();
        v_flex()
            .size_full()
            .min_h(px(0.))
            .gap_2()
            .child(
                div()
                    .id("preview-table")
                    .flex_1()
                    .min_h(px(0.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(border.opacity(0.22))
                    .bg(layer_color(surface, -0.02, 0.30))
                    .overflow_hidden()
                    .child(
                        div()
                            .id("preview-table-scroll")
                            .size_full()
                            .overflow_scroll()
                            .child(table),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(muted)
                    .child(if *truncated {
                        format!("{shape} · {}", rust_i18n::t!("preview_truncated"))
                    } else {
                        shape
                    }),
            )
            .into_any_element()
    }

    fn render_document_preview(&mut self, muted: Hsla) -> AnyElement {
        let tab = self.state.focused_tile;
        let Some(PreviewResult::Document {
            title,
            blocks,
            truncated,
        }) = self.state.preview.results.get(&tab)
        else {
            return div().into_any_element();
        };
        let mut page = v_flex()
            .gap_3()
            .p_4()
            .w_full()
            .max_w(px(560.))
            .min_w(px(0.))
            .min_h(px(720.))
            .bg(gpui::white())
            .text_color(gpui::black())
            .rounded(px(3.))
            .border_1()
            .border_color(muted.opacity(0.25))
            .raised_surface()
            .text_sm();
        if !title.is_empty() {
            page = page.child(
                div()
                    .w_full()
                    .min_w(px(0.))
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title.clone()),
            );
        }
        for (i, block) in blocks.iter().take(200).enumerate() {
            let is_heading =
                i == 0 && title.is_empty() || block.len() < 80 && !block.ends_with('.');
            page = page.child(
                div()
                    .w_full()
                    .min_w(px(0.))
                    .when(is_heading, |s| {
                        s.font_weight(gpui::FontWeight::SEMIBOLD).text_lg()
                    })
                    .child(block.clone()),
            );
        }
        if *truncated {
            page = page.child(div().text_xs().text_color(muted).child("Preview truncado"));
        }
        div()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scrollbar()
            .child(v_flex().w_full().items_center().p_3().child(page))
            .into_any_element()
    }

    fn render_presentation_preview(&mut self, muted: Hsla) -> AnyElement {
        let tab = self.state.focused_tile;
        let Some(PreviewResult::Presentation { slides, truncated }) =
            self.state.preview.results.get(&tab)
        else {
            return div().into_any_element();
        };
        let mut col = v_flex()
            .flex_1()
            .min_h(px(0.))
            .gap_3()
            .overflow_y_scrollbar()
            .items_center()
            .p_2()
            .text_sm();
        for (i, slide) in slides.iter().enumerate() {
            let title = if slide.title.is_empty() {
                format!("Slide {}", i + 1)
            } else {
                slide.title.clone()
            };
            let mut card = v_flex()
                .gap_2()
                .w_full()
                .max_w(px(520.))
                .min_w(px(0.))
                .min_h(px(220.))
                .p_4()
                .bg(gpui::white())
                .text_color(gpui::black())
                .rounded(px(4.))
                .border_1()
                .border_color(muted.opacity(0.25))
                .raised_surface();
            card = card.child(
                div()
                    .w_full()
                    .min_w(px(0.))
                    .text_color(muted)
                    .text_xs()
                    .child(format!("Slide {}", i + 1)),
            );
            card = card.child(
                div()
                    .w_full()
                    .min_w(px(0.))
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(title),
            );
            for line in &slide.body {
                card = card.child(
                    h_flex()
                        .w_full()
                        .min_w(px(0.))
                        .gap_2()
                        .child("•")
                        .child(div().min_w(px(0.)).child(line.clone())),
                );
            }
            col = col.child(card);
        }
        if *truncated {
            col = col.child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(rust_i18n::t!("preview_truncated")),
            );
        }
        col.into_any_element()
    }

    /// Audio: cover art (or a placeholder) plus title/artist/album/duration/bitrate.
    fn render_audio_preview(&mut self, muted: Hsla) -> AnyElement {
        let tab = self.state.focused_tile;
        let (title, artist, album, duration_secs, bitrate, has_cover) =
            match self.state.preview.results.get(&tab) {
                Some(PreviewResult::Audio {
                    title,
                    artist,
                    album,
                    duration_secs,
                    bitrate,
                    cover,
                }) => (
                    title.clone(),
                    artist.clone(),
                    album.clone(),
                    *duration_secs,
                    *bitrate,
                    cover.is_some(),
                ),
                _ => return div().into_any_element(),
            };
        let texture = if has_cover {
            self.focused_preview_texture()
        } else {
            None
        };

        let mut col = v_flex().flex_1().min_h(px(0.)).gap_2();
        if let Some(tex) = texture {
            col = col.child(
                img(tex)
                    .max_w(px(220.))
                    .max_h(px(220.))
                    .rounded_md()
                    .raised_surface(),
            );
        } else {
            col = col.child(
                h_flex()
                    .w(px(220.))
                    .h(px(220.))
                    .rounded_md()
                    .bg(muted)
                    .opacity(0.15)
                    .raised_surface()
                    .items_center()
                    .justify_center()
                    .child(icon(AtlasIcon::Music, 44.0, muted)),
            );
        }

        let mut meta = v_flex().gap_1();
        meta = meta.child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.unwrap_or_else(|| rust_i18n::t!("untitled").into())),
        );
        if let Some(artist) = artist {
            meta = meta.child(div().text_sm().text_color(muted).child(artist));
        }
        if let Some(album) = album {
            meta = meta.child(div().text_sm().text_color(muted).child(album));
        }
        let (mins, secs) = (duration_secs / 60, duration_secs % 60);
        let line = match bitrate {
            Some(br) => format!("{mins}:{secs:02} · {br} kbps"),
            None => format!("{mins}:{secs:02}"),
        };
        meta = meta.child(div().text_xs().text_color(muted).child(line));
        col.child(meta).into_any_element()
    }

    /// Ask for the thumbnails of the folder's image/video files (the ring cards).
    fn request_dir_ring_thumbnails(&mut self, paths: &[PathBuf]) {
        if let Ok(mut wanted) = self.thumb_wanted.lock() {
            wanted.extend(paths.iter().cloned());
        }
        for path in paths {
            if self.thumb_textures.contains_key(path) {
                continue;
            }
            let is_image = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| atlas_model::preview_cache::is_image_ext(&e.to_ascii_lowercase()));
            #[cfg(target_os = "windows")]
            if !is_image {
                self.request_shell_thumbnail(path);
                continue;
            }
            let _ = is_image;
            self.request_thumbnail(path);
        }
    }

    fn render_dir_preview(&mut self, muted: Hsla, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.state.focused_tile;
        let view = cx.entity();
        let in_archive = self
            .shown_preview_path(tab)
            .as_deref()
            .is_some_and(atlas_core::fs::archive::is_archive_path);
        let media: Vec<PathBuf> = match self.state.preview.results.get(&tab) {
            Some(PreviewResult::Dir { entries, .. }) if !in_archive => {
                dir_media_paths(entries).into_iter().cloned().collect()
            }
            _ => Vec::new(),
        };
        self.request_dir_ring_thumbnails(&media);
        let ring_cards: Vec<Arc<RenderImage>> = media
            .iter()
            .filter_map(|path| self.grid_thumbnail(path))
            .collect();
        let Some(PreviewResult::Dir {
            entries,
            total_count,
            total_size,
        }) = self.state.preview.results.get(&tab)
        else {
            return div().into_any_element();
        };
        let container = self.shown_preview_path(tab);
        let archive = container
            .as_deref()
            .is_some_and(atlas_core::fs::archive::is_archive_path);
        let (dirs, files) = entries.iter().fold((0, 0), |(d, f), e| {
            if e.is_dir { (d + 1, f) } else { (d, f + 1) }
        });
        let (total_count, total_size) = (*total_count, *total_size);
        let mut hero = Vec::new();
        if let Some(container) = container.as_ref() {
            let kind = if archive { FileKind::File } else { FileKind::Dir };
            let extension = archive.then(|| {
                container
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default()
            });
            hero.push(HeroItem::Icon {
                path: container.clone(),
                kind,
                extension,
            });
        }
        for e in entries.iter().filter(|e| !e.is_dir).take(2) {
            hero.push(HeroItem::Icon {
                path: if archive { PathBuf::new() } else { e.path.clone() },
                kind: FileKind::File,
                extension: e.extension.clone(),
            });
        }
        // Only real thumbnails are shown; with none there is no hero at all.
        let hero = if ring_cards.is_empty() {
            None
        } else {
            let key = container.clone().unwrap_or_default();
            let ring = crate::ui::dir_ring::ensure(&mut self.dir_ring, cx);
            ring.update(cx, |ring, _| ring.set_items(&key, ring_cards));
            Some(ring.into_any_element())
        };
        let stats = self.stat_row(
            [
                (AtlasIcon::Folder, dirs, "inspector_kind_folders"),
                (AtlasIcon::File, files, "inspector_kind_files"),
                (AtlasIcon::Grid, total_count, "inspector_kind_items"),
            ],
            Some(total_size),
            muted,
            cx,
        );
        let mut list = v_flex().gap_1().text_xs();
        for e in entries {
            let row_kind = if e.is_dir {
                FileKind::Dir
            } else {
                FileKind::File
            };
            let path = e.path.clone();
            let is_dir = e.is_dir;
            let size = e.size.map(format_size).unwrap_or_default();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!(
                        "inspector-dir-row-{}",
                        path.display()
                    )))
                    .gap_2()
                    .items_center()
                    .rounded(px(6.))
                    .px_1()
                    .py(px(2.))
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.82))
                    .child(
                        div().w(px(16.)).child(
                            match self.icon_tex_for_entry(&row_kind, e.extension.as_deref()) {
                                Some(tex) => gpui::img(tex)
                                    .w(px(14.))
                                    .h(px(14.))
                                    .flex_none()
                                    .into_any_element(),
                                None => file_type_icon(&row_kind, e.extension.as_deref(), 14.0),
                            },
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .child(e.name.clone()),
                    )
                    .child(div().text_color(muted).child(size))
                    .on_click({
                        let view = view.clone();
                        let path = path.clone();
                        move |ev, _window, cx| {
                            if ev.click_count() < 2 || archive {
                                return;
                            }
                            view.update(cx, |this, cx| {
                                let tile = this.state.focused_tile;
                                if is_dir {
                                    this.navigate_entry(tile, path.clone());
                                } else {
                                    this.process_actions(vec![Action::Open(vec![path.clone()])]);
                                }
                                cx.notify();
                            });
                        }
                    })
                    .when(!archive, |row| row.on_file_drag_anchor(view.clone(), path.clone()))
                    .when(is_dir && !archive, {
                        let nav_path = path.clone();
                        let dest = match self.folder_drop_command(tab, &path) {
                            crate::ui::drag_hover::DragHoverDropCommand::TransferTo(dest) => dest,
                            _ => path.clone(),
                        };
                        let dest_move = dest.clone();
                        let dest_copy = dest;
                        let mv = view.clone();
                        let cpv = view.clone();
                        move |row| {
                            use crate::ui::drag_hover::{
                                drag_hover_recorder_with_drop, DragHoverCommand,
                                DragHoverDropCommand, DragHoverId, DragHoverKind,
                            };
                            row.on_prepaint_child(drag_hover_recorder_with_drop(
                                DragHoverId::Inspector(dest_move.clone()),
                                DragHoverKind::Folder,
                                DragHoverCommand::NavigatePathIn(tab, nav_path),
                                DragHoverDropCommand::TransferTo(dest_move.clone()),
                                &mv,
                            ))
                            .on_drop::<DraggedFiles>(move |d, _window, cx| {
                                let (paths, dest) = (d.paths.clone(), dest_move.clone());
                                mv.update(cx, |this, cx| {
                                    this.execute_drag_hover_drop_command(
                                        DragHoverDropCommand::TransferTo(dest),
                                        paths,
                                        true,
                                    );
                                    cx.notify();
                                });
                            })
                            .on_drop::<gpui::ExternalPaths>(move |paths, _window, cx| {
                                let (paths, dest) = (paths.paths().to_vec(), dest_copy.clone());
                                cpv.update(cx, |this, cx| {
                                    this.execute_drag_hover_drop_command(
                                        DragHoverDropCommand::TransferTo(dest),
                                        paths,
                                        false,
                                    );
                                    cx.notify();
                                });
                            })
                        }
                    }),
            );
        }
        if entries.len() < total_count && entries.len() >= DIR_PREVIEW_LIMIT {
            list = list.child(
                div().text_color(muted).child(
                    rust_i18n::t!(
                        "inspector_and_more",
                        count = total_count - DIR_PREVIEW_LIMIT
                    )
                    .to_string(),
                ),
            );
        }
        let border = cx.theme().border;
        v_flex()
            .size_full()
            .min_h(px(0.))
            .gap_3()
            .child(
                v_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_3()
                    .pt_3()
                    .children(hero)
                    .child(stats),
            )
            .child(
                div()
                    .id("inspector-dir")
                    .flex_1()
                    .min_h(px(0.))
                    .pt_2()
                    .border_t_1()
                    .border_color(border.opacity(0.25))
                    .overflow_y_scroll()
                    .child(list),
            )
            .into_any_element()
    }

    fn render_multi(
        &mut self,
        summary: &SelectionSummary,
        muted: Hsla,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let textures = self.focused_multi_textures();
        let (fg, border) = (cx.theme().foreground, cx.theme().border);
        // Real thumbnails only, orbiting in the ring; no ring when nothing decoded.
        // PDFs rasterise to a mostly blank white page: not worth a ring card.
        let cards: Vec<Arc<RenderImage>> = textures
            .iter()
            .filter(|(path, _)| {
                !path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
            })
            .take(RING_LIMIT)
            .map(|(_, texture)| texture.clone())
            .collect();
        let hero = if cards.is_empty() {
            None
        } else {
            let key = PathBuf::from(format!(
                "multi:{}:{}",
                summary.count,
                textures.first().map(|(p, _)| p.display().to_string()).unwrap_or_default()
            ));
            let ring = crate::ui::dir_ring::ensure(&mut self.dir_ring, cx);
            ring.update(cx, |ring, _| ring.set_items(&key, cards));
            Some(ring.into_any_element())
        };
        let others = summary
            .count
            .saturating_sub(summary.dir_count + summary.image_count);
        let stats = self.stat_row(
            [
                (AtlasIcon::Folder, summary.dir_count, "inspector_kind_folders"),
                (AtlasIcon::Image, summary.image_count, "inspector_kind_images"),
                (AtlasIcon::File, others, "inspector_kind_files"),
            ],
            None,
            muted,
            cx,
        );
        let rows = summary.items.iter().map(|item| {
            h_flex()
                .h(crate::ui::rem_px(26.))
                .flex_shrink_0()
                .gap_2()
                .px_2()
                .rounded(px(6.))
                .min_w(px(0.))
                .child(
                    self.file_icon_for_entry(&item.path, &item.kind, item.extension.as_deref())
                        .element(16., muted),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .truncate()
                        .text_xs()
                        .text_color(fg)
                        .child(item.name.clone()),
                )
                .when_some(item.size, |row, size| {
                    row.child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(muted)
                            .child(format_size(size)),
                    )
                })
        });
        let hidden = summary.count.saturating_sub(summary.items.len());
        v_flex()
            .size_full()
            .min_h(px(0.))
            .gap_3()
            .child(
                v_flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_3()
                    .pt_3()
                    .children(hero)
                    .child(stats),
            )
            .child(
                div()
                    .id("preview-multi-list")
                    .flex_1()
                    .min_h(px(0.))
                    .pt_2()
                    .border_t_1()
                    .border_color(border.opacity(0.25))
                    .overflow_y_scroll()
                    .child(v_flex().children(rows).when(hidden > 0, |list| {
                        list.child(
                            div().px_2().pt_1().text_xs().text_color(muted).child(
                                rust_i18n::t!("inspector_more_items", count = hidden).to_string(),
                            ),
                        )
                    })),
            )
            .into_any_element()
    }

    /// Quick Look-style stack: `front` large in the middle, its neighbours
    /// peeking out behind on either side. `dx` slides the front card while the
    /// user swipes.
    #[allow(dead_code)]
    fn stack_hero(
        &self,
        items: &[HeroItem],
        front: usize,
        dx: f32,
        interactive: bool,
        muted: Hsla,
        cx: &Context<Self>,
    ) -> AnyElement {
        // Opaque cards: translucent theme surfaces would let the stack bleed through.
        let card_bg = layer_color(cx.theme().background, 0.05, 1.0);
        let border = cx.theme().border;
        let card = |item: &HeroItem, edge: f32| -> gpui::Div {
            let content = match item {
                HeroItem::Texture(texture) => img(texture.clone())
                    .size_full()
                    .rounded(px(10.))
                    .object_fit(gpui::ObjectFit::Cover)
                    .into_any_element(),
                HeroItem::Icon {
                    path,
                    kind,
                    extension,
                } => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        self.file_icon_for_entry(path, kind, extension.as_deref())
                            .element(edge * 0.56, muted),
                    )
                    .into_any_element(),
            };
            div()
                .size(crate::ui::rem_px(edge))
                .p(px(4.))
                .rounded(px(14.))
                .bg(card_bg)
                .border_1()
                .border_color(border.opacity(0.45))
                .raised_surface()
                .child(content)
        };
        let mut stack = div()
            .relative()
            .flex_shrink_0()
            .w(crate::ui::rem_px(236.))
            .h(crate::ui::rem_px(150.));
        let n = items.len();
        if n == 0 {
            return stack.into_any_element();
        }
        let front = front % n;
        // Previous card peeks left, next card peeks right.
        let mut backs: Vec<(usize, f32, &'static str, isize)> = Vec::new();
        if n > 1 {
            backs.push(((front + n - 1) % n, 4., "preview-stack-prev", -1));
        }
        if n > 2 || (n == 2 && front == 0) {
            backs.push(((front + 1) % n, 148., "preview-stack-next", 1));
        }
        if n == 2 && front == 1 {
            backs.retain(|b| b.3 < 0);
        }
        for (index, left, id, step) in backs {
            let app = cx.entity();
            let back = div()
                .id(id)
                .accessibility_id(id)
                .role(gpui::Role::Button)
                .absolute()
                .left(crate::ui::rem_px(left))
                .top(crate::ui::rem_px(4.))
                .opacity(0.82)
                .child(card(&items[index], 84.));
            if !interactive {
                stack = stack.child(back);
                continue;
            }
            stack = stack.child(
                back.cursor_pointer()
                    .hover(|s| s.opacity(1.))
                    .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                        app.update(cx, |this, cx| {
                            let tab = this.state.focused_tile;
                            let view = this.preview_view.entry(tab).or_default();
                            if view.stack_dx.abs() >= 4. {
                                // A swipe that ends over a peeking card is still a swipe.
                                finish_stack_swipe(this, n, cx);
                                return;
                            }
                            view.stack_index = step_index(view.stack_index, step, n);
                            view.stack_drag = None;
                            view.stack_dx = 0.;
                            cx.notify();
                        });
                        cx.stop_propagation();
                    }),
            );
        }
        let lift = (dx.abs() / 160.).min(1.);
        stack = stack.child(
            div()
                .absolute()
                .left(crate::ui::rem_px(56.))
                .top(crate::ui::rem_px(24.))
                .ml(px(dx))
                .opacity(1. - lift * 0.35)
                .child(card(&items[front], 124.)),
        );
        stack.into_any_element()
    }

    /// Interactive multi-selection stack: swipe the front photo, scroll, or
    /// click a peeking card to bring it forward; the caption names the card.
    #[allow(dead_code)]
    fn stack_carousel(
        &self,
        hero: Vec<(HeroItem, String)>,
        muted: Hsla,
        cx: &Context<Self>,
    ) -> AnyElement {
        let n = hero.len();
        let view = self.preview_view_for_focused();
        let front = if n == 0 { 0 } else { view.stack_index % n };
        let caption = hero.get(front).map(|(_, name)| name.clone()).unwrap_or_default();
        let (items, _): (Vec<HeroItem>, Vec<String>) = hero.into_iter().unzip();
        let stack = self.stack_hero(&items, front, view.stack_dx, true, muted, cx);
        let app = cx.entity();
        let (down, moved, up, out, wheel) =
            (app.clone(), app.clone(), app.clone(), app.clone(), app);
        let fg = cx.theme().foreground;
        v_flex()
            .items_center()
            .gap_1p5()
            .child(
                div()
                    .id("preview-stack")
                    .accessibility_id("preview-stack")
                    .role(gpui::Role::Group)
                    .cursor(if view.stack_drag.is_some() {
                        gpui::CursorStyle::ClosedHand
                    } else {
                        gpui::CursorStyle::OpenHand
                    })
                    .child(stack)
                    .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                        down.update(cx, |this, cx| {
                            let tab = this.state.focused_tile;
                            let view = this.preview_view.entry(tab).or_default();
                            view.stack_drag = Some(f32::from(event.position.x));
                            view.stack_dx = 0.;
                            cx.notify();
                        });
                    })
                    .on_mouse_move(move |event, _, cx| {
                        moved.update(cx, |this, cx| {
                            let tab = this.state.focused_tile;
                            let view = this.preview_view.entry(tab).or_default();
                            let Some(start) = view.stack_drag else { return };
                            if event.pressed_button != Some(MouseButton::Left) {
                                view.stack_drag = None;
                                view.stack_dx = 0.;
                            } else {
                                view.stack_dx = (f32::from(event.position.x) - start).clamp(-120., 120.);
                            }
                            cx.notify();
                        });
                    })
                    .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                        up.update(cx, |this, cx| finish_stack_swipe(this, n, cx));
                    })
                    .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                        out.update(cx, |this, cx| finish_stack_swipe(this, n, cx));
                    })
                    .on_scroll_wheel(move |event, window, cx| {
                        let delta = event.delta.pixel_delta(window.line_height());
                        let amount = if f32::from(delta.x).abs() > f32::from(delta.y).abs() {
                            f32::from(delta.x)
                        } else {
                            f32::from(delta.y)
                        };
                        wheel.update(cx, |this, cx| {
                            let tab = this.state.focused_tile;
                            let view = this.preview_view.entry(tab).or_default();
                            view.stack_wheel += amount;
                            if view.stack_wheel.abs() >= 36. {
                                let step = if view.stack_wheel > 0. { -1 } else { 1 };
                                view.stack_index = step_index(view.stack_index, step, n);
                                view.stack_wheel = 0.;
                                cx.notify();
                            }
                        });
                        cx.stop_propagation();
                    }),
            )
            .when(n > 0, |col| {
                col.child(
                    h_flex()
                        .max_w(crate::ui::rem_px(236.))
                        .gap_1p5()
                        .items_center()
                        .text_xs()
                        .child(div().min_w(px(0.)).truncate().text_color(fg).child(caption))
                        .when(n > 1, |row| {
                            row.child(
                                div()
                                    .flex_shrink_0()
                                    .px_1p5()
                                    .rounded(px(999.))
                                    .bg(muted.opacity(0.16))
                                    .text_color(muted)
                                    .child(format!("{} / {n}", front + 1)),
                            )
                        }),
                )
            })
            .into_any_element()
    }

    /// Icon + number chips (labels live in tooltips) and an optional size chip.
    fn stat_row<const N: usize>(
        &self,
        counts: [(AtlasIcon, usize, &'static str); N],
        size: Option<u64>,
        muted: Hsla,
        cx: &Context<Self>,
    ) -> AnyElement {
        let (fg, surface) = (cx.theme().foreground, cx.theme().secondary);
        let chip = |id: SharedString, glyph: AtlasIcon, text: String, tip: String| {
            h_flex()
                .id(id)
                .gap_1()
                .items_center()
                .px_2()
                .py(px(2.))
                .rounded(px(999.))
                .bg(layer_color(surface, 0.02, 0.70))
                .text_xs()
                .text_color(fg)
                .child(icon(glyph, 12., muted))
                .child(text)
                .tooltip(move |window, cx| {
                    gpui_component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                })
        };
        let mut row = h_flex().flex_wrap().justify_center().gap_1();
        for (glyph, count, key) in counts {
            if count > 0 {
                row = row.child(chip(
                    SharedString::from(format!("preview-stat-{key}")),
                    glyph,
                    count.to_string(),
                    format!("{count} {}", rust_i18n::t!(key)),
                ));
            }
        }
        if let Some(size) = size {
            row = row.child(chip(
                "preview-stat-size".into(),
                AtlasIcon::HardDrive,
                format_size(size),
                rust_i18n::t!("inspector_total_size").to_string(),
            ));
        }
        row.into_any_element()
    }

    /// Summary for the browsed folder when nothing is selected.
    fn folder_summary(&self) -> Option<SelectionSummary> {
        let path = self.state.active_tab_data()?.location.fs_dir()?.to_path_buf();
        let name = path
            .file_name()
            .map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        Some(SelectionSummary {
            count: 1,
            dir_count: 1,
            single: Some(SelectedEntry {
                name,
                path,
                kind: FileKind::Dir,
                size: None,
                modified: None,
                extension: None,
            }),
            ..SelectionSummary::default()
        })
    }
}

/// Cards the folder ring can orbit.
/// Kept small on purpose: a crowded ring hides the picture in front.
const RING_LIMIT: usize = 7;

/// Files of a folder preview that have a real picture to show (images, videos).
fn dir_media_paths(entries: &[atlas_model::preview_cache::DirPreviewEntry]) -> Vec<&PathBuf> {
    entries
        .iter()
        .filter(|e| {
            !e.is_dir
                && e.extension.as_deref().is_some_and(|ext| {
                    let ext = ext.to_ascii_lowercase();
                    atlas_model::preview_cache::is_image_ext(&ext)
                        || atlas_model::preview_cache::is_video_ext(&ext)
                })
        })
        .map(|e| &e.path)
        .take(RING_LIMIT)
        .collect()
}

/// Cards the multi-selection carousel can cycle through.
const STACK_LIMIT: usize = 48;

fn step_index(index: usize, step: isize, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    (index as isize + step).rem_euclid(n as isize) as usize
}

/// Release of a swipe: past the threshold the photo slides to its neighbour.
fn finish_stack_swipe(app: &mut App, n: usize, cx: &mut Context<App>) {
    let tab = app.state.focused_tile;
    let view = app.preview_view.entry(tab).or_default();
    if view.stack_drag.take().is_none() {
        return;
    }
    if view.stack_dx <= -48. {
        view.stack_index = step_index(view.stack_index, 1, n);
    } else if view.stack_dx >= 48. {
        view.stack_index = step_index(view.stack_index, -1, n);
    }
    view.stack_dx = 0.;
    cx.notify();
}

enum HeroItem {
    Texture(Arc<RenderImage>),
    Icon {
        path: PathBuf,
        kind: FileKind,
        extension: Option<String>,
    },
}

pub(crate) fn tool_label(name: &str) -> String {
    match name {
        "magick" => "ImageMagick".into(),
        "pdftoppm" => "PDF renderer".into(),
        "ffmpeg" => "FFmpeg".into(),
        "gs" => "Ghostscript".into(),
        other => other.to_string(),
    }
}

fn accessible_button(
    id: impl Into<SharedString>,
    label: String,
    button: gpui_component::button::Button,
) -> gpui::Stateful<gpui::Div> {
    let id = id.into();
    div()
        .id(id.clone())
        .accessibility_id(id)
        .role(gpui::Role::Button)
        .aria_label(label)
        .flex_shrink_0()
        .child(button)
}

#[derive(IntoElement)]
struct PreviewButton {
    id: &'static str,
    label: String,
    button: gpui_component::button::Button,
}

impl PreviewButton {
    fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> Self {
        self.button = self.button.on_click(handler);
        self
    }
}

impl RenderOnce for PreviewButton {
    fn render(self, _: &mut Window, _: &mut gpui::App) -> impl IntoElement {
        accessible_button(self.id, self.label, self.button)
    }
}

fn preview_button(id: &'static str, glyph: AtlasIcon, label: String, color: Hsla) -> PreviewButton {
    PreviewButton {
        id,
        label: label.clone(),
        button: gpui_component::button::Button::new(id)
            .small()
            .compact()
            .ghost()
            .tooltip(label)
            .child(icon(glyph, 14., color)),
    }
}

/// Content-sized column widths (approximate glyph width), clamped so one long
/// cell cannot push every other column out of view.
fn table_column_widths(headers: &[String], rows: &[Vec<String>], cols: usize) -> Vec<f32> {
    (0..cols)
        .map(|col| {
            let longest = std::iter::once(headers.get(col))
                .chain(rows.iter().take(60).map(|row| row.get(col)))
                .flatten()
                .map(|text| text.chars().count())
                .max()
                .unwrap_or(0);
            (longest as f32 * 6.6 + 18.).clamp(48., 240.)
        })
        .collect()
}

fn preview_icon_button(
    id: &'static str,
    glyph: gpui_component::IconName,
    label: String,
    color: Hsla,
) -> PreviewButton {
    PreviewButton {
        id,
        label: label.clone(),
        button: gpui_component::button::Button::new(id)
            .small()
            .compact()
            .ghost()
            .tooltip(label)
            .child(
                gpui_component::Icon::new(glyph)
                    .with_size(gpui_component::Size::Small)
                    .text_color(color),
            ),
    }
}

fn preview_summary(summary: &SelectionSummary, result: Option<&PreviewResult>) -> String {
    let mut parts = Vec::new();
    if let Some(PreviewResult::Image {
        original_width,
        original_height,
        ..
    }) = result
    {
        parts.push(format!("{original_width} × {original_height} px"));
    }
    if summary.count > 1 {
        if summary.file_count > 0 {
            parts.push(
                rust_i18n::t!("inspector_files_count", count = summary.file_count).to_string(),
            );
        }
        if summary.dir_count > 0 {
            parts.push(
                rust_i18n::t!("inspector_folders_count", count = summary.dir_count).to_string(),
            );
        }
    }
    if summary.known_size_count > 0 {
        parts.push(format_size(summary.total_size));
    }
    parts.join(" · ")
}

enum PreviewKind {
    Loading,
    Text,
    Markdown,
    Image,
    Pdf,
    Table,
    Document,
    Presentation,
    Audio,
    Dir,
    Binary,
    Error,
}

fn wrap_preview_error(msg: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in msg.split_whitespace() {
        if !line.is_empty() && line.len() + word.len() + 1 > 52 {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push("Preview unavailable".to_string());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::wrap_preview_error;

    #[test]
    fn preview_errors_wrap_to_short_lines() {
        let lines =
            wrap_preview_error("PSD preview unavailable. Install a PSD decoder preview extension.");
        assert!(lines.len() > 1);
        assert!(lines.iter().all(|line| line.len() <= 52));
    }
}

fn floating_icon_button(
    id: &'static str,
    icon_name: gpui_component::IconName,
    tooltip: String,
    disabled: bool,
    active_color: Hsla,
    muted: Hsla,
) -> gpui::Stateful<gpui::Div> {
    let color = if disabled { muted } else { active_color };
    let el = div()
        .id(gpui::SharedString::from(id))
        .flex()
        .items_center()
        .justify_center()
        .w(px(24.))
        .h(px(24.))
        .rounded(px(6.))
        .text_color(color)
        .cursor_pointer();
    if !disabled {
        el.hover(|s| s.opacity(0.75))
    } else {
        el
    }
    .child(
        gpui_component::Icon::new(icon_name)
            .with_size(gpui_component::Size::XSmall)
            .text_color(color),
    )
    .tooltip(move |window, cx| {
        gpui_component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
    })
}

// ── Selection summary (neutral; mirrors the egui inspector's SelectionSummary) ──

#[derive(Default, Clone, Debug)]
pub struct SelectionSummary {
    pub count: usize,
    pub file_count: usize,
    pub dir_count: usize,
    pub image_count: usize,
    pub total_size: u64,
    pub known_size_count: usize,
    pub single: Option<SelectedEntry>,
    /// First `MULTI_LIST_LIMIT` selected items, for the multi-selection list.
    pub items: Vec<SelectedItem>,
}

const MULTI_LIST_LIMIT: usize = 200;

#[derive(Clone, Debug)]
pub struct SelectedItem {
    pub name: String,
    pub path: PathBuf,
    pub kind: FileKind,
    pub extension: Option<String>,
    pub size: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct SelectedEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: FileKind,
    pub size: Option<u64>,
    pub modified: Option<std::time::SystemTime>,
    pub extension: Option<String>,
}

fn selection_summary(tab_state: &TabState, view_mode: &ViewMode) -> SelectionSummary {
    let entries = tab_state.selected_entries(view_mode);

    let mut s = SelectionSummary::default();
    for entry in &entries {
        s.count += 1;
        match entry.kind() {
            FileKind::Dir => s.dir_count += 1,
            FileKind::File | FileKind::Symlink => s.file_count += 1,
        }
        if let Some(size) = entry.size() {
            s.total_size += size;
            s.known_size_count += 1;
        }
        if s.items.len() < MULTI_LIST_LIMIT {
            s.items.push(SelectedItem {
                name: entry.name.to_string(),
                path: entry.path(),
                kind: entry.kind().clone(),
                extension: entry.extension().as_ref().map(ToString::to_string),
                size: entry.size(),
            });
        }
        if entry
            .extension()
            .as_deref()
            .is_some_and(atlas_model::preview_cache::is_image_ext)
        {
            s.image_count += 1;
        }
    }
    if let [entry] = entries.as_slice() {
        s.single = Some(SelectedEntry {
            name: entry.name.to_string(),
            path: entry.path(),
            kind: entry.kind().clone(),
            size: entry.size(),
            modified: entry.modified,
            extension: entry.extension().as_ref().map(ToString::to_string),
        });
    }
    s
}
