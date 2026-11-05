#![allow(clippy::disallowed_methods)]
// reason: pdftoppm invocation + temp file cleanup run on blocking workers.

use std::path::Path;
use std::process::Command;

use atlas_model::preview_cache::PreviewResult;

use super::MAX_PDF_PREVIEW_EDGE;

pub fn preview_pdf_public(path: &Path) -> PreviewResult {
    #[cfg(windows)]
    {
        if super::is_cloud_placeholder(path) { return super::offline_preview(); }
        let directory = std::env::temp_dir().join(format!("atlas-native-pdf-{}", uuid::Uuid::new_v4()));
        let rendered = atlas_win_space::pdf::render_first_page(path, &directory, 1600)
            .and_then(|png| image::open(png).map_err(|e| e.to_string()));
        let _ = std::fs::remove_dir_all(&directory);
        if let Ok(img) = rendered {
            let rgba = img.to_rgba8();
            let (width, height) = rgba.dimensions();
            return PreviewResult::Pdf { rgba: rgba.into_raw(), width: width as usize, height: height as usize };
        }
    }
    preview_pdf(path, Path::new("pdftoppm"))
}

pub fn preview_pdf_with_pdftoppm(path: &Path, pdftoppm: &Path) -> PreviewResult {
    preview_pdf(path, pdftoppm)
}

fn preview_pdf(path: &Path, pdftoppm: &Path) -> PreviewResult {
    if super::is_cloud_placeholder(path) {
        return super::offline_preview();
    }
    let output_prefix =
        std::env::temp_dir().join(format!("atlas-pdf-preview-{}", uuid::Uuid::new_v4()));
    let png_path = output_prefix.with_extension("png");
    let mut cmd = Command::new(pdftoppm);
    cmd.args([
        "-f",
        "1",
        "-l",
        "1",
        "-scale-to",
        MAX_PDF_PREVIEW_EDGE,
        "-png",
        "-singlefile",
    ])
    .arg(path)
    .arg(&output_prefix);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let output = match cmd.output() {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return PreviewResult::Error("PDF preview unavailable: pdftoppm not found".to_string());
        }
        Err(e) => return PreviewResult::Error(format!("Could not start pdftoppm: {e}")),
    };
    if !output.status.success() {
        let _ = std::fs::remove_file(&png_path);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return PreviewResult::Error(format!("Could not render PDF: {}", stderr.trim()));
    }
    let img = match image::open(&png_path) {
        Ok(img) => img,
        Err(e) => {
            let _ = std::fs::remove_file(&png_path);
            return PreviewResult::Error(format!("Could not decode PDF preview: {e}"));
        }
    };
    let _ = std::fs::remove_file(&png_path);
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    PreviewResult::Pdf {
        rgba: rgba.into_raw(),
        width: width as usize,
        height: height as usize,
    }
}
