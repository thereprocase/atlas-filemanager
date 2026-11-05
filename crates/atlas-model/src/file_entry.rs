pub use compact_str::CompactString;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct RecycleBinInfo {
    pub original_path: PathBuf,
    pub deleted_at: SystemTime,
    pub i_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FileKind {
    Dir,
    File,
    Symlink,
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize, Default,
)]
pub enum SortColumn {
    #[default]
    Name,
    Size,
    Modified,
    Extension,
    Kind,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}

/// Construction data, consumed immediately rather than retained for every row.
pub struct FileEntryData {
    pub name: CompactString,
    pub path: PathBuf,
    pub kind: FileKind,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    pub _is_hidden: bool,
    pub _is_symlink: bool,
    /// When `Some(archive_path)`, this entry is a virtual item inside that archive
    /// and `path` is its path relative to the archive root.
    pub archive_root: Option<PathBuf>,
    /// Populated only for entries listed from `Location::RecycleBin`.
    pub recycle_info: Option<RecycleBinInfo>,
}

/// Shared directory identity. Exceptional entries retain their exact native path
/// and metadata here (trash, archives and filenames that are not valid UTF-8).
#[derive(Debug, Clone)]
pub struct EntryDirectory {
    parent: PathBuf,
    exact_path: Option<PathBuf>,
    archive_root: Option<PathBuf>,
    recycle_info: Option<RecycleBinInfo>,
    file_ids: Arc<std::sync::RwLock<Vec<u128>>>,
}

impl EntryDirectory {
    pub fn new(parent: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            parent,
            exact_path: None,
            archive_root: None,
            recycle_info: None,
            file_ids: Arc::default(),
        })
    }
}

/// A directory row occupies at most 64 bytes, including a small inline filename.
/// Paths and presentation strings are derived on demand; directory snapshots
/// share their parent. Keep full SystemTime precision and the entire u64 size
/// range, including zero and u64::MAX, instead of using lossy sentinel values.
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub name: CompactString,
    pub modified: Option<SystemTime>,
    directory: Arc<EntryDirectory>,
    size_bytes: u64,
    file_id_index: u32,
    flags: u16,
}

/// The compact representation is the actual entry used throughout Atlas.
pub type CompactFileEntry = FileEntry;
const _: () = assert!(std::mem::size_of::<FileEntry>() <= 64);

impl From<FileEntryData> for FileEntry {
    fn from(data: FileEntryData) -> Self {
        let directory = EntryDirectory::new(
            data.path
                .parent()
                .unwrap_or(std::path::Path::new(""))
                .to_path_buf(),
        );
        Self::in_directory(data, directory)
    }
}

impl FileEntry {
    pub fn in_directory(data: FileEntryData, directory: Arc<EntryDirectory>) -> Self {
        let reconstructs = data.path.parent() == Some(directory.parent.as_path())
            && data.path.file_name().and_then(std::ffi::OsStr::to_str) == Some(data.name.as_str());
        let directory =
            if reconstructs && data.archive_root.is_none() && data.recycle_info.is_none() {
                directory
            } else {
                Arc::new(EntryDirectory {
                    parent: directory.parent.clone(),
                    exact_path: (!reconstructs).then_some(data.path),
                    archive_root: data.archive_root,
                    recycle_info: data.recycle_info,
                    file_ids: directory.file_ids.clone(),
                })
            };
        let flags = u16::from(data.kind == FileKind::Dir)
            | (u16::from(data.kind == FileKind::Symlink || data._is_symlink) << 1)
            | (u16::from(data._is_hidden) << 2)
            | (u16::from(data.size.is_some()) << 5);
        Self {
            name: data.name,
            modified: data.modified,
            directory,
            size_bytes: data.size.unwrap_or(0),
            file_id_index: u32::MAX,
            flags,
        }
    }

    pub fn path(&self) -> PathBuf {
        self.directory
            .exact_path
            .clone()
            .unwrap_or_else(|| self.directory.parent.join(self.name.as_str()))
    }

    pub fn size(&self) -> Option<u64> {
        (self.flags & (1 << 5) != 0).then_some(self.size_bytes)
    }

    pub fn kind(&self) -> FileKind {
        if self.flags & 1 != 0 {
            FileKind::Dir
        } else if self.flags & 2 != 0 {
            FileKind::Symlink
        } else {
            FileKind::File
        }
    }

    pub fn is_hidden(&self) -> bool {
        self.flags & (1 << 2) != 0
    }
    pub fn is_symlink(&self) -> bool {
        self.flags & 2 != 0
    }
    pub fn is_readonly(&self) -> bool {
        self.flags & (1 << 3) != 0
    }
    pub fn is_cloud_placeholder(&self) -> bool {
        self.flags & (1 << 4) != 0
    }

    /// Retain metadata already returned by the native enumerator, with no opens
    /// or reads. Full 128-bit IDs live in one contiguous directory-owned table;
    /// the row retains only its index, keeping its own size below 64 bytes.
    pub fn with_native_metadata(mut self, attributes: u32, file_id: u128) -> Self {
        self.flags &= !((1 << 3) | (1 << 4));
        self.flags |= u16::from(attributes & 1 != 0) << 3;
        self.flags |= u16::from(attributes & (0x0040_0000 | 0x0004_0000 | 0x0000_1000) != 0) << 4;
        if file_id != 0 {
            let mut ids = self
                .directory
                .file_ids
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(index) = u32::try_from(ids.len()).ok().filter(|index| *index != u32::MAX) {
                self.file_id_index = index;
                ids.push(file_id);
            }
        }
        self
    }

    /// Keep the cloud safety bit without adding per-row storage to the compact
    /// entry. The enumerator already obtained the tag without reading streams.
    pub fn with_native_reparse_metadata(self, attributes: u32, file_id: u128, tag: u32) -> Self {
        let mut entry = self.with_native_metadata(attributes, file_id);
        if tag & !0x0000_f000 == 0x9000_001a || tag == 0x8000_0021 {
            entry.flags |= 1 << 4;
        }
        entry
    }

    pub fn file_id(&self) -> Option<u128> {
        if self.file_id_index == u32::MAX {
            return None;
        }
        self.directory
            .file_ids
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(self.file_id_index as usize)
            .copied()
    }

    pub fn extension(&self) -> Option<CompactString> {
        std::path::Path::new(self.name.as_str())
            .extension()
            .and_then(std::ffi::OsStr::to_str)
            .map(lowercase_compact)
    }

    pub fn name_lower(&self) -> CompactString {
        lowercase_compact(&self.name)
    }

    pub fn archive_root(&self) -> Option<PathBuf> {
        self.directory.archive_root.clone()
    }

    pub fn recycle_info(&self) -> Option<&RecycleBinInfo> {
        self.directory.recycle_info.as_ref()
    }

    /// Search results may display a relative breadcrumb instead of the basename.
    /// Preserve the real path before changing the label, including after cloning.
    pub fn set_display_name(&mut self, name: CompactString) {
        let path = self.path();
        Arc::make_mut(&mut self.directory).exact_path = Some(path);
        self.name = name;
    }
}

fn lowercase_compact(text: &str) -> CompactString {
    if text.is_ascii() {
        let mut text = CompactString::from(text);
        text.make_ascii_lowercase();
        text
    } else {
        text.to_lowercase().into()
    }
}

pub fn file_category(kind: &FileKind, ext: Option<&str>) -> &'static str {
    match kind {
        FileKind::Dir => "file_kind_folder",
        FileKind::Symlink => "file_kind_symlink",
        FileKind::File => {
            let Some(e) = ext else {
                return "file_kind_file";
            };
            match e.to_ascii_lowercase().as_str() {
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "ico" | "tif" | "tiff"
                | "avif" | "heic" | "heif" | "svg" => "file_kind_image",
                "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "wma" | "opus" => {
                    "file_kind_audio"
                }
                "mp4" | "mov" | "mkv" | "webm" | "avi" | "wmv" | "m4v" | "flv" => {
                    "file_kind_video"
                }
                "rs" | "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" | "cts" | "py"
                | "pyw" | "go" | "java" | "c" | "cc" | "cpp" | "cxx" | "h" | "hpp" | "cs"
                | "csx" | "php" | "rb" | "swift" | "kt" | "kts" | "sh" | "bash" | "zsh"
                | "fish" | "ps1" | "bat" | "cmd" | "lua" | "dart" | "zig" | "nim" | "r"
                | "scala" | "ex" | "exs" | "clj" | "hs" | "ml" | "html" | "htm" | "xhtml"
                | "xml" | "vue" | "svelte" | "astro" | "css" | "scss" | "sass" | "less"
                | "json" | "jsonc" | "json5" | "yaml" | "yml" | "toml" | "sql" | "graphql"
                | "gql" | "dockerfile" | "makefile" => "file_kind_code",
                "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "tgz" | "zst" => {
                    "file_kind_archive"
                }
                "doc" | "docx" | "odt" | "rtf" | "pdf" | "txt" | "md" | "markdown" | "rst"
                | "tex" => "file_kind_document",
                "xls" | "xlsm" | "xlsx" | "csv" | "tsv" | "ods" | "numbers" => {
                    "file_kind_spreadsheet"
                }
                "ppt" | "pptx" | "odp" | "key" => {
                    "file_kind_presentation"
                }
                "exe" | "msi" | "app" | "dmg" | "deb" | "rpm" => {
                    "file_kind_application"
                }
                _ => "file_kind_file",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(path: PathBuf, size: Option<u64>) -> FileEntryData {
        FileEntryData {
            name: path.file_name().unwrap().to_string_lossy().as_ref().into(),
            path,
            kind: FileKind::File,
            size,
            modified: None,
            _is_hidden: false,
            _is_symlink: false,
            archive_root: None,
            recycle_info: None,
        }
    }

    #[test]
    fn compact_entries_share_parent_and_preserve_sizes_and_precise_timestamps() {
        assert!(std::mem::size_of::<CompactFileEntry>() <= 64);
        let parent = PathBuf::from("shared-parent");
        let directory = EntryDirectory::new(parent.clone());
        let timestamp = std::time::UNIX_EPOCH - std::time::Duration::new(1, 123_456_789);
        for size in [None, Some(0), Some(u64::MAX)] {
            let mut input = data(parent.join("café.TXT"), size);
            input.modified = Some(timestamp);
            let entry = FileEntry::in_directory(input, directory.clone());
            assert!(Arc::ptr_eq(&entry.directory, &directory));
            assert_eq!(entry.path(), parent.join("café.TXT"));
            assert_eq!(entry.extension().as_deref(), Some("txt"));
            assert_eq!(entry.size(), size);
            assert_eq!(entry.modified, Some(timestamp));
        }
    }

    #[test]
    fn search_labels_and_clones_keep_the_real_path() {
        let original = FileEntry::from(data(PathBuf::from("root/sub/file.txt"), Some(42)));
        let mut search = original.clone();
        search.set_display_name("sub/file.txt".into());
        search.set_display_name("alternate label.txt".into());
        assert_eq!(search.path(), original.path());
        assert_eq!(original.name, "file.txt");
        assert_eq!(search.name_lower(), "alternate label.txt");
    }

    #[test]
    fn native_attributes_and_128_bit_ids_survive_sorting_cloning_and_search_labels() {
        let parent = EntryDirectory::new(PathBuf::from("root"));
        let first = FileEntry::in_directory(data(PathBuf::from("root/a"), None), parent.clone())
            .with_native_metadata(0x0040_0001, u128::MAX);
        let second = FileEntry::in_directory(data(PathBuf::from("root/b"), Some(0)), parent)
            .with_native_metadata(0x0004_0000, 1 << 100);
        assert!(first.is_cloud_placeholder());
        assert!(first.is_readonly());
        assert!(second.is_cloud_placeholder());
        assert!(!second.is_readonly());
        let mut reversed = [second.clone(), first.clone()];
        reversed[1].set_display_name("search label".into());
        assert_eq!(reversed[0].file_id(), Some(1 << 100));
        assert_eq!(reversed[1].file_id(), Some(u128::MAX));
        assert_eq!(first.file_id(), Some(u128::MAX));
        assert_eq!(reversed[1].path(), PathBuf::from("root/a"));
        assert!(!CompactString::from("TXT").is_heap_allocated());
    }

    #[test]
    fn cloud_reparse_tags_are_preserved_in_the_compact_safety_bit() {
        for tag in (0..16).map(|provider| 0x9000_001a | provider << 12).chain([0x8000_0021]) {
            let entry = FileEntry::from(data(PathBuf::from("root/cloud"), Some(42)))
                .with_native_reparse_metadata(0x400, 0, tag);
            assert!(entry.is_cloud_placeholder(), "tag={tag:x}");
            assert_eq!(entry.size(), Some(42));
            assert!(std::mem::size_of_val(&entry) <= 64);
        }
        let link = FileEntry::from(data(PathBuf::from("root/link"), Some(0)))
            .with_native_reparse_metadata(0x400, 0, 0xa000_000c);
        assert!(!link.is_cloud_placeholder());
    }

    #[test]
    fn archive_and_recycle_entries_retain_exceptional_metadata() {
        let mut archive = data(PathBuf::from("sub/file.txt"), Some(1));
        archive.archive_root = Some(PathBuf::from("archive.zip"));
        let archive = FileEntry::from(archive);
        assert_eq!(archive.archive_root(), Some(PathBuf::from("archive.zip")));
        assert_eq!(archive.path(), PathBuf::from("sub/file.txt"));
        let mut trash = data(PathBuf::from("bin/$R123"), Some(2));
        trash.name = "original.txt".into();
        trash.recycle_info = Some(RecycleBinInfo {
            original_path: PathBuf::from("documents/original.txt"),
            deleted_at: std::time::UNIX_EPOCH,
            i_file: PathBuf::from("bin/$I123"),
        });
        let trash = FileEntry::from(trash);
        assert_eq!(trash.path(), PathBuf::from("bin/$R123"));
        assert_eq!(trash.extension().as_deref(), Some("txt"));
        assert_eq!(
            trash.recycle_info().unwrap().original_path,
            PathBuf::from("documents/original.txt")
        );
    }

    #[cfg(windows)]
    #[test]
    fn unpaired_utf16_filename_keeps_the_exact_native_path() {
        use std::os::windows::ffi::OsStringExt;
        let path =
            PathBuf::from("root").join(std::ffi::OsString::from_wide(&[0xd800, 46, 116, 120, 116]));
        let entry = FileEntry::from(data(path.clone(), None));
        assert_eq!(entry.path(), path);
    }

    #[cfg(unix)]
    #[test]
    fn invalid_utf8_filename_keeps_the_exact_native_path() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from("root").join(std::ffi::OsString::from_vec(b"\xff.txt".to_vec()));
        let entry = FileEntry::from(data(path.clone(), None));
        assert_eq!(entry.path(), path);
    }

    #[test]
    fn common_filename_and_extension_need_no_heap_allocation() {
        for value in ["report.txt", "archivo-ñ.txt", "txt"] {
            assert!(!CompactString::from(value).is_heap_allocated());
        }
        assert_eq!(
            std::mem::size_of::<CompactString>(),
            std::mem::size_of::<String>()
        );
        let long_name = "long-unicode-文件名-that-does-not-fit-inline.txt";
        assert_eq!(CompactString::from(long_name).as_str(), long_name);
    }
}
