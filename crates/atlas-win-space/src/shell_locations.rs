//! Read-only Windows shell namespace discovery. Call on a blocking worker.
use std::path::PathBuf;
use windows::{core::HSTRING, Win32::{System::{Com::{CoInitializeEx, CoUninitialize, CoTaskMemFree, COINIT_APARTMENTTHREADED}, SystemServices::{SFGAO_FOLDER, SFGAO_FILESYSTEM}}, UI::Shell::{SHCreateItemFromParsingName, IShellItem, IEnumShellItems, BHID_EnumItems, SIGDN_NORMALDISPLAY, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH}}};
#[derive(Debug, Clone)]
pub struct ShellLocation { pub name: String, pub path: PathBuf, pub folder: bool, pub filesystem: bool }
struct Apartment(bool);
impl Drop for Apartment { fn drop(&mut self) { if self.0 { unsafe { CoUninitialize(); } } } }
unsafe fn text(item: &IShellItem, kind: windows::Win32::UI::Shell::SIGDN) -> windows::core::Result<String> {
    let value = item.GetDisplayName(kind)?;
    let result = value.to_string();
    CoTaskMemFree(Some(value.0.cast()));
    Ok(result?)
}
pub fn enumerate(path: &str) -> Result<Vec<ShellLocation>, String> {
    let path = path.to_string();
    std::thread::Builder::new().name("atlas-shell-discovery".into()).spawn(move || enumerate_sta(&path))
        .map_err(|e| e.to_string())?.join().map_err(|_| "Shell discovery worker panicked".to_string())?
}
fn enumerate_sta(path: &str) -> Result<Vec<ShellLocation>, String> {
    let run = || -> windows::core::Result<Vec<ShellLocation>> { unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        init.ok()?;
        let _apartment = Apartment(true);
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None)?;
        let items: IEnumShellItems = item.BindToHandler(None, &BHID_EnumItems)?;
        let mut out = Vec::new();
        loop {
            let mut next = [None];
            items.Next(&mut next, None)?;
            let Some(child) = next[0].take() else { break };
            let Ok(attributes) = child.GetAttributes(SFGAO_FOLDER | SFGAO_FILESYSTEM) else { continue };
            let filesystem = attributes.contains(SFGAO_FILESYSTEM);
            let Ok(path) = text(&child, if filesystem { SIGDN_FILESYSPATH } else { SIGDN_DESKTOPABSOLUTEPARSING }) else { continue };
            let name = text(&child, SIGDN_NORMALDISPLAY).unwrap_or_else(|_| path.clone());
            out.push(ShellLocation { name, path: PathBuf::from(path), folder: attributes.contains(SFGAO_FOLDER), filesystem });
            if out.len() >= 10_000 { break; }
        }
        Ok(out)
    }};
    run().map_err(|e| e.to_string())
}
/// Provider entries registered on the virtual Desktop, including organization groups.
pub fn sync_roots() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    for item in enumerate("::{00021400-0000-0000-C000-000000000046}").unwrap_or_default() {
        if !item.folder { continue; }
        let parsing = item.path.to_string_lossy();
        if parsing.contains("20D04FE0") || parsing.contains("F02C1A0D") || parsing.contains("645FF040") || parsing.contains("26EE0668") { continue; }
        if item.filesystem {
            if home.as_ref().is_some_and(|home| item.path == *home || item.path.starts_with(home.join("Desktop"))) { continue; }
            out.push((item.name, item.path));
        } else {
            for child in enumerate(&parsing).unwrap_or_default().into_iter().filter(|c| c.filesystem && c.folder) {
                out.push((format!("{} / {}", item.name, child.name), child.path));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn this_pc_enumerates_the_system_drive_as_a_filesystem_folder() {
        let root = format!("{}\\", std::env::var("SystemDrive").unwrap());
        let entries = enumerate("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}").unwrap();
        assert!(entries.iter().any(|entry| entry.filesystem && entry.folder && entry.path.to_string_lossy().eq_ignore_ascii_case(&root)), "{entries:?}");
    }
}
