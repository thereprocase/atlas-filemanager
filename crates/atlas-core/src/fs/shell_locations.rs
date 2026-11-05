//! Shell folders have no writable filesystem destination.
use atlas_model::file_entry::{FileEntry, FileEntryData, FileKind};
pub const COMPUTER: &str = "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}";
pub const NETWORK: &str = "::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}";
pub fn list(path: &str) -> anyhow::Result<Vec<FileEntry>> {
    #[cfg(windows)]
    { Ok(atlas_win_space::shell_locations::enumerate(path).map_err(anyhow::Error::msg)?.into_iter().map(|item| FileEntryData { name: item.name.into(), path: item.path, kind: if item.folder { FileKind::Dir } else { FileKind::File }, size: None, modified: None, _is_hidden: false, _is_symlink: false, archive_root: None, recycle_info: None }.into()).collect()) }
    #[cfg(not(windows))]
    { let _ = path; anyhow::bail!("Windows shell locations require Windows") }
}
