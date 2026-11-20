use crate::config::Config;
use std::{io::BufRead, path::PathBuf};

#[derive(Debug)]
pub struct MountPoint {
    pub dev: String,
    pub path: Option<String>,
    pub fs: String,
    pub mounted: bool,
}

impl MountPoint {
    pub fn collect_from_file(path: &str, config: &Config) -> Vec<MountPoint> {
        std::io::BufReader::new(std::fs::File::open(PathBuf::from(path)).unwrap())
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| {
                let mut parts = l.split_whitespace();
                Some(MountPoint {
                    dev: parts
                        .next()
                        .and_then(|d| if d.starts_with('#') { None } else { Some(d) })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_from_text(name: &str, text: &str) -> Vec<MountPoint> {
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
}
