use crate::tag_cache::TagId;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Location {
    Directory(PathBuf),
    Shell(String),
    Archive {
        archive: PathBuf,
        inner: PathBuf,
    },
    Tag(TagId),
    RecycleBin,
    #[cfg(feature = "provider-host")]
    Remote {
        id: String,
        path: String,
    },
}

impl Location {
    /// Real filesystem directory for this location; `None` for archives, tags, remotes.
    /// Use for: filesystem watchers, DnD drops, paste destinations, goto-root.
    pub fn fs_dir(&self) -> Option<&Path> {
        match self {
            Self::Directory(p) => Some(p),
            _ => None,
        }
    }

    /// Path suitable for breadcrumbs and title bars.
    pub fn display_path(&self) -> PathBuf {
        match self {
            Self::Directory(p) => p.clone(),
            Self::Shell(path) => PathBuf::from(path),
            Self::Archive { archive, inner } => {
                if inner == Path::new("") {
                    archive.clone()
                } else {
                    archive.join(inner)
                }
            }
            Self::Tag(id) => PathBuf::from(format!("tag:{}", id.0)),
            Self::RecycleBin => PathBuf::from("Recycle Bin"),
            #[cfg(feature = "provider-host")]
            Self::Remote { id, path } => PathBuf::from(format!("{id}:{}", crate::remote_path::display(path))),
        }
    }

    /// `(archive_file, inner_path)` when browsing inside an archive, else `None`.
    pub fn as_archive(&self) -> Option<(&Path, &Path)> {
        match self {
            Self::Archive { archive, inner } => Some((archive, inner)),
            _ => None,
        }
    }

    pub fn is_directory(&self) -> bool {
        matches!(self, Self::Directory(_))
    }

    pub fn is_archive(&self) -> bool {
        matches!(self, Self::Archive { .. })
    }

    /// Navigate one level up, returning the parent `Location`.
    /// Returns `None` at filesystem roots and for tags/remotes.
    pub fn ascend(&self) -> Option<Location> {
        match self {
            Self::Directory(p) => {
                let parent = p.parent()?;
                if parent == p.as_path() {
                    return None;
                }
                Some(Self::Directory(parent.to_path_buf()))
            }
            Self::Archive { archive, inner } => {
                if inner == Path::new("") {
                    Some(Self::Directory(archive.parent()?.to_path_buf()))
                } else {
                    let parent_inner = inner.parent().unwrap_or(Path::new("")).to_path_buf();
                    Some(Self::Archive {
                        archive: archive.clone(),
                        inner: parent_inner,
                    })
                }
            }
            Self::Shell(path) => {
                if is_unc_host(path) { Some(Self::Shell("::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}".into())) }
                else { path.rsplit_once('\\').map(|(parent, _)| Self::Shell(parent.into())) }
            }
            Self::Tag(_) | Self::RecycleBin => None,
            #[cfg(feature = "provider-host")]
            Self::Remote { id, path } => {
                let trimmed = path.trim_matches('/');
                if trimmed.is_empty() {
                    None
                } else {
                    let parent = match trimmed.rsplit_once('/') {
                        Some((p, _)) => p.to_string(),
                        None => String::new(),
                    };
                    Some(Self::Remote {
                        id: id.clone(),
                        path: parent,
                    })
                }
            }
        }
    }
}

impl Default for Location {
    fn default() -> Self {
        Self::Directory(PathBuf::new())
    }
}

#[cfg(all(test, feature = "provider-host"))]
mod tests {
    use super::*;

    #[test]
    fn test_remote_ascend() {
        let nested = Location::Remote {
            id: "drive".into(),
            path: "folder/subfolder".into(),
        };
        let parent = nested.ascend().unwrap();
        assert_eq!(
            parent,
            Location::Remote {
                id: "drive".into(),
                path: "folder".into(),
            }
        );

        let root_parent = parent.ascend().unwrap();
        assert_eq!(
            root_parent,
            Location::Remote {
                id: "drive".into(),
                path: String::new(),
            }
        );

        assert_eq!(root_parent.ascend(), None);
    }
}

/// A UNC host is a shell container; shares and files are filesystem paths.
pub fn is_unc_host(path: &str) -> bool {
    path.strip_prefix(r"\\").is_some_and(|rest| {
        let host = rest.trim_end_matches('\\');
        !host.is_empty() && !host.contains('\\') && !matches!(host, "?" | ".")
    })
}
#[cfg(test)]
mod shell_navigation_tests {
    use super::*;
    #[test]
    fn unc_hosts_are_separate_from_shares_files_and_device_paths() {
        assert!(is_unc_host(r"\\nas"));
        assert!(is_unc_host(r"\\192.168.1.20\"));
        for path in [r"\\nas\work", r"\\nas\work\plan.xlsm", r"C:\work", r"\\?\C:\work", r"\\."] { assert!(!is_unc_host(path), "{path}"); }
    }
    #[test]
    fn shell_containers_do_not_supply_filesystem_destinations() {
        let location = Location::Shell("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".into());
        assert!(location.fs_dir().is_none());
        assert!(!location.is_directory());
    }
}
