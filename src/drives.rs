use crate::{config::Config, mountpoints};
use std::collections::HashMap;
use tokio::sync::OnceCell;
use udisks2::zbus::{
    fdo::ManagedObjects,
    names::OwnedInterfaceName,
    zvariant::{OwnedObjectPath, OwnedValue},
};

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
    pub object_path: String,
    pub model: String,
    pub blocks: Vec<Block>,
}

fn device_name(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| c != &'\0')
        .collect()
}

const DRIVE: &str = "org.freedesktop.UDisks2.Drive";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const ENCRYPTED: &str = "org.freedesktop.UDisks2.Encrypted";

type Interfaces = HashMap<OwnedInterfaceName, HashMap<String, OwnedValue>>;

static CLIENT: OnceCell<udisks2::Client> = OnceCell::const_new();

async fn shared_client() -> udisks2::Result<&'static udisks2::Client> {
    CLIENT.get_or_try_init(udisks2::Client::new).await
}

fn property<T: TryFrom<OwnedValue>>(
    interfaces: &Interfaces,
    interface: &str,
    name: &str,
) -> Option<T> {
    let value = interfaces.get(interface)?.get(name)?.try_clone().ok()?;
    T::try_from(value).ok()
}

fn encryption_of(objects: &ManagedObjects, interfaces: &Interfaces) -> Option<Encryption> {
    if !interfaces.contains_key(ENCRYPTED) {
        return Some(Encryption::Unencrypted);
    }
    let cleartext_path: OwnedObjectPath = property(interfaces, ENCRYPTED, "CleartextDevice")?;
    if cleartext_path.as_str() == "/" {
        return Some(Encryption::Locked);
    }
    let cleartext = objects.get(&cleartext_path)?;
    Some(Encryption::Unlocked(Cleartext {
        object_path: cleartext_path.to_string(),
        devices: vec![
            device_name(&property::<Vec<u8>>(cleartext, BLOCK, "Device")?),
            device_name(&property::<Vec<u8>>(cleartext, BLOCK, "PreferredDevice")?),
        ],
    }))
}

fn drive_from(path: &OwnedObjectPath, interfaces: &Interfaces) -> Option<Drive> {
    Some(Drive {
        object_path: path.to_string(),
        model: property(interfaces, DRIVE, "Model")?,
        blocks: Vec::new(),
    })
}

fn block_from(
    objects: &ManagedObjects,
    path: &OwnedObjectPath,
    interfaces: &Interfaces,
    size_text: impl Fn(u64) -> String,
) -> Option<Block> {
    let crypto_backing_device: OwnedObjectPath =
        property(interfaces, BLOCK, "CryptoBackingDevice")?;
    if crypto_backing_device.as_str() != "/" {
        return None;
    }
    Some(Block {
        object_path: path.to_string(),
        drive_path: property::<OwnedObjectPath>(interfaces, BLOCK, "Drive")?.to_string(),
        dev: device_name(&property::<Vec<u8>>(interfaces, BLOCK, "Device")?),
        label: property(interfaces, BLOCK, "IdLabel")?,
        mount: None,
        fstype: property(interfaces, BLOCK, "IdType")?,
        mounted: false,
        size: property(interfaces, BLOCK, "Size")
            .map(size_text)
            .unwrap_or_default(),
        encryption: encryption_of(objects, interfaces)?,
    })
}

fn drives_from_objects(objects: &ManagedObjects, size_text: impl Fn(u64) -> String) -> Vec<Drive> {
    let mut drives: Vec<Drive> = objects
        .iter()
        .filter_map(|(path, interfaces)| drive_from(path, interfaces))
        .collect();

    for (path, interfaces) in objects {
        let Some(block) = block_from(objects, path, interfaces, &size_text) else {
            continue;
        };
        if let Some(drive) = drives
            .iter_mut()
            .find(|d| d.object_path == block.drive_path)
        {
            drive.blocks.push(block);
        } else {
            drives.push(Drive {
                object_path: block.drive_path.clone(),
                model: String::new(),
                blocks: vec![block],
            });
        }
    }

    drives
}

pub async fn collect_drives_from_udisk() -> udisks2::Result<Vec<Drive>> {
    let client = shared_client().await?;
    let objects = client
        .object_manager()
        .get_managed_objects()
        .await
        .unwrap_or_default();
    Ok(drives_from_objects(&objects, |size| {
        client.size_for_display(size, true, false)
    }))
}

pub async fn collect_all(config: &Config) -> udisks2::Result<Vec<Drive>> {
    let drives = collect_drives_from_udisk().await?;
    let mounts = mountpoints::MountPoint::collect(config);
    Ok(merge_mounts(drives, mounts))
}

fn merge_mounts(mut drives: Vec<Drive>, mounts: Vec<mountpoints::MountPoint>) -> Vec<Drive> {
    let mut fstab = Drive {
        object_path: "fstab".to_owned(),
        model: String::new(),
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
    let client = shared_client().await?;
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
    let client = shared_client().await?;

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
    let client = shared_client().await?;

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
    let client = shared_client().await?;

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
    use udisks2::zbus::{names::InterfaceName, zvariant::Value};

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
            object_path: "/org/freedesktop/UDisks2/drives/disk".to_owned(),
            model: String::new(),
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

    const DISK: &str = "/org/freedesktop/UDisks2/drives/disk";
    const PARTITION: &str = "/org/freedesktop/UDisks2/block_devices/sdc2";
    const CLEARTEXT: &str = "/org/freedesktop/UDisks2/block_devices/dm_2d0";
    const LOOP: &str = "/org/freedesktop/UDisks2/block_devices/loop0";

    fn value(value: impl Into<Value<'static>>) -> OwnedValue {
        value.into().try_into().unwrap()
    }

    fn path_value(path: &str) -> OwnedValue {
        value(OwnedObjectPath::try_from(path).unwrap())
    }

    fn device_value(dev: &str) -> OwnedValue {
        value(format!("{dev}\0").into_bytes())
    }

    fn interfaces<const N: usize>(name: &str, properties: [(&str, OwnedValue); N]) -> Interfaces {
        let properties = properties
            .into_iter()
            .map(|(property, value)| (property.to_owned(), value))
            .collect();
        HashMap::from([(InterfaceName::try_from(name).unwrap().into(), properties)])
    }

    fn drive_object(model: &str) -> Interfaces {
        interfaces(DRIVE, [("Model", value(model.to_owned()))])
    }

    fn block_object(dev: &str, drive: &str) -> Interfaces {
        interfaces(
            BLOCK,
            [
                ("Device", device_value(dev)),
                ("PreferredDevice", device_value(dev)),
                ("Drive", path_value(drive)),
                ("CryptoBackingDevice", path_value("/")),
                ("IdLabel", value("data")),
                ("IdType", value("ext4")),
                ("Size", value(1024_u64)),
            ],
        )
    }

    fn encrypted(mut object: Interfaces, cleartext_device: &str) -> Interfaces {
        object.extend(interfaces(
            ENCRYPTED,
            [("CleartextDevice", path_value(cleartext_device))],
        ));
        object
    }

    fn cleartext_object(crypto_backing_device: &str) -> Interfaces {
        let mut object = block_object("/dev/dm-0", "/");
        let block = object.get_mut(BLOCK).unwrap();
        block.insert(
            "PreferredDevice".to_owned(),
            device_value("/dev/mapper/luks-1234"),
        );
        block.insert(
            "CryptoBackingDevice".to_owned(),
            path_value(crypto_backing_device),
        );
        object
    }

    fn scan(objects: Vec<(&str, Interfaces)>) -> Vec<Drive> {
        let objects = objects
            .into_iter()
            .map(|(path, interfaces)| (OwnedObjectPath::try_from(path).unwrap(), interfaces))
            .collect();
        drives_from_objects(&objects, |size| format!("{size} B"))
    }

    // The scan builds all rows from one GetManagedObjects reply. Each column must come
    // from its own udisks property, and udisks ends the Device bytes with a NUL.
    // The model is a property of the drive object, not of the partition.
    #[test]
    fn scan_shows_partition_on_its_drive() {
        let drives = scan(vec![
            (DISK, drive_object("SanDisk 3.2Gen1")),
            (PARTITION, block_object("/dev/sdc2", DISK)),
        ]);
        let [drive] = drives.as_slice() else {
            panic!("expected one drive: {drives:?}")
        };
        assert_eq!(drive.object_path, DISK);
        assert_eq!(drive.model, "SanDisk 3.2Gen1");
        let [block] = drive.blocks.as_slice() else {
            panic!("expected one row: {drive:?}")
        };
        assert_eq!(block.object_path, PARTITION);
        assert_eq!(block.drive_path, DISK);
        assert_eq!(block.dev, "/dev/sdc2");
        assert_eq!(block.label, "data");
        assert_eq!(block.fstype, "ext4");
        assert_eq!(block.size, "1024 B");
        assert!(matches!(block.encryption, Encryption::Unencrypted));
    }

    // udisks shows the cleartext device of an unlocked LUKS partition as a second block.
    // It must not be a second row. The partition row needs the cleartext names,
    // else a mount of the cleartext device does not join the row.
    #[test]
    fn unlocked_partition_is_one_row_with_cleartext_names() {
        let drives = scan(vec![
            (
                PARTITION,
                encrypted(block_object("/dev/sdc2", DISK), CLEARTEXT),
            ),
            (CLEARTEXT, cleartext_object(PARTITION)),
        ]);
        let rows: Vec<&Block> = drives.iter().flat_map(|drive| &drive.blocks).collect();
        let [row] = rows.as_slice() else {
            panic!("expected one row: {rows:?}")
        };
        assert_eq!(row.dev, "/dev/sdc2");
        let Encryption::Unlocked(cleartext) = &row.encryption else {
            panic!("expected an unlocked row: {row:?}")
        };
        assert_eq!(cleartext.object_path, CLEARTEXT);
        assert_eq!(cleartext.devices, ["/dev/dm-0", "/dev/mapper/luks-1234"]);
    }

    // `m` asks for the passphrase only on a locked row.
    // Without it, mmtui tries to mount the LUKS container and fails.
    #[test]
    fn luks_partition_without_cleartext_device_is_locked() {
        let drives = scan(vec![(
            PARTITION,
            encrypted(block_object("/dev/sdc2", DISK), "/"),
        )]);
        assert!(matches!(drives[0].blocks[0].encryption, Encryption::Locked));
    }

    // Loop devices have no udisks drive (Drive is "/"). They must still show as rows.
    #[test]
    fn block_without_drive_shows_as_row() {
        let drives = scan(vec![(LOOP, block_object("/dev/loop0", "/"))]);
        let [drive] = drives.as_slice() else {
            panic!("expected one drive: {drives:?}")
        };
        assert_eq!(drive.object_path, "/");
        assert_eq!(drive.blocks[0].dev, "/dev/loop0");
    }
}
