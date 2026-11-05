use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

const WSL_LOCALHOST: &str = r"\\wsl.localhost\";
const WSL_CACHE_TTL: Duration = Duration::from_secs(5);
const WSL_READY_TTL: Duration = Duration::from_secs(30);

fn wsl_tail(path: &Path) -> Option<&str> {
    let s = path.to_str()?;
    let lower = s.to_ascii_lowercase();
    for prefix in [
        r"\\wsl$\",
        WSL_LOCALHOST,
        r"\\?\unc\wsl$\",
        r"\\?\unc\wsl.localhost\",
    ] {
        if lower.starts_with(prefix) {
            return Some(&s[prefix.len()..]);
        }
    }
    None
}

pub fn wsl_unc_parts(path: &Path) -> Option<(String, String)> {
    let tail = wsl_tail(path)?;
    let mut parts = tail.split(['\\', '/']);
    let distro = parts.next()?.to_string();
    if distro.is_empty() {
        return None;
    }
    let linux_path = format!("/{}", parts.collect::<Vec<_>>().join("/"));
    Some((distro, linux_path.trim_end_matches('/').to_string()))
}

pub fn normalize_wsl_unc_path(path: PathBuf) -> PathBuf {
    wsl_tail(&path).map_or(path.clone(), |tail| {
        PathBuf::from(format!("{WSL_LOCALHOST}{tail}"))
    })
}

pub fn normalize_wsl_unc_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.into_iter().map(normalize_wsl_unc_path).collect()
}

pub fn normalize_wsl_location_path(location: &mut atlas_model::location::Location) {
    match location {
        atlas_model::location::Location::Directory(path) => {
            *path = normalize_wsl_unc_path(std::mem::take(path));
        }
        atlas_model::location::Location::Archive { archive, .. } => {
            *archive = normalize_wsl_unc_path(std::mem::take(archive));
        }
        atlas_model::location::Location::Tag(_) | atlas_model::location::Location::Shell(_) | atlas_model::location::Location::RecycleBin => {}
        #[cfg(feature = "provider-host")]
        atlas_model::location::Location::Remote { .. } => {}
    }
}

pub fn wsl_distro_name(path: &Path) -> Option<String> {
    wsl_tail(path)?
        .split(['\\', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn is_user_wsl_distro(name: &str) -> bool {
    !name.to_ascii_lowercase().starts_with("docker-desktop")
}

use std::sync::{Arc, Mutex, OnceLock};

static WSL_ROOT_CACHE: OnceLock<Arc<Mutex<Option<(Instant, Vec<PathBuf>, bool)>>>> =
    OnceLock::new();

#[cfg(windows)]
pub fn list_wsl_roots() -> Vec<PathBuf> {
    let cache_arc = WSL_ROOT_CACHE
        .get_or_init(|| Arc::new(Mutex::new(None)))
        .clone();
    let mut cache = cache_arc.lock().unwrap();

    if let Some((last_update, roots, is_updating)) = &mut *cache {
        if last_update.elapsed() < WSL_CACHE_TTL {
            return roots.clone();
        }
        if !*is_updating {
            *is_updating = true;
            let cache_clone = cache_arc.clone();
            std::thread::spawn(move || {
                let new_roots = list_wsl_roots_unc()
                    .into_iter()
                    .chain(list_wsl_roots_command())
                    .fold(Vec::new(), |mut roots, path| {
                        if !roots.contains(&path) {
                            roots.push(path);
                        }
                        roots
                    });
                if let Ok(mut c) = cache_clone.lock() {
                    *c = Some((Instant::now(), new_roots, false));
                }
            });
        }
        return roots.clone();
    }

    let roots = list_wsl_roots_unc()
        .into_iter()
        .chain(list_wsl_roots_command())
        .fold(Vec::new(), |mut roots, path| {
            if !roots.contains(&path) {
                roots.push(path);
            }
            roots
        });
    *cache = Some((Instant::now(), roots.clone(), false));
    roots
}

#[cfg(windows)]
fn list_wsl_roots_unc() -> Vec<PathBuf> {
    std::fs::read_dir(WSL_LOCALHOST)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(std::fs::FileType::is_dir)
                .map(|_| normalize_wsl_unc_path(entry.path()))
                .filter(|path| wsl_distro_name(path).is_some_and(|name| is_user_wsl_distro(&name)))
        })
        .collect()
}

#[cfg(windows)]
fn list_wsl_roots_command() -> Vec<PathBuf> {
    use std::os::windows::process::CommandExt;

    let Ok(output) = std::process::Command::new("wsl.exe")
        .args(["--list", "--quiet"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
    else {
        return Vec::new();
    };
    parse_wsl_list_output(&output.stdout)
        .into_iter()
        .filter(|name| is_user_wsl_distro(name))
        .map(|name| PathBuf::from(format!("{WSL_LOCALHOST}{name}")))
        .collect()
}

#[cfg(not(windows))]
pub fn list_wsl_roots() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(windows)]
pub fn ensure_wsl_ready(path: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::collections::HashMap;
    use std::os::windows::process::CommandExt;
    use std::sync::{Mutex, OnceLock};

    let Some(distro) = wsl_distro_name(path) else {
        return Ok(());
    };
    static READY_CACHE: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    let cache = READY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if cache
        .lock()
        .ok()
        .and_then(|ready| ready.get(&distro.to_ascii_lowercase()).copied())
        .is_some_and(|last_ready| last_ready.elapsed() < WSL_READY_TTL)
    {
        return Ok(());
    }
    let status = std::process::Command::new("wsl.exe")
        .args(["-d", &distro, "--", "true"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .status()
        .with_context(|| format!("failed to start WSL distro {distro}"))?;
    if !status.success() {
        anyhow::bail!("WSL distro {distro} is not available");
    }
    if let Ok(mut ready) = cache.lock() {
        ready.insert(distro.to_ascii_lowercase(), Instant::now());
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn ensure_wsl_ready(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn parse_wsl_list_output(bytes: &[u8]) -> Vec<String> {
    let text = if bytes.len() >= 2 && bytes[1] == 0 {
        let words = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        String::from_utf16_lossy(&words)
    } else {
        String::from_utf8_lossy(bytes).replace('\0', "")
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub fn volume_root(path: &Path) -> Option<String> {
    if let Some(distro) = wsl_distro_name(path) {
        return Some(format!("wsl:{}", distro.to_ascii_lowercase()));
    }
    match path.components().next() {
        Some(Component::Prefix(p)) => Some(p.as_os_str().to_string_lossy().to_ascii_lowercase()),
        Some(Component::RootDir) => Some("/".to_string()),
        _ => None,
    }
}

#[cfg(unix)]
fn path_device(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let mut current = Some(path);
    while let Some(p) = current {
        let candidate = if p.as_os_str().is_empty() {
            Path::new(".")
        } else {
            p
        };
        if let Ok(meta) = std::fs::metadata(candidate) {
            return Some(meta.dev());
        }
        current = p.parent();
    }
    None
}

#[cfg(unix)]
pub fn same_volume(a: &Path, b: &Path) -> bool {
    if let (Some(da), Some(db)) = (wsl_distro_name(a), wsl_distro_name(b)) {
        return da.eq_ignore_ascii_case(&db);
    }
    if wsl_distro_name(a).is_some() || wsl_distro_name(b).is_some() {
        return false;
    }
    let s_a = a.to_string_lossy();
    let s_b = b.to_string_lossy();
    let da = s_a.get(..2).filter(|s| s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1] == b':').map(|s| s.to_ascii_uppercase());
    let db = s_b.get(..2).filter(|s| s.as_bytes()[0].is_ascii_alphabetic() && s.as_bytes()[1] == b':').map(|s| s.to_ascii_uppercase());
    if let (Some(da), Some(db)) = (da, db) {
        if da != db {
            return false;
        }
    }
    match (path_device(a), path_device(b)) {
        (Some(dev_a), Some(dev_b)) => dev_a == dev_b,
        _ => false,
    }
}

#[cfg(not(unix))]
pub fn same_volume(a: &Path, b: &Path) -> bool {
    match (volume_root(a), volume_root(b)) {
        (Some(va), Some(vb)) => va == vb,
        _ => false,
    }
}

/// True when `path` lives inside a Windows Recycle Bin folder (`$Recycle.Bin`).
/// Used to reject transfer *destinations* that would drop payloads next to the
/// `$R`/`$I` pairs and corrupt the bin's bookkeeping.
pub fn is_recycle_bin_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.split(['/', '\\'])
        .any(|part| part.eq_ignore_ascii_case("$recycle.bin"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recycle_bin_paths_detected() {
        assert!(is_recycle_bin_path(Path::new(
            r"C:\$Recycle.Bin\S-1-5-21-x\$R1A2B3C\docs"
        )));
        assert!(is_recycle_bin_path(Path::new(r"D:\$RECYCLE.BIN\S-1\$Rxyz")));
        assert!(!is_recycle_bin_path(Path::new(r"C:\Users\me\Recycle.Bin")));
    }

    #[test]
    fn normalizes_wsl_dollar_to_localhost() {
        assert_eq!(
            normalize_wsl_unc_path(PathBuf::from(r"\\wsl$\Ubuntu\home\a")),
            PathBuf::from(r"\\wsl.localhost\Ubuntu\home\a")
        );
    }

    #[test]
    fn keeps_wsl_localhost_stable() {
        let path = PathBuf::from(r"\\wsl.localhost\Ubuntu\home\a");
        assert_eq!(normalize_wsl_unc_path(path.clone()), path);
    }

    #[test]
    fn parses_wsl_unc_parts() {
        assert_eq!(
            wsl_unc_parts(Path::new(r"\\wsl$\Ubuntu\home\a")),
            Some(("Ubuntu".to_string(), "/home/a".to_string()))
        );
        assert_eq!(
            wsl_unc_parts(Path::new(r"\\?\UNC\wsl.localhost\Debian\tmp")),
            Some(("Debian".to_string(), "/tmp".to_string()))
        );
        assert_eq!(wsl_unc_parts(Path::new(r"C:\tmp")), None);
    }

    #[test]
    fn normalizes_location_paths() {
        let mut location =
            atlas_model::location::Location::Directory(PathBuf::from(r"\\wsl$\Ubuntu\home\a"));
        normalize_wsl_location_path(&mut location);
        assert_eq!(
            location,
            atlas_model::location::Location::Directory(PathBuf::from(
                r"\\wsl.localhost\Ubuntu\home\a"
            ))
        );
    }

    #[test]
    fn wsl_volume_is_the_distro() {
        assert!(same_volume(
            Path::new(r"\\wsl$\Ubuntu\home\a"),
            Path::new(r"\\wsl.localhost\Ubuntu\tmp\b"),
        ));
        assert!(!same_volume(
            Path::new(r"\\wsl$\Ubuntu\home\a"),
            Path::new(r"\\wsl.localhost\Debian\home\a"),
        ));
    }

    #[test]
    fn parses_utf16_wsl_list_output() {
        let bytes = "Arch\r\ndocker-desktop\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(
            parse_wsl_list_output(&bytes),
            vec!["Arch".to_string(), "docker-desktop".to_string()]
        );
    }

    #[test]
    fn extracts_wsl_distro_name() {
        assert_eq!(
            wsl_distro_name(Path::new(r"\\wsl.localhost\Arch")),
            Some("Arch".to_string())
        );
    }

    #[test]
    fn filters_docker_internal_distros() {
        assert!(is_user_wsl_distro("Arch"));
        assert!(!is_user_wsl_distro("docker-desktop"));
        assert!(!is_user_wsl_distro("docker-desktop-data"));
    }

    #[cfg(unix)]
    #[test]
    fn unix_same_volume_checks() {
        let tmp_a = std::env::temp_dir().join("atlas_test_nonexistent_a");
        let tmp_b = std::env::temp_dir().join("atlas_test_nonexistent_b");
        assert!(same_volume(&tmp_a, &tmp_b));
        assert!(!same_volume(Path::new("/tmp"), Path::new("/dev")));
    }
}
