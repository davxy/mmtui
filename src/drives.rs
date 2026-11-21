use crate::config::Config;
use crate::mountpoints;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum Encryption {
    Unencrypted,
    Locked,
    Unlocked(Cleartext),
}

#[derive(Debug, Clone)]
pub struct Cleartext {
    pub object_path: String,
    pub devices: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub object_path: String,
    pub drive_path: String,
    pub dev: String,
    pub label: String,
    pub mount: Option<String>,
    pub fstype: String,
    pub mounted: bool,
    pub size: String,
    pub encryption: Encryption,
}

impl Block {
    fn has_device(&self, dev: &str) -> bool {
        self.dev == dev
            || matches!(&self.encryption, Encryption::Unlocked(cleartext)
                if cleartext.devices.iter().any(|name| name == dev))
    }
}

#[derive(Debug, Clone)]
pub struct Drive {
    pub id: String,
    pub object_path: String,
    pub model: String,
    pub ejectable: bool,
    pub blocks: Vec<Block>,
}

fn device_name(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| c != &'\0')
        .collect()
}

async fn encryption_of(
    client: &udisks2::Client,
    object: &udisks2::Object,
) -> udisks2::Result<Encryption> {
    let Ok(encrypted) = object.encrypted().await else {
        return Ok(Encryption::Unencrypted);
    };
    let cleartext_path = encrypted.cleartext_device().await?.to_string();
    if cleartext_path == "/" {
        return Ok(Encryption::Locked);
    }
    let cleartext = client.object(cleartext_path.clone())?.block().await?;
    Ok(Encryption::Unlocked(Cleartext {
        object_path: cleartext_path,
        devices: vec![
            device_name(&cleartext.device().await?),
            device_name(&cleartext.preferred_device().await?),
        ],
    }))
}

pub async fn collect_drives_from_udisk() -> udisks2::Result<Vec<Drive>> {
    let mut drives: Vec<Drive> = Vec::new();
    let client = udisks2::Client::new().await?;
    let objects = client
        .object_manager()
        .get_managed_objects()
        .await
        .into_iter()
        .flatten()
        .filter_map(|(object_path, _)| {
            client
                .object(object_path.clone())
                .ok()
                .map(|obj| (object_path, obj))
        });

    for (path, i) in objects {
        let path = path.to_string();
        if let Ok(drv) = i.drive().await {
            let drv = Drive {
                object_path: path,
                id: drv.id().await?,
                model: drv.model().await?,
                ejectable: drv.ejectable().await?,
                blocks: Vec::new(),
            };

            if let Some(d) = drives.iter_mut().find(|i| i.object_path == drv.object_path) {
                d.model = drv.model;
                d.ejectable = drv.ejectable;
                d.id = drv.id;
            } else {
                drives.push(drv);
            }
        } else if let Ok(blk) = i.block().await {
            if blk.crypto_backing_device().await?.as_str() != "/" {
                continue;
            }
            let drv_path = blk.drive().await?.to_string();
            let size = if let Ok(size) = blk.size().await {
                client.size_for_display(size, true, false)
            } else {
                String::new()
            };
            let block = Block {
                object_path: path,
                drive_path: drv_path.clone(),
                dev: device_name(&blk.device().await?),
                label: blk.id_label().await?,
                mount: None,
                fstype: blk.id_type().await?,
                mounted: false,
                size,
                encryption: encryption_of(&client, &i).await?,
            };

            if let Some(d) = drives.iter_mut().find(|i| i.object_path == drv_path) {
                d.blocks.push(block);
            } else {
                drives.push(Drive {
                    object_path: drv_path,
                    id: String::new(),
                    model: String::new(),
                    ejectable: false,
                    blocks: vec![block],
                });
            }
        }
    }

    Ok(drives)
}

pub async fn collect_all(config: &Config) -> udisks2::Result<Vec<Drive>> {
    let drives = collect_drives_from_udisk().await?;
    let mounts = mountpoints::MountPoint::collect(config);
    Ok(merge_mounts(drives, mounts))
}

fn merge_mounts(mut drives: Vec<Drive>, mounts: Vec<mountpoints::MountPoint>) -> Vec<Drive> {
    let mut fstab = Drive {
        id: "fstab".to_owned(),
        object_path: "fstab".to_owned(),
        model: "fstab".to_owned(),
        ejectable: false,
        blocks: Vec::new(),
    };

    for i in mounts {
        let block = drives
            .iter_mut()
            .find(|d| d.blocks.iter().any(|b| b.has_device(&i.dev)))
            .and_then(|d| d.blocks.iter_mut().find(|b| b.has_device(&i.dev)));
        if let Some(block) = block {
            // The first line is the first mount, and the fstab lines that are not mounted come last.
            if block.mount.is_none() {
                block.mount = i.path;
                block.mounted = i.mounted;
            }
        } else {
            fstab.blocks.push(Block {
                object_path: String::new(),
                drive_path: fstab.object_path.clone(),
                dev: i.dev,
                label: String::new(),
                mount: i.path,
                fstype: i.fs,
                mounted: i.mounted,
                size: String::new(),
                encryption: Encryption::Unencrypted,
            });
        }
    }

    drives.push(fstab);
    drives.sort_by_cached_key(|b| b.object_path.clone());
    for i in &mut drives {
        i.blocks.sort_by_cached_key(|b| b.dev.clone());
    }

    drives
}

pub async fn mount(block: &Block) -> udisks2::Result<()> {
    let client = udisks2::Client::new().await?;
    let filesystem_path = match &block.encryption {
        Encryption::Unlocked(cleartext) => cleartext.object_path.clone(),
        Encryption::Unencrypted | Encryption::Locked => block.object_path.clone(),
    };

    client
        .object(filesystem_path)?
        .filesystem()
        .await?
        .mount(HashMap::new())
        .await?;

    Ok(())
}

pub async fn unlock_and_mount(block: &Block, passphrase: &str) -> udisks2::Result<()> {
    let client = udisks2::Client::new().await?;

    let cleartext_path = client
        .object(block.object_path.clone())?
        .encrypted()
        .await?
        .unlock(passphrase, HashMap::new())
        .await?;
    client
        .object(cleartext_path)?
        .filesystem()
        .await?
        .mount(HashMap::new())
        .await?;

    Ok(())
}

pub async fn unmount(block: &Block) -> udisks2::Result<()> {
    let client = udisks2::Client::new().await?;

    if let Encryption::Unlocked(cleartext) = &block.encryption {
        if let Ok(filesystem) = client
            .object(cleartext.object_path.clone())?
            .filesystem()
            .await
        {
            if !filesystem.mount_points().await?.is_empty() {
                filesystem.unmount(HashMap::new()).await?;
            }
        }
        client
            .object(block.object_path.clone())?
            .encrypted()
            .await?
            .lock(HashMap::new())
            .await?;
        return Ok(());
    }

    client
        .object(block.object_path.clone())?
        .filesystem()
        .await?
        .unmount(HashMap::new())
        .await?;

    Ok(())
}

pub async fn eject(block: &Block) -> udisks2::Result<()> {
    let client = udisks2::Client::new().await?;

    client
        .object(block.drive_path.clone())?
        .drive()
        .await?
        .eject(HashMap::new())
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luks_block(encryption: Encryption) -> Block {
        Block {
            object_path: "/org/freedesktop/UDisks2/block_devices/sdc2".to_owned(),
            drive_path: "/org/freedesktop/UDisks2/drives/disk".to_owned(),
            dev: "/dev/sdc2".to_owned(),
            label: String::new(),
            mount: None,
            fstype: "crypto_LUKS".to_owned(),
            mounted: false,
            size: String::new(),
            encryption,
        }
    }

    fn unlocked() -> Encryption {
        Encryption::Unlocked(Cleartext {
            object_path: "/org/freedesktop/UDisks2/block_devices/dm_2d0".to_owned(),
            devices: vec!["/dev/dm-0".to_owned(), "/dev/mapper/luks-1234".to_owned()],
        })
    }

    // An unlocked LUKS partition shows as one row. /proc/self/mounts names its
    // filesystem /dev/mapper/<name> (mount tools) or /dev/dm-N, never the LUKS
    // partition, so the row must match both names to show its mount point.
    #[test]
    fn unlocked_row_matches_cleartext_device_names() {
        let block = luks_block(unlocked());
        assert!(block.has_device("/dev/mapper/luks-1234"));
        assert!(block.has_device("/dev/dm-0"));
        assert!(block.has_device("/dev/sdc2"));
        assert!(!block.has_device("/dev/mapper/other"));
    }

    // A locked row must not take the mount of a cleartext device: the row of
    // a locked partition always shows "not mounted".
    #[test]
    fn locked_row_matches_only_its_own_device() {
        let block = luks_block(Encryption::Locked);
        assert!(block.has_device("/dev/sdc2"));
        assert!(!block.has_device("/dev/mapper/luks-1234"));
    }

    fn drive_with_block(dev: &str) -> Drive {
        Drive {
            id: String::new(),
            object_path: "/org/freedesktop/UDisks2/drives/disk".to_owned(),
            model: String::new(),
            ejectable: false,
            blocks: vec![Block {
                dev: dev.to_owned(),
                fstype: "btrfs".to_owned(),
                ..luks_block(Encryption::Unencrypted)
            }],
        }
    }

    fn mount_line(dev: &str, path: &str, mounted: bool) -> mountpoints::MountPoint {
        mountpoints::MountPoint {
            dev: dev.to_owned(),
            path: Some(path.to_owned()),
            fs: "btrfs".to_owned(),
            mounted,
        }
    }

    fn block_of<'a>(drives: &'a [Drive], dev: &str) -> &'a Block {
        drives
            .iter()
            .flat_map(|drive| &drive.blocks)
            .find(|block| block.dev == dev)
            .unwrap()
    }

    // A btrfs partition is often mounted many times, one subvolume at each path.
    // The row must show the first mount (the root subvolume at / on a usual system), not the last one,
    // which can be any subvolume (on the test system /var/cache/pacman/pkg). `l` goes to this path.
    #[test]
    fn device_with_many_mounts_shows_first_mount() {
        let mounts = vec![
            mount_line("/dev/sda2", "/", true),
            mount_line("/dev/sda2", "/home", true),
            mount_line("/dev/sda2", "/var/cache/pacman/pkg", true),
        ];
        let drives = merge_mounts(vec![drive_with_block("/dev/sda2")], mounts);
        assert_eq!(block_of(&drives, "/dev/sda2").mount.as_deref(), Some("/"));
    }

    // MountPoint::collect lists the fstab lines that are not mounted after the mounted lines.
    // Such a line must not hide a real mount of the same device, else the row shows O for a mounted device.
    #[test]
    fn fstab_line_does_not_hide_real_mount() {
        let mounts = vec![
            mount_line("/dev/sdb1", "/run/media/user/data", true),
            mount_line("/dev/sdb1", "/mnt/data", false),
        ];
        let drives = merge_mounts(vec![drive_with_block("/dev/sdb1")], mounts);
        let block = block_of(&drives, "/dev/sdb1");
        assert_eq!(block.mount.as_deref(), Some("/run/media/user/data"));
        assert!(block.mounted);
    }
}
