use std::{
    ffi::{CStr, CString, OsString},
    fs, io,
    mem::MaybeUninit,
    os::unix::{ffi::OsStrExt, ffi::OsStringExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq)]
pub struct DiskUsage {
    pub mount_path: PathBuf,
    pub source: OsString,
    pub total_bytes: u64,

    pub used_bytes: u64,

    pub available_bytes: u64,

    pub usage_ratio: f64,
}

#[derive(Debug)]
struct Mount {
    id: u64,
    device: (u32, u32),
    path: PathBuf,
    fs_type: String,
    source: OsString,
}

pub fn disk_usage(path: &Path) -> io::Result<DiskUsage> {
    let path = fs::canonicalize(path)?;
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let mut stats = MaybeUninit::<libc::statvfs>::uninit();

    if unsafe { libc::statvfs(c_path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }

    let stats = unsafe { stats.assume_init() };
    let block_bytes = if stats.f_frsize != 0 {
        stats.f_frsize
    } else {
        stats.f_bsize
    };

    #[allow(clippy::unnecessary_cast)]
    let (total_bytes, used_bytes, available_bytes, usage_ratio) = capacity(
        stats.f_blocks as u64,
        stats.f_bfree as u64,
        stats.f_bavail as u64,
        block_bytes as u64,
    )?;

    let device = fs::metadata(&path)?.dev();
    let device = (libc::major(device), libc::minor(device));
    let mountinfo = fs::read("/proc/self/mountinfo")?;
    let mounts = parse_mountinfo(&mountinfo);
    let mount = containing_mount(&mounts, &path, mount_id(&c_path), device).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "filesystem mount is absent from /proc/self/mountinfo",
        )
    })?;

    Ok(DiskUsage {
        mount_path: mount.path.clone(),
        source: mount.source.clone(),
        total_bytes,
        used_bytes,
        available_bytes,
        usage_ratio,
    })
}

fn capacity(
    blocks: u64,
    free_blocks: u64,
    available_blocks: u64,
    block_bytes: u64,
) -> io::Result<(u64, u64, u64, f64)> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid filesystem capacity");
    if blocks == 0 || block_bytes == 0 || free_blocks > blocks || available_blocks > free_blocks {
        return Err(invalid());
    }
    let total = blocks.checked_mul(block_bytes).ok_or_else(invalid)?;
    let used = (blocks - free_blocks)
        .checked_mul(block_bytes)
        .ok_or_else(invalid)?;
    let available = available_blocks
        .checked_mul(block_bytes)
        .ok_or_else(invalid)?;
    let ratio = (total - available) as f64 / total as f64;
    Ok((total, used, available, ratio))
}

fn parse_mountinfo(contents: &[u8]) -> Vec<Mount> {
    contents
        .split(|byte| *byte == b'\n')
        .filter_map(parse_mount)
        .collect()
}

fn parse_mount(line: &[u8]) -> Option<Mount> {
    let fields: Vec<_> = line
        .split(|byte| *byte == b' ')
        .filter(|field| !field.is_empty())
        .collect();
    let separator = fields.iter().position(|field| *field == b"-")?;

    if separator < 6 || fields.len() < separator + 4 {
        return None;
    }
    let id = std::str::from_utf8(fields[0]).ok()?.parse().ok()?;
    let mut device = fields[2].split(|byte| *byte == b':');
    let major = std::str::from_utf8(device.next()?).ok()?.parse().ok()?;
    let minor = std::str::from_utf8(device.next()?).ok()?.parse().ok()?;
    if device.next().is_some() {
        return None;
    }
    let path = PathBuf::from(OsString::from_vec(unescape(fields[4])?));
    if !path.is_absolute() {
        return None;
    }
    let fs_type = String::from_utf8_lossy(fields[separator + 1]).into_owned();
    let source = OsString::from_vec(unescape(fields[separator + 2])?);
    Some(Mount {
        id,
        device: (major, minor),
        path,
        fs_type,
        source,
    })
}

pub fn mounted_disks() -> Vec<DiskUsage> {
    let Ok(mountinfo) = fs::read("/proc/self/mountinfo") else {
        return Vec::new();
    };
    let mounts = filter_unique_user_mounts(parse_mountinfo(&mountinfo));
    let mut disks = Vec::new();

    for mount in &mounts {
        let Ok(c_path) = CString::new(mount.path.as_os_str().as_bytes()) else {
            continue;
        };
        let mut stats = MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(c_path.as_ptr(), stats.as_mut_ptr()) } != 0 {
            continue;
        }
        let stats = unsafe { stats.assume_init() };
        let block_bytes = if stats.f_frsize != 0 {
            stats.f_frsize
        } else {
            stats.f_bsize
        };
        #[allow(clippy::unnecessary_cast)]
        let Ok((total_bytes, used_bytes, available_bytes, usage_ratio)) = capacity(
            stats.f_blocks as u64,
            stats.f_bfree as u64,
            stats.f_bavail as u64,
            block_bytes as u64,
        ) else {
            continue;
        };
        disks.push(DiskUsage {
            mount_path: mount.path.clone(),
            source: mount.source.clone(),
            total_bytes,
            used_bytes,
            available_bytes,
            usage_ratio,
        });
    }
    disks
}

fn filter_unique_user_mounts(mounts: Vec<Mount>) -> Vec<Mount> {
    let mut candidates: Vec<Mount> = mounts.into_iter().filter(is_user_mount).collect();
    candidates.sort_by(|a, b| {
        let rank = |p: &Path| {
            if p == Path::new("/") {
                0
            } else if p.starts_with("/run/media") || p.starts_with("/media") {
                1
            } else if p.starts_with("/mnt") {
                2
            } else {
                3
            }
        };
        rank(&a.path)
            .cmp(&rank(&b.path))
            .then_with(|| a.path.cmp(&b.path))
    });
    let mut seen_mounts = std::collections::HashSet::new();
    let mut seen_devices = std::collections::HashSet::new();
    let mut seen_sources = std::collections::HashSet::new();
    let mut out = Vec::new();
    for mount in candidates {
        if !seen_mounts.insert(mount.path.clone()) {
            continue;
        }
        if !seen_devices.insert(mount.device) {
            continue;
        }
        if mount.source.as_bytes().starts_with(b"/dev/")
            && !seen_sources.insert(mount.source.clone())
        {
            continue;
        }
        out.push(mount);
    }
    out
}

fn is_user_mount(mount: &Mount) -> bool {
    let path = &mount.path;
    if path == Path::new("/") {
        return true;
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::Normal(s) if s.as_bytes().starts_with(b".")))
    {
        return false;
    }
    if path.starts_with("/run") && !path.starts_with("/run/media") {
        return false;
    }
    for prefix in [
        "/proc", "/sys", "/dev", "/var", "/boot", "/usr", "/opt", "/srv", "/etc", "/tmp",
        "/root", "/snap",
    ] {
        if path.starts_with(prefix) {
            return false;
        }
    }
    if path.starts_with("/media") || path.starts_with("/run/media") || path.starts_with("/mnt") {
        return !matches!(
            mount.fs_type.as_str(),
            "autofs" | "proc" | "sysfs" | "devtmpfs" | "devpts" | "tmpfs"
        );
    }
    let fs = mount.fs_type.as_str();
    matches!(
        fs,
        "ext2"
            | "ext3"
            | "ext4"
            | "btrfs"
            | "xfs"
            | "f2fs"
            | "vfat"
            | "exfat"
            | "ntfs"
            | "ntfs3"
            | "fuseblk"
            | "zfs"
            | "nfs"
            | "nfs4"
            | "cifs"
            | "smb3"
    )
}

fn unescape(field: &[u8]) -> Option<Vec<u8>> {
    let mut decoded = Vec::with_capacity(field.len());
    let mut cursor = 0;
    while cursor < field.len() {
        let byte = field[cursor];
        if byte != b'\\' {
            decoded.push(byte);
            cursor += 1;
            continue;
        }
        let octal = field.get(cursor + 1..cursor + 4)?;
        if !octal.iter().all(|byte| (b'0'..=b'7').contains(byte)) {
            return None;
        }
        let value =
            (octal[0] - b'0') as u16 * 64 + (octal[1] - b'0') as u16 * 8 + (octal[2] - b'0') as u16;
        decoded.push(u8::try_from(value).ok()?);
        cursor += 4;
    }
    Some(decoded)
}

fn mount_id(path: &CStr) -> Option<u64> {
    let mut stats = MaybeUninit::<libc::statx>::zeroed();

    let result = unsafe {
        libc::statx(
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::AT_STATX_DONT_SYNC | libc::AT_NO_AUTOMOUNT,
            libc::STATX_MNT_ID,
            stats.as_mut_ptr(),
        )
    };
    if result != 0 {
        return None;
    }

    let stats = unsafe { stats.assume_init() };
    (stats.stx_mask & libc::STATX_MNT_ID != 0).then_some(stats.stx_mnt_id)
}

fn containing_mount<'a>(
    mounts: &'a [Mount],
    path: &Path,
    mount_id: Option<u64>,
    device: (u32, u32),
) -> Option<&'a Mount> {
    if let Some(id) = mount_id {
        if let Some(mount) = mounts
            .iter()
            .find(|mount| mount.id == id && path.starts_with(&mount.path))
        {
            return Some(mount);
        }
    }

    mounts
        .iter()
        .filter(|mount| path.starts_with(&mount.path))
        .max_by_key(|mount| {
            (
                mount.path.as_os_str().as_bytes().len(),
                mount.device == device,
                mount.id,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_parser_retains_escaped_and_non_utf8_paths() {
        let mounts = parse_mountinfo(
            b"20 1 8:1 / / rw - ext4 /dev/sda1 rw\n\
              21 20 8:2 / /media/My\\040disk\\134name rw shared:1 - ext4 /dev/disk\\040two rw\n\
              22 20 8:3 / /media/\xff rw - ext4 /dev/\xfe rw\n",
        );
        assert_eq!(mounts.len(), 3);
        assert_eq!(mounts[1].path, Path::new("/media/My disk\\name"));
        assert_eq!(mounts[1].source, "/dev/disk two");
        assert_eq!(mounts[2].path.as_os_str().as_bytes(), b"/media/\xff");
        assert_eq!(mounts[2].source.as_bytes(), b"/dev/\xfe");
        assert!(parse_mount(b"23 1 8:1 / /bad\\999 rw - ext4 /dev/sda1 rw").is_none());
        assert!(parse_mount(b"24 1 8:1 / relative rw - ext4 /dev/sda1 rw").is_none());
    }

    #[test]
    fn containing_mount_uses_mount_id_then_directory_boundaries() {
        let mounts = parse_mountinfo(
            b"10 1 8:1 / / rw - ext4 /dev/sda1 rw\n\
              11 10 8:2 / /home rw - ext4 /dev/sdb1 rw\n\
              12 11 8:2 /projects /home/me/projects rw - ext4 /dev/sdb1 rw\n\
              13 10 8:3 / /home/me/projects rw - ext4 /dev/sdc1 rw\n",
        );
        let mount = containing_mount(
            &mounts,
            Path::new("/home/me/projects/code"),
            Some(12),
            (8, 3),
        )
        .unwrap();
        assert_eq!(mount.path, Path::new("/home/me/projects"));
        assert_eq!(mount.source, "/dev/sdb1");
        let mount = containing_mount(&mounts, Path::new("/home-other/me"), None, (8, 1)).unwrap();
        assert_eq!(mount.path, Path::new("/"));
        let mount =
            containing_mount(&mounts, Path::new("/home/me/projects/code"), None, (8, 2)).unwrap();
        assert_eq!(mount.id, 12);
    }

    #[test]
    fn btrfs_subvolumes_do_not_require_matching_device_numbers() {
        let mounts = parse_mountinfo(
            b"40 1 0:34 /@ / rw - btrfs /dev/nvme0n1p3 rw,subvol=/@\n\
              51 40 0:34 /@home /home rw - btrfs /dev/nvme0n1p3 rw,subvol=/@home\n",
        );
        for mount_id in [Some(51), None] {
            let mount =
                containing_mount(&mounts, Path::new("/home/me/Documents"), mount_id, (0, 46))
                    .unwrap();
            assert_eq!(mount.path, Path::new("/home"));
            assert_eq!(mount.source, "/dev/nvme0n1p3");
        }
        let root = containing_mount(&mounts, Path::new("/etc"), Some(40), (0, 36)).unwrap();
        assert_eq!(root.path, Path::new("/"));
    }

    #[test]
    fn capacity_is_available_on_real_root_and_home_filesystems() {
        let mut paths = vec![PathBuf::from("/")];
        if let Some(home) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
        {
            paths.push(home);
        }
        for path in paths {
            let usage =
                disk_usage(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            assert!(usage.total_bytes > 0);
            assert!(usage.available_bytes <= usage.total_bytes);
            assert!(fs::canonicalize(&path)
                .unwrap()
                .starts_with(&usage.mount_path));
            eprintln!(
                "{}: mount={}, total={}, available={}, filled={:.1}%",
                path.display(),
                usage.mount_path.display(),
                usage.total_bytes,
                usage.available_bytes,
                usage.usage_ratio * 100.
            );
        }
    }

    #[test]
    fn capacity_preserves_reserved_blocks_and_rejects_invalid_counts() {
        let (total, used, available, ratio) = capacity(100, 40, 30, 1024).unwrap();
        assert_eq!((total, used, available), (102_400, 61_440, 30_720));
        assert!((ratio - 0.7).abs() < f64::EPSILON);
        assert!(capacity(0, 0, 0, 4096).is_err());
        assert!(capacity(1, 0, 0, 0).is_err());
        assert!(capacity(10, 11, 10, 4096).is_err());
        assert!(capacity(10, 5, 6, 4096).is_err());
        assert!(capacity(u64::MAX, 0, 0, 4096).is_err());
    }

    #[test]
    fn real_filesystem_usage_and_errors() {
        let directory = tempfile::tempdir().unwrap();
        let usage = disk_usage(directory.path()).unwrap();
        assert!(usage.total_bytes > 0);
        assert!(usage.used_bytes <= usage.total_bytes);
        assert!(usage.available_bytes <= usage.total_bytes - usage.used_bytes);
        assert!((0.0..=1.0).contains(&usage.usage_ratio));
        assert!(directory.path().starts_with(&usage.mount_path));
        assert!(!usage.source.is_empty());
        let missing = disk_usage(&directory.path().join("missing")).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);

        let non_utf8 = directory
            .path()
            .join(OsString::from_vec(b"disk-\xff".to_vec()));
        fs::create_dir(&non_utf8).unwrap();
        let non_utf8_usage = disk_usage(&non_utf8).unwrap();
        assert_eq!(non_utf8_usage.mount_path, usage.mount_path);
        assert_eq!(non_utf8_usage.source, usage.source);
    }

    #[test]
    fn btrfs_subvolumes_snapshots_and_system_mounts_are_never_duplicated_as_extra_disks() {
        let mountinfo = br#"37 40 0:34 /@snapshots /.snapshots rw,noatime shared:115 - btrfs /dev/nvme0n1p3 rw,subvol=/@snapshots
40 1 0:34 /@ / rw,noatime shared:1 - btrfs /dev/nvme0n1p3 rw,subvol=/@
60 40 0:34 /@cache /var/cache/pacman/pkg rw,noatime shared:119 - btrfs /dev/nvme0n1p3 rw,subvol=/@cache
64 40 0:34 /@log /var/log rw,noatime shared:123 - btrfs /dev/nvme0n1p3 rw,subvol=/@log
59 40 0:34 /@home /home rw,noatime shared:127 - btrfs /dev/nvme0n1p3 rw,subvol=/@home
161 40 259:1 / /boot/efi rw,relatime shared:131 - vfat /dev/nvme0n1p1 rw
200 40 8:1 / /run/media/tymur/USB_DRIVE rw,relatime shared:200 - exfat /dev/sda1 rw
"#;
        let filtered = filter_unique_user_mounts(parse_mountinfo(mountinfo));
        let paths: Vec<PathBuf> = filtered.into_iter().map(|m| m.path).collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/"),
                PathBuf::from("/run/media/tymur/USB_DRIVE")
            ]
        );
    }
}
