//! Metadata-only volume identity and media capabilities.
use std::{
    ffi::OsStr,
    mem::{size_of, zeroed},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    Storage::FileSystem::{
        CreateFileW, GetLogicalDrives, GetVolumeInformationW, GetVolumePathNameW,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    },
    System::{
        Ioctl::{
            PropertyStandardQuery, StorageDeviceSeekPenaltyProperty,
            DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_PROPERTY_QUERY,
        },
        IO::DeviceIoControl,
    },
};

pub fn volume_serial(root: &Path) -> Option<u32> {
    let root = super::wide(root.as_os_str());
    let mut serial = 0;
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            null_mut(),
            0,
            &mut serial,
            null_mut(),
            null_mut(),
            null_mut(),
            0,
        )
    };
    (ok != 0).then_some(serial)
}

pub fn mounted_volume_serials() -> Vec<(PathBuf, u32)> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26)
        .filter(|bit| mask & (1 << bit) != 0)
        .filter_map(|bit| {
            let letter = char::from(b'A' + bit as u8);
            let root = PathBuf::from(format!("{letter}:\\"));
            Some((root.clone(), volume_serial(&root)?))
        })
        .collect()
}

/// `None` means an unsupported/remote device: callers should use conservative I/O.
pub fn incurs_seek_penalty(path: &Path) -> Option<bool> {
    let path = super::wide(path.as_os_str());
    let mut root = [0u16; 32768];
    if unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return None;
    }
    let root_len = root.iter().position(|unit| *unit == 0)?;
    let root = String::from_utf16_lossy(&root[..root_len]);
    // Drive-letter volumes support the zero-access storage property query.
    let bytes = root.as_bytes();
    if bytes.len() != 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        return None;
    }
    let device = super::wide(OsStr::new(&format!(r"\\.\{}:", char::from(bytes[0]))));
    let handle = unsafe {
        CreateFileW(
            device.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            0,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let handle = super::Handle(handle);
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceSeekPenaltyProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    let mut descriptor: DEVICE_SEEK_PENALTY_DESCRIPTOR = unsafe { zeroed() };
    let mut returned = 0;
    let ok = unsafe {
        DeviceIoControl(
            handle.0,
            IOCTL_STORAGE_QUERY_PROPERTY,
            (&raw const query).cast(),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            (&raw mut descriptor).cast(),
            size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            &mut returned,
            null_mut(),
        )
    };
    (ok != 0 && returned as usize >= size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>())
        .then_some(descriptor.IncursSeekPenalty != 0)
}

/// Includes drive-letter mappings visible to the current user token.
pub fn logical_drive_roots() -> Vec<(PathBuf, bool)> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26).filter(|bit| mask & (1 << bit) != 0).map(|bit| {
        let root = PathBuf::from(format!("{}:\\", char::from(b'A' + bit as u8)));
        let remote = unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(super::wide(root.as_os_str()).as_ptr()) } == 4;
        (root, remote)
    }).collect()
}
