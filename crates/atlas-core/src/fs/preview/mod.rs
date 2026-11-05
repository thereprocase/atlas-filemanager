#![allow(dead_code)]
// reason: retained API surface not exercised by every GPUI flow yet
// reason: preview generation + temp-file cleanup run on background workers, never
// the gpui render thread; the sync-fs ban targets UI-foreground blocking.
#![allow(clippy::disallowed_methods)]

mod archive;
mod audio;
mod dir;
pub(crate) mod document;
mod image;
mod linux_thumb;
mod markdown;
mod model3d;
pub mod metadata;
mod pdf;
pub mod syntax;
mod text;
mod video;

pub use archive::load_archive_preview;
pub use audio::extract_audio_thumbnail;
pub use image::decode_svg;
pub use linux_thumb::freedesktop_thumbnail;
pub use model3d::model_thumbnail;
pub use pdf::{preview_pdf_public, preview_pdf_with_pdftoppm};

use std::path::PathBuf;

/// Metadata-only guard for automatic reads. A failed attribute query is treated
/// conservatively: automatic previews must never hydrate an offline cloud file.
/// Metadata rows (resolution, duration, codec) for a video, via ffprobe.
pub fn video_exif(path: &std::path::Path) -> Vec<(String, String)> {
    video::video_exif(path)
}

pub fn is_cloud_placeholder(path: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        atlas_win_space::directory::is_cloud_placeholder(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

fn offline_preview() -> PreviewResult {
    PreviewResult::Error(
        "Archivo disponible solo en la nube. Descárgalo para previsualizarlo.".into(),
    )
}

use atlas_model::app_state::BgResult;
use atlas_model::pane::TabId;
use atlas_model::preview_cache::{is_audio_ext, is_image_ext, MultiPreviewKey, PreviewResult};

pub(crate) const MAX_TEXT_BYTES: usize = 256 * 1024;
pub(crate) const MAX_IMAGE_PREVIEW_EDGE: u32 = 1600;
pub(crate) const MAX_MULTI_THUMBNAIL_EDGE: u32 = 384;
pub(crate) const MAX_PDF_PREVIEW_EDGE: &str = "1024";

/// Per-position tilt (degrees) for multi-selection thumbnails. The inspector
/// now frames them in upright stacked cards, so they stay unrotated.
const MULTI_BUTTERFLY_ANGLES: [f32; 4] = [0.0; 4];

pub async fn load_preview(tab: TabId, path: PathBuf, tx: flume::Sender<BgResult>) {
    if crate::vault::is_private_path(&path) {
        let _ = tx.send(BgResult::PreviewReady { tab, path, result: PreviewResult::Binary { size: 0 } });
        return;
    }
    if is_cloud_placeholder(&path) {
        let _ = tx.send(BgResult::PreviewReady {
            tab,
            path,
            result: offline_preview(),
        });
        return;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| {
            path.file_name()
                .and_then(|f| f.to_str())
                .unwrap_or("")
                .to_lowercase()
        });

    let result = if path.is_dir() {
        dir::preview_dir(&path).await
    } else if is_image_ext(&ext) {
        let p = path.clone();
        tokio::task::spawn_blocking(move || image::preview_image(&p))
            .await
            .unwrap_or_else(|e| PreviewResult::Error(e.to_string()))
    } else if crate::vault::live::is_private(&path) && (atlas_model::preview_cache::is_video_ext(&ext) || ext == "pdf") {
        // These providers render via temporary files or OS caches. Opening in an
        // external app remains explicit; automatic private previews stay in memory.
        PreviewResult::Binary { size: std::fs::metadata(&path).map_or(0, |m| m.len()) }
    } else if atlas_model::preview_cache::is_video_ext(&ext) {
        let p = path.clone();
        tokio::task::spawn_blocking(move || video::preview_video(&p))
            .await
            .unwrap_or_else(|e| PreviewResult::Error(e.to_string()))
    } else if ext == "pdf" {
        let p = path.clone();
        tokio::task::spawn_blocking(move || pdf::preview_pdf_public(&p))
            .await
            .unwrap_or_else(|e| PreviewResult::Error(e.to_string()))
    } else if is_audio_ext(&ext) {
        let p = path.clone();
        tokio::task::spawn_blocking(move || audio::preview_audio(&p))
            .await
            .unwrap_or_else(|e| PreviewResult::Error(e.to_string()))
    } else if model3d::is_model_ext(&ext) {
        let p = path.clone();
        tokio::task::spawn_blocking(move || model3d::preview_model(&p))
            .await
            .unwrap_or_else(|e| PreviewResult::Error(e.to_string()))
    } else if matches!(ext.as_str(), "docx" | "csv" | "tsv" | "xls" | "xlsm" | "xlsx" | "ods" | "pptx" | "psd" | "psb") {
        let p = path.clone();
        tokio::task::spawn_blocking(move || crate::ext::runner::native_document_preview(&p))
            .await
            .unwrap_or_else(|e| PreviewResult::Error(e.to_string()))
    } else if crate::fs::archive::is_archive_path(&path) {
        let p = path.clone();
        tokio::task::spawn_blocking(move || archive::preview_archive_listing(&p))
            .await
            .unwrap_or_else(|e| PreviewResult::Error(e.to_string()))
    } else if markdown::is_markdown_ext(&ext) {
        markdown::preview_markdown(&path).await
    } else {
        text::preview_text(&path, &ext).await
    };

    let _ = tx.send(BgResult::PreviewReady { tab, path, result });
}

pub async fn load_multi_thumbnail(
    tab: TabId,
    key: MultiPreviewKey,
    index: usize,
    path: PathBuf,
    tx: flume::Sender<BgResult>,
) {
    let thumbnail_path = path.clone();
    let thumbnail = tokio::task::spawn_blocking(move || {
        if is_cloud_placeholder(&thumbnail_path) {
            return None;
        }
        let ext = thumbnail_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let decoded = if is_audio_ext(&ext) {
            audio::extract_audio_thumbnail(&thumbnail_path, MAX_MULTI_THUMBNAIL_EDGE)
        } else if ext == "svg" {
            image::decode_svg(&thumbnail_path, MAX_MULTI_THUMBNAIL_EDGE)
        } else if model3d::is_model_ext(&ext) {
            model3d::model_thumbnail(&thumbnail_path, MAX_MULTI_THUMBNAIL_EDGE)
        } else {
            image::preview_thumbnail(&thumbnail_path).ok()
        };
        let decoded = decoded.or_else(|| {
            linux_thumb::freedesktop_thumbnail(&thumbnail_path, MAX_MULTI_THUMBNAIL_EDGE)
        });
        let (rgba, width, height) = decoded?;
        let angle = MULTI_BUTTERFLY_ANGLES.get(index).copied().unwrap_or(0.0);
        Some(if angle == 0.0 {
            (rgba, width, height)
        } else {
            image::rotate_rgba(&rgba, width, height, angle)
        })
    })
    .await
    .ok()
    .flatten();
    let Some((rgba, width, height)) = thumbnail else {
        return;
    };
    let _ = tx.send(BgResult::MultiPreviewThumbnailReady {
        tab,
        key,
        path,
        rgba,
        width,
        height,
    });
}
