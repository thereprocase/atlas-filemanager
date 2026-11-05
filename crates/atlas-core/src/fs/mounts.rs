#![allow(dead_code)] // reason: retained API surface not exercised by every GPUI flow yet
use std::path::PathBuf;
use std::time::{Duration, Instant};

use std::sync::{Arc, Mutex, OnceLock};

thread_local! {
    static MOUNT_CACHE: std::cell::RefCell<Option<(Instant, Vec<PathBuf>)>> = const { std::cell::RefCell::new(None) };
}

static DRIVE_CACHE: OnceLock<Arc<Mutex<Option<(Instant, Vec<DriveInfo>, bool)>>>> = OnceLock::new();

/// A mounted volume with capacity info, used to render a fullness bar in the sidebar.
#[derive(Debug, Clone)]
pub struct DriveInfo {
    /// Human label (volume name, or the mount point basename / drive root as fallback).
    pub label: String,
    pub mount_point: PathBuf,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub is_removable: bool,
    pub is_system: bool,
}

impl DriveInfo {
    /// Fraction of the volume in use, `0.0..=1.0`. Zero when total is unknown.
    pub fn used_fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        let used = self.total_bytes.saturating_sub(self.available_bytes);
        (used as f64 / self.total_bytes as f64).clamp(0.0, 1.0) as f32
    }
}

/// Enumerate mounted volumes with capacity, cached for 5s (sysinfo refresh is not free).
/// Cross-platform via `sysinfo`: on Windows these are drive roots like `C:\`, on
/// macOS `/` plus `/Volumes/*`, on Linux the real mounted filesystems.
pub fn list_drives() -> Vec<DriveInfo> {
    let cache_arc = DRIVE_CACHE
        .get_or_init(|| Arc::new(Mutex::new(None)))
        .clone();
    let mut cache = cache_arc.lock().unwrap();

    if let Some((last_update, drives, is_updating)) = &mut *cache {
        if last_update.elapsed() < Duration::from_secs(5) {
            return drives.clone();
        }
        if !*is_updating {
            *is_updating = true;
            let cache_clone = cache_arc.clone();
            std::thread::spawn(move || {
                let new_drives = list_drives_internal();
                if let Ok(mut c) = cache_clone.lock() {
                    *c = Some((Instant::now(), new_drives, false));
                }
            });
        }
        return drives.clone();
    }

    let drives = list_drives_internal();
    *cache = Some((Instant::now(), drives.clone(), false));
    drives
}

fn list_drives_internal() -> Vec<DriveInfo> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut drives: Vec<DriveInfo> = disks
        .list()
        .iter()
        .map(|disk| {
            let mount_point = disk.mount_point().to_path_buf();
            let name = disk.name().to_string_lossy();
            let label = if name.trim().is_empty() {
                mount_point.file_name().map_or_else(
                    || mount_point.to_string_lossy().into_owned(),
                    |s| s.to_string_lossy().into_owned(),
                )
            } else {
                name.into_owned()
            };
            let is_system = is_system_mount(&mount_point);
            let is_removable = disk.is_removable();
            DriveInfo {
                label,
                mount_point,
                total_bytes: disk.total_space(),
                available_bytes: disk.available_space(),
                is_removable,
                is_system,
            }
        })
        .collect();
    #[cfg(windows)]
    for (root, _network) in atlas_win_space::storage::logical_drive_roots() {
        if !drives.iter().any(|drive| drive.mount_point == root) {
            drives.push(DriveInfo { label: root.to_string_lossy().into_owned(), mount_point: root, total_bytes: 0, available_bytes: 0, is_removable: false, is_system: false });
        }
    }
    drives.sort_by(|a, b| a.mount_point.cmp(&b.mount_point));
    drives.dedup_by(|a, b| a.mount_point == b.mount_point);
    drives
}

#[inline]
fn is_system_mount(mount_point: &std::path::Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        if let Ok(sys_drive) = std::env::var("SystemDrive") {
            let p_str = mount_point.to_string_lossy();
            if p_str.starts_with(&sys_drive) {
                return true;
            }
        }
        let s = mount_point.to_string_lossy();
        s.starts_with("C:") || s.starts_with("c:")
    }
    #[cfg(not(target_os = "windows"))]
    {
        mount_point == std::path::Path::new("/")
    }
}

pub fn read_mounts() -> Vec<PathBuf> {
    MOUNT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((last_update, mounts)) = &*cache {
            if last_update.elapsed() < Duration::from_secs(5) {
                return mounts.clone();
            }
        }

        let new_mounts = read_mounts_internal();
        *cache = Some((Instant::now(), new_mounts.clone()));
        new_mounts
    })
}

/// Linux: parse `/proc/mounts` and keep only real on-disk filesystems (skipping the
/// many virtual/pseudo mounts the kernel exposes).
#[cfg(target_os = "linux")]
fn read_mounts_internal() -> Vec<PathBuf> {
    let content = match std::fs::read_to_string("/proc/mounts") {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let real_fstypes = [
        "ext2", "ext3", "ext4", "btrfs", "xfs", "f2fs", "jfs", "reiserfs", "ntfs", "ntfs-3g",
        "vfat", "exfat", "fuseblk", "zfs",
    ];
    content
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let _dev = parts.next()?;
            let mountpoint = parts.next()?;
            let fstype = parts.next()?;
            real_fstypes
                .contains(&fstype)
                .then(|| PathBuf::from(mountpoint))
        })
        .collect()
}

/// Windows / macOS: enumerate mounted volumes via `sysinfo` (already a dependency and
/// cross-platform). On Windows these are drive roots like `C:\`; on macOS they are the
/// root `/` plus `/Volumes/*` for external disks.
#[cfg(not(target_os = "linux"))]
fn read_mounts_internal() -> Vec<PathBuf> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut mounts: Vec<PathBuf> = disks
        .list()
        .iter()
        .map(|disk| disk.mount_point().to_path_buf())
        .collect();
    mounts.sort();
    mounts.dedup();
    mounts
}
