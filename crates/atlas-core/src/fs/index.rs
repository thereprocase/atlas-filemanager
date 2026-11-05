#![allow(dead_code)] // reason: retained API surface not exercised by every GPUI flow yet
use crate::search::nucleo_pool::IndexEntry;
use crate::util::display::is_hidden;
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

pub fn build_index(current_root: PathBuf, injector: nucleo::Injector<IndexEntry>) {
    rayon::spawn(move || {
        let home = dirs::home_dir().unwrap_or_else(|| current_root.clone());
        let mut seen = HashSet::new();

        // Browsing a WSL distro: its folders are what the user is completing.
        // Index its home deeply (and the rest of the distro shallowly) first.
        if super::paths::wsl_distro_name(&current_root).is_some() {
            let wsl_home = wsl_home_of(&current_root);
            index_tree(&wsl_home, WSL_HOME_DEPTH, &wsl_home, &mut seen, &injector, WSL_ENTRY_CAP, is_linux_system_dir);
            let distro = wsl_distro_root(&current_root);
            if distro != wsl_home {
                index_tree(&distro, 2, &distro, &mut seen, &injector, WSL_ENTRY_CAP * 2, is_linux_system_dir);
            }
        }

        for path in common_folders()
            .into_iter()
            .chain([home.clone(), current_root.clone()])
        {
            inject_folder(path, &home, &mut seen, &injector);
        }

        index_tree(&home, 5, &home, &mut seen, &injector, usize::MAX, is_heavy_dir);
        if !current_root.starts_with(&home) {
            index_tree(&current_root, 3, &home, &mut seen, &injector, usize::MAX, is_heavy_dir);
        }
        index_files(&current_root, 4, 10_000, &injector);
    });
}

/// The global goto index; `extended` (a profile opt-in) adds every drive and WSL distro.
pub fn build_goto_index(root: PathBuf, injector: nucleo::Injector<IndexEntry>, extended: bool) {
    if extended {
        build_extended_index(injector.clone());
    }
    build_index(root, injector);
}

/// Lightweight directory-only warm index used by Goto (never repointed by scoped searches).
pub fn build_warm_navigation_index(injector: nucleo::Injector<IndexEntry>) {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    build_index(home, injector);
}

/// Scope cap: a drilled-into folder is walked deeper than the global index but
/// still bounded so pathological trees (node_modules…) don't hang the walker.
const SCOPED_MAX_DEPTH: usize = 6;
const SCOPED_MAX_ENTRIES: usize = 50_000;

/// Index *only* `root` — folders **and files** — matching against the path
/// relative to `root`, so the needle filters within the folder instead of
/// trivially matching every descendant's absolute path. Used when the goto
/// query carries an absolute-path prefix (drill-in).
pub fn build_index_scoped(root: PathBuf, injector: nucleo::Injector<IndexEntry>) {
    rayon::spawn(move || {
        let mut injected = 0usize;
        for entry in WalkDir::new(&root)
            .max_depth(SCOPED_MAX_DEPTH)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| !super::scan_policy::is_scan_excluded(entry.path()) && !super::preview::is_cloud_placeholder(entry.path()) && (entry.depth() == 0 || !is_hidden(entry.path())))
            .flatten()
        {
            if entry.depth() == 0 {
                continue;
            }
            let is_dir = entry.file_type().is_dir();
            let path = entry.into_path();
            let display = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            injector.push(IndexEntry { path, is_dir }, move |_item, cols| {
                cols[0] = display.as_str().into();
            });
            injected += 1;
            if injected >= SCOPED_MAX_ENTRIES {
                break;
            }
        }
    });
}

pub fn prioritize_folders(folders: &mut [PathBuf], current: Option<&Path>) {
    let home = dirs::home_dir();
    let common = common_folders();
    folders.sort_by_key(|path| {
        let priority = if current == Some(path.as_path()) {
            0
        } else if common.iter().any(|folder| folder == path)
            || home.as_ref().is_some_and(|folder| folder == path)
        {
            1
        } else if current.is_some_and(|folder| path.starts_with(folder)) {
            2
        } else if common.iter().any(|folder| path.starts_with(folder)) {
            3
        } else if home.as_ref().is_some_and(|folder| path.starts_with(folder)) {
            4
        } else {
            5
        };
        (priority, path.components().count(), path.clone())
    });
}

fn index_tree(
    root: &Path,
    max_depth: usize,
    home: &Path,
    seen: &mut HashSet<PathBuf>,
    injector: &nucleo::Injector<IndexEntry>,
    limit: usize,
    prune: fn(&str) -> bool,
) {
    // Breadth first: publish nearby folders before descending into large
    // AppData, dependency or document trees. A new project at home/developer/
    // git/myproj must not wait behind every deep subtree that sorts before it.
    inject_folder(root.to_path_buf(), home, seen, injector);
    let mut pending = VecDeque::from([(root.to_path_buf(), 0_usize)]);
    while let Some((directory, depth)) = pending.pop_front() {
        if depth >= max_depth || seen.len() >= limit {
            continue;
        }
        if super::scan_policy::is_scan_excluded(&directory) || super::preview::is_cloud_placeholder(&directory) { continue; }
        let Ok(children) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut folders: Vec<_> = children
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir() && !kind.is_symlink()))
            .map(|entry| entry.path())
            .filter(|path| !is_hidden(path))
            .collect();
        folders.sort();
        for folder in folders {
            // `file_type` above already proved it is a directory: skip the
            // extra stat, which is a network round trip on WSL shares.
            inject_known_folder(folder.clone(), home, seen, injector);
            // Massive trees (target, node_modules, /usr…) stay findable by name
            // but are never descended into.
            if !folder.file_name().and_then(|n| n.to_str()).is_some_and(prune) {
                pending.push_back((folder, depth + 1));
            }
        }
    }
}

fn index_files(
    root: &Path,
    max_depth: usize,
    max_entries: usize,
    injector: &nucleo::Injector<IndexEntry>,
) {
    let mut count = 0;
    for entry in WalkDir::new(root)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !super::scan_policy::is_scan_excluded(e.path()) && !super::preview::is_cloud_placeholder(e.path()) && (e.depth() == 0 || !is_hidden(e.path())))
        .flatten()
    {
        if entry.depth() == 0 || entry.file_type().is_dir() {
            continue;
        }
        let path = entry.into_path();
        let display = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        injector.push(IndexEntry { path, is_dir: false }, move |_item, cols| {
            cols[0] = display.as_str().into();
        });
        count += 1;
        if count >= max_entries {
            break;
        }
    }
}

fn inject_folder(
    path: PathBuf,
    home: &Path,
    seen: &mut HashSet<PathBuf>,
    injector: &nucleo::Injector<IndexEntry>,
) {
    if !path.is_dir() {
        return;
    }
    inject_known_folder(path, home, seen, injector);
}

fn inject_known_folder(
    path: PathBuf,
    home: &Path,
    seen: &mut HashSet<PathBuf>,
    injector: &nucleo::Injector<IndexEntry>,
) {
    if !seen.insert(path.clone()) {
        return;
    }
    // Match against the home-relative path when possible: the constant
    // "C:\Users\name\" prefix only adds spurious match material. Home itself
    // would strip to "", so it keeps its absolute form.
    let rel = path.strip_prefix(home).unwrap_or(&path);
    let display = if rel.as_os_str().is_empty() {
        path.to_string_lossy().into_owned()
    } else {
        rel.to_string_lossy().into_owned()
    };
    injector.push(IndexEntry { path, is_dir: true }, move |_item, cols| {
        cols[0] = display.as_str().into();
    });
}

const WSL_HOME_DEPTH: usize = 6;
const WSL_ENTRY_CAP: usize = 60_000;
const EXTENDED_DEPTH: usize = 4;
const EXTENDED_ENTRY_CAP: usize = 250_000;

/// Dependency and build output trees: listed, never walked.
fn is_heavy_dir(name: &str) -> bool {
    matches!(name, "node_modules" | "target" | "__pycache__" | "vendor" | "venv" | "dist" | "build")
}

/// Linux trees that are never a navigation target and are huge over 9P.
fn is_linux_system_dir(name: &str) -> bool {
    matches!(
        name,
        "proc" | "sys" | "dev" | "run" | "usr" | "lib" | "lib32" | "lib64" | "bin" | "sbin"
            | "boot" | "snap" | "lost+found"
    ) || is_heavy_dir(name)
}

/// System and dependency trees skipped by the opt-in whole-disk index.
fn is_extended_skip(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    matches!(
        name.as_str(),
        "windows" | "$recycle.bin" | "system volume information" | "winsxs" | "node_modules"
            | "recovery" | "msocache"
    ) || is_linux_system_dir(&name)
}

/// The distro root (`\wsl.localhost\Arch\`) containing `path`.
fn wsl_distro_root(path: &Path) -> PathBuf {
    let mut root = PathBuf::new();
    for (index, component) in path.components().enumerate() {
        root.push(component);
        // `\wsl.localhost\Arch` is a prefix plus a root dir; the prefix already
        // carries the distro name.
        if index == 1 {
            break;
        }
    }
    root
}

/// `\wsl.localhost\Arch\home\me` when `path` is inside it, else the distro root.
fn wsl_home_of(path: &Path) -> PathBuf {
    let root = wsl_distro_root(path);
    let mut rest = path.strip_prefix(&root).unwrap_or(path).components();
    match (rest.next(), rest.next()) {
        (Some(home), Some(user)) if home.as_os_str().eq_ignore_ascii_case("home") => {
            root.join(home).join(user)
        }
        _ => root,
    }
}

/// Opt-in: also make every local drive and WSL distro searchable by folder
/// name. Breadth first and capped, so the nearest folders arrive first and the
/// memory cost stays bounded (roughly 100 bytes per folder).
pub fn build_extended_index(injector: nucleo::Injector<IndexEntry>) {
    rayon::spawn(move || {
        let roots: Vec<PathBuf> = super::mounts::list_drives()
            .into_iter()
            .filter(|drive| !drive.is_network)
            .map(|drive| drive.mount_point)
            .chain(super::paths::list_wsl_roots())
            .collect();
        let mut seen = HashSet::new();
        for root in roots {
            index_tree(&root, EXTENDED_DEPTH, &root, &mut seen, &injector, EXTENDED_ENTRY_CAP, is_extended_skip);
        }
    });
}

fn common_folders() -> Vec<PathBuf> {
    [
        dirs::desktop_dir(),
        dirs::download_dir(),
        dirs::document_dir(),
        dirs::picture_dir(),
        dirs::video_dir(),
        dirs::audio_dir(),
    ]
    .into_iter()
    .flatten()
    .collect()
}

#[cfg(test)]
mod tests {
    use super::{index_tree, prioritize_folders};
    use crate::search::nucleo_pool::NucleoPool;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[test]
    fn fresh_project_folders_are_published_before_deep_subtrees() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path();
        let project = root.join("developer/git/myproj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(root.join("a-deep/one/two/three/four/beyond-limit")).unwrap();
        std::fs::create_dir_all(root.join(".hidden/ignored")).unwrap();
        let mut pool = NucleoPool::new();
        let mut seen = HashSet::new();
        index_tree(root, 5, root, &mut seen, &pool.injector(), usize::MAX, super::is_heavy_dir);
        let deadline = Instant::now() + Duration::from_secs(5);
        while pool.nucleo.snapshot().item_count() as usize != seen.len() {
            pool.tick();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let snapshot = pool.nucleo.snapshot();
        let paths: Vec<_> = (0..snapshot.item_count())
            .map(|index| snapshot.get_item(index).unwrap().data.path.clone())
            .collect();
        let depths: Vec<_> = paths.iter().map(|path| path.strip_prefix(root).unwrap().components().count()).collect();
        assert!(depths.windows(2).all(|pair| pair[0] <= pair[1]), "shallow project folders must arrive first");
        assert!(paths.contains(&project));
        assert!(!paths.contains(&root.join(".hidden")));
        assert!(!paths.contains(&root.join("a-deep/one/two/three/four/beyond-limit")));
        pool.set_pattern("myproj");
        while pool.results(100).iter().all(|entry| entry.path != project) {
            pool.tick();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[cfg(windows)]
    #[test]
    fn wsl_home_is_found_from_any_path_inside_the_distro() {
        use super::{wsl_distro_root, wsl_home_of};
        use std::path::Path;
        let deep = Path::new(r"\\wsl.localhost\Arch\home\me\dev\x");
        assert_eq!(wsl_home_of(deep), Path::new(r"\\wsl.localhost\Arch\home\me"));
        let etc = Path::new(r"\\wsl.localhost\Arch\etc\systemd");
        assert_eq!(wsl_home_of(etc), wsl_distro_root(etc));
        assert_eq!(wsl_distro_root(etc), Path::new(r"\\wsl.localhost\Arch\"));
    }

    #[test]
    fn prioritizes_current_folder_and_its_nearby_children() {
        let current = PathBuf::from("/tmp/atlas-project");
        let child = current.join("src");
        let nested = child.join("components");
        let mut folders = vec![nested.clone(), child.clone(), current.clone()];

        prioritize_folders(&mut folders, Some(&current));

        assert_eq!(folders, vec![current, child, nested]);
    }
}
