#![cfg(windows)]

pub mod ntfs;
pub mod cleanup;
pub mod mft_space;
pub mod directory;
pub mod persistence;
pub mod crash;
pub mod storage;
pub mod shell_locations;
pub mod recycle;
pub mod integration;
pub mod metadata;
pub mod media;
pub mod pdf;
pub mod process_job;
pub mod vault_safety;
pub mod vault_driver;
pub mod data_protection;

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::AtomicBool;
use std::time::Instant;
use windows_sys::Wdk::Storage::FileSystem::{MFT_ENUM_DATA, USN_RECORD};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_HANDLE_EOF, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FileIdType, GetFileInformationByHandle, GetVolumeInformationW, OpenFileById,
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_DESCRIPTOR,
    FILE_ID_DESCRIPTOR_0, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Ioctl::FSCTL_ENUM_USN_DATA;
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_TYPENAME};

const BUFFER_SIZE: usize = 1024 * 1024;
const SNAPSHOT_MS: u128 = 250;

/// Copy one file through Windows' native CopyFile2 pipeline. This lets the OS
/// select filesystem/SMB offload paths while retaining progress and cooperative
/// cancellation. The caller owns fallback and partial-file cleanup.
pub fn copy_file2<F: FnMut(u64, u64)>(
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
    mut progress: F,
) -> std::io::Result<u64> {
    use windows_sys_copy::Win32::Storage::FileSystem::{
        CopyFile2, COPYFILE2_CALLBACK_CHUNK_FINISHED, COPYFILE2_EXTENDED_PARAMETERS,
        COPYFILE2_MESSAGE, COPYFILE2_PROGRESS_CANCEL, COPYFILE2_PROGRESS_CONTINUE,
        COPY_FILE_NO_BUFFERING, COPY_FILE_REQUEST_COMPRESSED_TRAFFIC,
    };

    struct Context<'a, F> {
        cancel: &'a AtomicBool,
        progress: &'a mut F,
    }

    unsafe extern "system" fn callback<F: FnMut(u64, u64)>(
        message: *const COPYFILE2_MESSAGE,
        raw_context: *const core::ffi::c_void,
    ) -> i32 {
        let context = unsafe { &mut *(raw_context.cast_mut().cast::<Context<'_, F>>()) };
        if context.cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return COPYFILE2_PROGRESS_CANCEL;
        }
        let message = unsafe { &*message };
        if message.Type == COPYFILE2_CALLBACK_CHUNK_FINISHED {
            let chunk = unsafe { message.Info.ChunkFinished };
            (context.progress)(chunk.uliTotalBytesTransferred, chunk.uliTotalFileSize);
        }
        COPYFILE2_PROGRESS_CONTINUE
    }

    let source_len = source.metadata()?.len();
    let mut source_wide = wide(source.as_os_str());
    let mut destination_wide = wide(destination.as_os_str());
    let mut context = Context {
        cancel,
        progress: &mut progress,
    };
    let mut flags = 0;
    if source_len >= 1024 * 1024 * 1024 {
        flags |= COPY_FILE_NO_BUFFERING;
    }
    if source.to_string_lossy().starts_with(r"\\")
        || destination.to_string_lossy().starts_with(r"\\")
    {
        flags |= COPY_FILE_REQUEST_COMPRESSED_TRAFFIC;
    }
    let parameters = COPYFILE2_EXTENDED_PARAMETERS {
        dwSize: size_of::<COPYFILE2_EXTENDED_PARAMETERS>() as u32,
        dwCopyFlags: flags,
        pfCancel: null_mut(),
        pProgressRoutine: Some(callback::<F>),
        pvCallbackContext: (&raw mut context).cast(),
    };
    let result = unsafe {
        CopyFile2(
            source_wide.as_mut_ptr(),
            destination_wide.as_mut_ptr(),
            &parameters,
        )
    };
    if result >= 0 {
        progress(source_len, source_len);
        Ok(source_len)
    } else if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "CopyFile2 cancelled",
        ))
    } else {
        Err(std::io::Error::from_raw_os_error(result & 0xFFFF))
    }
}

/// Fast free-space query for the volume containing `path`.
pub fn available_space(path: &Path) -> Option<u64> {
    use windows_sys_copy::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let existing = path.ancestors().find(|candidate| candidate.exists())?;
    let wide_path = wide(existing.as_os_str());
    let mut available = 0u64;
    let result =
        unsafe { GetDiskFreeSpaceExW(wide_path.as_ptr(), &mut available, null_mut(), null_mut()) };
    (result != 0).then_some(available)
}

/// Try an explicit same-volume copy-on-write block clone. Returns `Ok(false)`
/// when the filesystem/server does not support extent duplication, allowing a
/// transparent fallback to CopyFile2.
pub fn try_block_clone(source: &Path, destination: &Path) -> std::io::Result<bool> {
    use std::fs::{self, File};
    use std::io::{Seek, SeekFrom};
    use std::os::windows::io::AsRawHandle;
    use windows_sys_copy::Win32::Storage::FileSystem::GetDiskFreeSpaceW;
    use windows_sys_copy::Win32::System::Ioctl::{
        DUPLICATE_EXTENTS_DATA, FSCTL_DUPLICATE_EXTENTS_TO_FILE,
    };
    use windows_sys_copy::Win32::System::IO::DeviceIoControl;

    let source_len = source.metadata()?.len();
    if source_len < 16 * 1024 * 1024 {
        return Ok(false);
    }
    let existing = destination
        .ancestors()
        .find(|candidate| candidate.exists())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "destination volume"))?;
    let volume_wide = wide(existing.as_os_str());
    let mut sectors_per_cluster = 0u32;
    let mut bytes_per_sector = 0u32;
    let cluster_ok = unsafe {
        GetDiskFreeSpaceW(
            volume_wide.as_ptr(),
            &mut sectors_per_cluster,
            &mut bytes_per_sector,
            null_mut(),
            null_mut(),
        )
    };
    if cluster_ok == 0 {
        return Ok(false);
    }
    let cluster_size = u64::from(sectors_per_cluster) * u64::from(bytes_per_sector);
    let clone_bytes = source_len / cluster_size * cluster_size;
    if clone_bytes == 0 {
        return Ok(false);
    }

    let mut source_file = File::open(source)?;
    let mut destination_file = File::create(destination)?;
    destination_file.set_len(source_len)?;
    let data = DUPLICATE_EXTENTS_DATA {
        FileHandle: source_file.as_raw_handle(),
        SourceFileOffset: 0,
        TargetFileOffset: 0,
        ByteCount: clone_bytes as i64,
    };
    let mut returned = 0u32;
    let cloned = unsafe {
        DeviceIoControl(
            destination_file.as_raw_handle(),
            FSCTL_DUPLICATE_EXTENTS_TO_FILE,
            (&raw const data).cast(),
            size_of::<DUPLICATE_EXTENTS_DATA>() as u32,
            null_mut(),
            0,
            &mut returned,
            null_mut(),
        )
    };
    if cloned == 0 {
        drop(destination_file);
        let _ = fs::remove_file(destination);
        return Ok(false);
    }

    if clone_bytes < source_len {
        source_file.seek(SeekFrom::Start(clone_bytes))?;
        destination_file.seek(SeekFrom::Start(clone_bytes))?;
        std::io::copy(&mut source_file, &mut destination_file)?;
    }
    destination_file.sync_data()?;
    fs::set_permissions(destination, source.metadata()?.permissions())?;
    Ok(true)
}

#[derive(Clone, Debug)]
pub struct WinSpaceItem {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

pub enum ScanError {
    Unsupported,
    Win32,
    Cancelled,
}

pub fn type_name_for_path(path: &Path) -> Option<String> {
    let wide = wide(path.as_os_str());
    let mut info = unsafe { zeroed::<SHFILEINFOW>() };
    let ok = unsafe {
        SHGetFileInfoW(
            wide.as_ptr(),
            0,
            &mut info,
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_TYPENAME,
        )
    };
    if ok == 0 {
        return None;
    }
    let len = info
        .szTypeName
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(info.szTypeName.len());
    (len > 0).then(|| String::from_utf16_lossy(&info.szTypeName[..len]))
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[derive(Clone)]
struct Node {
    parent: u64,
    name: String,
    is_dir: bool,
}

pub fn scan_ntfs<F, S>(root: &Path, cancelled: F, snapshot: S) -> Result<(), ScanError>
where
    F: FnMut() -> bool,
    S: FnMut(Vec<WinSpaceItem>, u64, u64, u64, bool) -> bool,
{
    scan_ntfs_with_files(root, cancelled, snapshot, |_| true, |_, _| {})
}

pub fn scan_ntfs_with_files<F, S, I, E>(root: &Path, mut cancelled: F, mut snapshot: S, include: I, mut file: E) -> Result<(), ScanError>
where F: FnMut() -> bool,
    S: FnMut(Vec<WinSpaceItem>, u64, u64, u64, bool) -> bool,
    I: Fn(&Path) -> bool,
    E: FnMut(PathBuf, u64),
{
    let volume = volume_letter(root).ok_or(ScanError::Unsupported)?;
    if !is_ntfs_root(volume) {
        return Err(ScanError::Unsupported);
    }
    let target_id = file_id(root).ok_or(ScanError::Win32)?;
    let volume_handle = open_volume(volume).ok_or(ScanError::Win32)?;
    let nodes = enum_usn_nodes(volume_handle.0, &mut cancelled, &mut snapshot)?;
    let mut top_cache = HashMap::<u64, Option<u64>>::new();
    let mut items = HashMap::<u64, WinSpaceItem>::new();
    let mut scanned_bytes = 0_u64;
    let mut scanned_entries = 0_u64;
    let mut error_count = 0_u64;
    let mut last_snapshot = Instant::now();
    let mut directory_paths = HashMap::from([(target_id, root.to_path_buf())]);

    for (&id, node) in &nodes {
        if cancelled() {
            return Err(ScanError::Cancelled);
        }
        let Some(top_id) = top_child(id, target_id, &nodes, &mut top_cache) else {
            continue;
        };
        let Some(top) = nodes.get(&top_id) else {
            continue;
        };
        let Some(path) = node_path(id, target_id, &nodes, &mut directory_paths) else { continue; };
        if !include(&path) { continue; }
        let item = items.entry(top_id).or_insert_with(|| WinSpaceItem {
            path: root.join(&top.name),
            name: top.name.clone(),
            is_dir: top.is_dir,
            size: 0,
        });
        if !node.is_dir {
            scanned_entries += 1;
            match file_size_by_id(volume_handle.0, id) {
                Some(size) => {
                    file(path, size);
                    scanned_bytes = scanned_bytes.saturating_add(size);
                    item.size = item.size.saturating_add(size);
                }
                None => error_count += 1,
            }
        }
        if last_snapshot.elapsed().as_millis() >= SNAPSHOT_MS {
            if !snapshot(
                items.values().cloned().collect(),
                scanned_bytes,
                scanned_entries,
                error_count,
                false,
            ) {
                return Err(ScanError::Cancelled);
            }
            last_snapshot = Instant::now();
        }
    }

    let _ = snapshot(
        items.values().cloned().collect(),
        scanned_bytes,
        scanned_entries,
        error_count,
        true,
    );
    Ok(())
}

fn enum_usn_nodes<F, S>(
    volume: HANDLE,
    cancelled: &mut F,
    snapshot: &mut S,
) -> Result<HashMap<u64, Node>, ScanError>
where
    F: FnMut() -> bool,
    S: FnMut(Vec<WinSpaceItem>, u64, u64, u64, bool) -> bool,
{
    let mut data = MFT_ENUM_DATA {
        StartFileReferenceNumber: 0,
        LowUsn: 0,
        HighUsn: i64::MAX,
        MinMajorVersion: 2,
        MaxMajorVersion: 2,
    };
    let mut buffer = vec![0_u8; BUFFER_SIZE];
    let mut nodes = HashMap::new();
    let mut scanned_entries = 0_u64;
    let mut last_snapshot = Instant::now();

    loop {
        if cancelled() {
            return Err(ScanError::Cancelled);
        }
        let mut returned = 0_u32;
        let ok = unsafe {
            DeviceIoControl(
                volume,
                FSCTL_ENUM_USN_DATA,
                (&data as *const MFT_ENUM_DATA).cast(),
                size_of::<MFT_ENUM_DATA>() as u32,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut returned,
                null_mut(),
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            return if err == ERROR_HANDLE_EOF {
                Ok(nodes)
            } else {
                Err(ScanError::Win32)
            };
        }
        if returned as usize <= size_of::<u64>() {
            return Ok(nodes);
        }
        data.StartFileReferenceNumber = u64::from_ne_bytes(
            buffer[..size_of::<u64>()]
                .try_into()
                .map_err(|_| ScanError::Win32)?,
        );
        let mut offset = size_of::<u64>();
        while offset + size_of::<USN_RECORD>() <= returned as usize {
            let record = unsafe { &*(buffer.as_ptr().add(offset).cast::<USN_RECORD>()) };
            if record.RecordLength == 0 {
                break;
            }
            let name_start = offset + record.FileNameOffset as usize;
            let name_len = record.FileNameLength as usize / 2;
            if name_start + name_len * 2 <= returned as usize {
                let name_ptr = unsafe { buffer.as_ptr().add(name_start).cast::<u16>() };
                let name = String::from_utf16_lossy(unsafe {
                    std::slice::from_raw_parts(name_ptr, name_len)
                });
                let is_dir = record.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
                let is_reparse = record.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
                if !is_reparse && !directory::attributes_require_hydration(record.FileAttributes) {
                    nodes.insert(
                        record.FileReferenceNumber,
                        Node {
                            parent: record.ParentFileReferenceNumber,
                            name,
                            is_dir,
                        },
                    );
                    scanned_entries += 1;
                }
            }
            offset += record.RecordLength as usize;
        }
        if last_snapshot.elapsed().as_millis() >= SNAPSHOT_MS {
            let _ = snapshot(Vec::new(), 0, scanned_entries, 0, false);
            last_snapshot = Instant::now();
        }
    }
}

fn top_child(
    id: u64,
    target: u64,
    nodes: &HashMap<u64, Node>,
    cache: &mut HashMap<u64, Option<u64>>,
) -> Option<u64> {
    if let Some(cached) = cache.get(&id) {
        return *cached;
    }
    if id == target {
        cache.insert(id, None);
        return None;
    }
    let mut cur = id;
    let mut seen = HashSet::new();
    loop {
        let node = nodes.get(&cur)?;
        if node.parent == target {
            cache.insert(id, Some(cur));
            return Some(cur);
        }
        if node.parent == cur || !seen.insert(cur) {
            cache.insert(id, None);
            return None;
        }
        cur = node.parent;
    }
}

fn node_path(id: u64, root: u64, nodes: &HashMap<u64, Node>, cache: &mut HashMap<u64, PathBuf>) -> Option<PathBuf> {
    if let Some(path) = cache.get(&id) { return Some(path.clone()); }
    let mut chain = Vec::new();
    let mut current = id;
    let mut seen = HashSet::new();
    let mut path = loop {
        if let Some(path) = cache.get(&current) { break path.clone(); }
        if current == root || !seen.insert(current) { return None; }
        let node = nodes.get(&current)?;
        chain.push((current, node));
        current = node.parent;
    };
    for (id, node) in chain.into_iter().rev() {
        path.push(&node.name);
        if node.is_dir { cache.insert(id, path.clone()); }
    }
    Some(path)
}

fn file_size_by_id(volume: HANDLE, id: u64) -> Option<u64> {
    let descriptor = FILE_ID_DESCRIPTOR {
        dwSize: size_of::<FILE_ID_DESCRIPTOR>() as u32,
        Type: FileIdType,
        Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: id as i64 },
    };
    let handle = unsafe {
        OpenFileById(
            volume,
            &descriptor,
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let handle = Handle(handle);
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    let ok = unsafe { GetFileInformationByHandle(handle.0, &mut info) };
    (ok != 0).then(|| (u64::from(info.nFileSizeHigh) << 32) | u64::from(info.nFileSizeLow))
}

fn file_id(path: &Path) -> Option<u64> {
    let handle = open_path(path)?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    let ok = unsafe { GetFileInformationByHandle(handle.0, &mut info) };
    (ok != 0).then(|| (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
}

fn open_path(path: &Path) -> Option<Handle> {
    let wide = wide(path.as_os_str());
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            0,
        )
    };
    (handle != INVALID_HANDLE_VALUE).then_some(Handle(handle))
}

fn open_volume(letter: char) -> Option<Handle> {
    let path = format!(r"\\.\{}:", letter.to_ascii_uppercase());
    let wide = wide(OsStr::new(&path));
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            0,
        )
    };
    (handle != INVALID_HANDLE_VALUE).then_some(Handle(handle))
}

fn is_ntfs_root(letter: char) -> bool {
    let root = format!("{}:\\", letter.to_ascii_uppercase());
    let root = wide(OsStr::new(&root));
    let mut fs_name = [0_u16; 16];
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    ok != 0 && String::from_utf16_lossy(&fs_name).trim_end_matches('\0') == "NTFS"
}

fn volume_letter(path: &Path) -> Option<char> {
    let s = path.as_os_str().to_string_lossy();
    let bytes = s.as_bytes();
    (bytes.len() >= 2 && bytes[1] == b':')
        .then(|| bytes[0] as char)
        .filter(|c| c.is_ascii_alphabetic())
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

pub fn call_named_pipe(name: &OsStr, input: &[u8], max_output_len: u32) -> std::io::Result<Vec<u8>> {
    use windows_sys::Win32::System::Pipes::CallNamedPipeW;
    let wide_name = wide(name);
    let mut output = vec![0u8; max_output_len as usize];
    let mut bytes_read = 0u32;
    let ok = unsafe {
        CallNamedPipeW(
            wide_name.as_ptr(),
            input.as_ptr().cast(),
            input.len() as u32,
            output.as_mut_ptr().cast(),
            output.len() as u32,
            &mut bytes_read,
            3000,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        output.truncate(bytes_read as usize);
        Ok(output)
    }
}
