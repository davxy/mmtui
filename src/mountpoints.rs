use crate::config::Config;

#[derive(Debug)]
pub struct MountPoint {
    pub dev: String,
    pub path: Option<String>,
    pub fs: String,
    pub mounted: bool,
}

impl MountPoint {
    pub fn collect_from_file(path: &str, config: &Config) -> Vec<MountPoint> {
        let Ok(content) = std::fs::read(path) else {
            return Vec::new();
        };
        String::from_utf8_lossy(&content)
            .lines()
            .filter_map(|l| {
                let mut parts = l.split_whitespace();
                Some(MountPoint {
                    dev: parts
                        .next()
                        .filter(|d| !d.starts_with('#'))
                        .map(decode_octal_escapes)?,
                    path: Some(decode_octal_escapes(parts.next()?)),
                    fs: decode_octal_escapes(parts.next()?),
                    mounted: false,
                })
            })
            .filter(|p| !config.fstype_ignore.contains(&p.fs))
            .filter(|p| {
                if let Some(p) = &p.path {
                    !config
                        .path_ignore
                        .iter()
                        .any(|ignore| p.starts_with(ignore))
                } else {
                    false
                }
            })
            .collect()
    }

    pub fn collect(config: &Config) -> Vec<MountPoint> {
        let mnt = Self::collect_from_file("/proc/self/mounts", config);
        let fstab = Self::collect_from_file("/etc/fstab", config);

        let fstab: Vec<MountPoint> = fstab
            .into_iter()
            .filter(|p| !mnt.iter().any(|f| f.path == p.path))
            .map(|p| MountPoint {
                dev: kernel_device_name(&p.dev),
                mounted: false,
                ..p
            })
            .collect();

        mnt.into_iter()
            .map(|m| MountPoint { mounted: true, ..m })
            .chain(fstab)
            .collect()
    }
}

fn decode_octal_escapes(field: &str) -> String {
    let mut decoded = Vec::with_capacity(field.len());
    let mut rest = field.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        if let Some(escaped) = octal_escape(rest) {
            decoded.push(escaped);
            rest = &rest[4..];
        } else {
            decoded.push(byte);
            rest = tail;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn octal_escape(bytes: &[u8]) -> Option<u8> {
    match bytes.get(..4)? {
        [b'\\', digits @ ..] if digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) => {
            u8::from_str_radix(std::str::from_utf8(digits).ok()?, 8).ok()
        }
        _ => None,
    }
}

// fstab can name a device by a tag or a udev link, but udisks and /proc/self/mounts use the kernel name.
// mount finds the device through the same /dev/disk links.
fn kernel_device_name(fstab_name: &str) -> String {
    let link = fstab_device_link(fstab_name);
    if !link.starts_with("/dev/") {
        return fstab_name.to_owned();
    }
    std::fs::canonicalize(link)
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
        .unwrap_or_else(|| fstab_name.to_owned())
}

fn fstab_device_link(fstab_name: &str) -> String {
    match fstab_name.split_once('=') {
        Some(("UUID", uuid)) => format!("/dev/disk/by-uuid/{}", blkid_encode(uuid)),
        Some(("LABEL", label)) => format!("/dev/disk/by-label/{}", blkid_encode(label)),
        Some(("PARTUUID", uuid)) => format!("/dev/disk/by-partuuid/{uuid}"),
        Some(("PARTLABEL", label)) => format!("/dev/disk/by-partlabel/{label}"),
        _ => fstab_name.to_owned(),
    }
}

fn blkid_encode(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if !character.is_ascii()
                || character.is_ascii_alphanumeric()
                || "#+-.:=@_".contains(character)
            {
                character.to_string()
            } else {
                format!("\\x{:02x}", u32::from(character))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_from_text(name: &str, text: impl AsRef<[u8]>) -> Vec<MountPoint> {
        let file = std::env::temp_dir().join(format!("mmtui-{}-{name}", std::process::id()));
        std::fs::write(&file, text).unwrap();
        let mounts = MountPoint::collect_from_file(file.to_str().unwrap(), &Config::default());
        std::fs::remove_file(&file).unwrap();
        mounts
    }

    // fstab(5) and the kernel write a space as \040 and a tab as \011, in any field.
    // udisks mounts at /run/media/<user>/<label>, labels often have spaces, and `l` must print the real path.
    #[test]
    fn escaped_fields_are_decoded() {
        let mounts = collect_from_text(
            "escaped",
            "/dev/sdc1 /run/media/user/My\\040Photos ext4 rw 0 0\n\
             user@host:/srv/my\\040share /mnt/back\\134slash fuse.sshfs rw 0 0\n\
             LABEL=foo\\040bar /mnt/data my\\011fs rw 0 0\n",
        );
        assert_eq!(mounts[0].path.as_deref(), Some("/run/media/user/My Photos"));
        assert_eq!(mounts[1].dev, "user@host:/srv/my share");
        assert_eq!(mounts[1].path.as_deref(), Some("/mnt/back\\slash"));
        assert_eq!(mounts[2].dev, "LABEL=foo bar");
        assert_eq!(mounts[2].fs, "my\tfs");
    }

    // A hand-written fstab line can have a backslash that is not an escape. It must stay as it is.
    #[test]
    fn invalid_escapes_stay_as_they_are() {
        let mounts = collect_from_text(
            "invalid",
            "/dev/sdd1 /mnt/a\\777b\\08c\\+12\\ ext4 rw 0 0\n",
        );
        assert_eq!(mounts[0].path.as_deref(), Some("/mnt/a\\777b\\08c\\+12\\"));
    }

    // The kernel escapes only space, tab, new line and backslash, and writes all other path bytes as they are.
    // A path that is not valid UTF-8 (for example Latin-1 from an old disk) must not hide the next mount points.
    #[test]
    fn line_that_is_not_utf8_does_not_hide_next_lines() {
        let mounts = collect_from_text(
            "not-utf8",
            b"/dev/sdc1 /mnt/caf\xe9 vfat rw 0 0\n/dev/sdd1 /mnt/data ext4 rw 0 0\n",
        );
        let paths: Vec<_> = mounts.iter().map(|mount| mount.path.as_deref()).collect();
        assert_eq!(paths, [Some("/mnt/caf\u{FFFD}"), Some("/mnt/data")]);
    }

    // fstab can name a device by a tag. udev makes a /dev/disk link for each tag, and mount finds the device
    // through that link. blkid encodes a label for the link name, for example a space as \x20.
    #[test]
    fn fstab_tags_map_to_udev_links() {
        assert_eq!(
            fstab_device_link("UUID=d06c58e9-6860"),
            "/dev/disk/by-uuid/d06c58e9-6860"
        );
        assert_eq!(
            fstab_device_link("LABEL=My Photos"),
            "/dev/disk/by-label/My\\x20Photos"
        );
        assert_eq!(
            fstab_device_link("PARTUUID=0a1b2c3d-02"),
            "/dev/disk/by-partuuid/0a1b2c3d-02"
        );
        assert_eq!(
            fstab_device_link("PARTLABEL=root"),
            "/dev/disk/by-partlabel/root"
        );
        assert_eq!(
            fstab_device_link("/dev/disk/by-id/usb-disk"),
            "/dev/disk/by-id/usb-disk"
        );
    }

    // A disk that is not connected has no link. Its fstab line keeps its own row with the fstab name.
    #[test]
    fn name_of_device_that_is_not_connected_stays() {
        assert_eq!(kernel_device_name("UUID=no-such-uuid"), "UUID=no-such-uuid");
    }

    // Names like "tmpfs" or "host:/srv" are not devices. Only /dev paths may be looked up in the file system.
    #[test]
    fn only_dev_paths_are_resolved() {
        let directory = std::env::temp_dir().join(format!("mmtui-{}-links", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let link = directory.join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&directory, &link).unwrap();
        let name = link.to_str().unwrap();
        let resolved = kernel_device_name(name);
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_dir(&directory).unwrap();
        assert_eq!(resolved, name);
    }

    // /etc/fstab is optional. A panic here stops the scan task, and the table never changes again.
    #[test]
    fn missing_file_gives_no_mount_points() {
        let file = std::env::temp_dir().join(format!("mmtui-{}-missing", std::process::id()));
        let mounts = MountPoint::collect_from_file(file.to_str().unwrap(), &Config::default());
        assert!(mounts.is_empty());
    }
}
