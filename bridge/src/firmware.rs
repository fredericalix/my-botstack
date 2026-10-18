//! Firmware updates over the air (docs/firmware.md#updates).
//!
//! `scripts/publish-firmware.sh` puts a signed image on the data volume, one per
//! firmware project (`<dir>/<project>.bin`): the project is the hardware type.
//! The bridge offers a device the build of its own project (`firmware`
//! message) and serves it at `/botstack/v1/firmware`; the device checks the
//! signature before it installs anything, so the bridge only hands the file over.

use std::io::Read;
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

use crate::fleet::valid_project;

/// `esp_app_desc_t` starts 32 bytes into the image (image header, first segment header).
const DESC: usize = 32;
const DESC_MAGIC: u32 = 0xABCD_5432;
/// Magic, secure version, reserved, version, project name, time, date, IDF version, ELF SHA-256.
const DESC_LEN: usize = 176;

pub struct Firmware {
    /// The published images, `<project>.bin`.
    pub dir: PathBuf,
    /// Public URL the devices download from.
    pub url: String,
}

/// What an ESP-IDF app image says about itself.
#[derive(Debug, PartialEq)]
pub struct Image {
    pub project: String,
    /// The first 8 bytes of its ELF SHA-256 in hex, as the firmware reports its own in its hello.
    pub build: String,
}

pub struct Published {
    pub project: String,
    pub build: String,
    pub bytes: u64,
    /// Unix seconds.
    pub modified: u64,
}

impl Firmware {
    /// Where `project`'s image lives; None for a name that is not a project.
    pub fn path(&self, project: &str) -> Option<PathBuf> {
        valid_project(project).then(|| self.dir.join(format!("{project}.bin")))
    }

    /// The published build for `project`, or None when nothing (valid) is published.
    pub fn build(&self, project: &str) -> Option<String> {
        let mut head = [0u8; DESC + DESC_LEN];
        std::fs::File::open(self.path(project)?).ok()?.read_exact(&mut head).ok()?;
        image_info(&head).filter(|image| image.project == project).map(|image| image.build)
    }

    /// Every valid published image, by project.
    pub fn published(&self) -> Vec<Published> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut list: Vec<Published> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let project = name.strip_suffix(".bin")?.to_string();
                let build = self.build(&project)?;
                let meta = entry.metadata().ok()?;
                let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
                Some(Published { project, build, bytes: meta.len(), modified })
            })
            .collect();
        list.sort_by(|a, b| a.project.cmp(&b.project));
        list
    }
}

/// Project and build of an ESP-IDF app image, or None when it is not one.
pub fn image_info(image: &[u8]) -> Option<Image> {
    let desc = image.get(DESC..DESC + DESC_LEN)?;
    if u32::from_le_bytes(desc[0..4].try_into().ok()?) != DESC_MAGIC {
        return None;
    }
    let field = &desc[48..80];
    let project = std::str::from_utf8(&field[..field.iter().position(|&b| b == 0).unwrap_or(field.len())]).ok()?;
    if !valid_project(project) {
        return None;
    }
    let build = desc[144..152].iter().map(|b| format!("{b:02x}")).collect();
    Some(Image { project: project.to_string(), build })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(project: &[u8]) -> Vec<u8> {
        let mut image = vec![0u8; 4096];
        image[DESC..DESC + 4].copy_from_slice(&DESC_MAGIC.to_le_bytes());
        image[DESC + 48..DESC + 48 + project.len()].copy_from_slice(project);
        for (i, byte) in image[DESC + 144..DESC + 176].iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(17);
        }
        image
    }

    #[test]
    fn reads_project_and_build() {
        let expected = Image { project: "botstack-fw".into(), build: "0011223344556677".into() };
        assert_eq!(image_info(&image(b"botstack-fw")), Some(expected));
        assert_eq!(image_info(&image(b"botstack-p4")).unwrap().project, "botstack-p4");
    }

    #[test]
    fn refuses_broken_images() {
        let mut broken = image(b"botstack-fw");
        broken[DESC] ^= 1;
        assert_eq!(image_info(&broken), None);
        assert_eq!(image_info(&image(b"botstack-fw")[..100]), None);
        assert_eq!(image_info(&image(b"Bad Name")), None);
    }

    #[test]
    fn one_image_per_project() {
        let dir = tempfile::tempdir().unwrap();
        let firmware = Firmware { dir: dir.path().to_path_buf(), url: String::new() };
        assert_eq!(firmware.build("botstack-fw"), None);
        assert!(firmware.published().is_empty());
        std::fs::write(dir.path().join("botstack-fw.bin"), image(b"botstack-fw")).unwrap();
        // A file whose image is another project is not that project's firmware.
        std::fs::write(dir.path().join("botstack-p4.bin"), image(b"botstack-fw")).unwrap();
        assert_eq!(firmware.build("botstack-fw").as_deref(), Some("0011223344556677"));
        assert_eq!(firmware.build("botstack-p4"), None);
        let published = firmware.published();
        assert_eq!(published.len(), 1);
        assert_eq!((published[0].project.as_str(), published[0].bytes), ("botstack-fw", 4096));
    }

    #[test]
    fn a_project_is_never_a_path() {
        let firmware = Firmware { dir: "/data/firmware".into(), url: String::new() };
        assert_eq!(firmware.path("botstack-fw"), Some("/data/firmware/botstack-fw.bin".into()));
        assert_eq!(firmware.path("../botstack-fw"), None);
        assert_eq!(firmware.path("a/b"), None);
        assert_eq!(firmware.path(""), None);
    }
}
