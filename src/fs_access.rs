#![allow(dead_code)]

use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path};

use cap_std::fs::{Dir, OpenOptions};

pub(crate) struct ConfinedDir(Dir);

impl ConfinedDir {
    pub(crate) fn open(root: &Path) -> io::Result<Self> {
        Dir::open_ambient_dir(root, cap_std::ambient_authority()).map(Self)
    }

    fn options(write: bool) -> OpenOptions {
        let mut options = OpenOptions::new();
        options.read(!write).write(write).create(write);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        options
    }

    fn relative(path: &Path) -> io::Result<()> {
        if path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path must be relative to its directory",
            ))
        }
    }

    pub(crate) fn read(&self, path: &Path, limit: u64) -> io::Result<Vec<u8>> {
        Self::relative(path)?;
        let file = self.0.open_with(path, &Self::options(false))?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "backing file exceeds the remaining budget or is not regular",
            ));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve(metadata.len().min(1024 * 1024) as usize)
            .map_err(|_| io::Error::other("cannot allocate backing file"))?;
        file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "backing file grew beyond the remaining budget",
            ));
        }
        Ok(bytes)
    }

    pub(crate) fn create(&self, path: &Path, truncate: bool) -> io::Result<File> {
        Self::relative(path)?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            self.0.create_dir_all(parent)?;
        }
        // Do not truncate until the opened object has been checked.
        let file = self.0.open_with(path, &Self::options(true))?.into_std();
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "extraction target is not a regular file",
            ));
        }
        if truncate {
            file.set_len(0)?;
        }
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::io::Write;

    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "unretro-confined-{}-{stamp}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn read_checks_regular_file_length_before_acquisition() {
        let scratch = Scratch::new();
        std::fs::write(scratch.0.join("data"), b"payload").unwrap();
        let root = ConfinedDir::open(&scratch.0).unwrap();
        assert!(root.read(Path::new("data"), 6).is_err());
        assert_eq!(root.read(Path::new("data"), 7).unwrap(), b"payload");
        assert!(root.read(Path::new("."), 100).is_err());
        assert!(root.read(Path::new("../data"), 100).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn one_opened_directory_supplies_all_backing_files_after_rename() {
        let scratch = Scratch::new();
        let inside = scratch.0.join("inside");
        let outside = scratch.0.join("outside");
        std::fs::create_dir(&inside).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(inside.join("sheet"), b"sheet").unwrap();
        std::fs::write(inside.join("track"), b"inside").unwrap();
        std::fs::write(outside.join("track"), b"outside").unwrap();
        let root = ConfinedDir::open(&inside).unwrap();
        assert_eq!(root.read(Path::new("sheet"), 100).unwrap(), b"sheet");
        std::fs::rename(&inside, scratch.0.join("moved")).unwrap();
        std::os::unix::fs::symlink(&outside, &inside).unwrap();
        assert_eq!(root.read(Path::new("track"), 100).unwrap(), b"inside");
        let selected = ConfinedDir::open(&inside).unwrap();
        assert_eq!(selected.read(Path::new("track"), 100).unwrap(), b"outside");
    }

    #[cfg(unix)]
    #[test]
    fn reads_and_writes_stay_on_opened_capability() {
        let scratch = Scratch::new();
        let inside = scratch.0.join("inside");
        std::fs::create_dir(&inside).unwrap();
        std::fs::write(scratch.0.join("outside"), b"untouched").unwrap();
        std::os::unix::fs::symlink("../outside", inside.join("escape")).unwrap();
        let root = ConfinedDir::open(&inside).unwrap();
        assert!(root.read(Path::new("escape"), 100).is_err());
        assert!(root.create(Path::new("escape"), true).is_err());
        let mut file = root.create(Path::new("output"), true).unwrap();
        std::fs::rename(inside.join("output"), inside.join("original")).unwrap();
        std::os::unix::fs::symlink("../outside", inside.join("output")).unwrap();
        file.write_all(b"safe").unwrap();
        assert_eq!(std::fs::read(inside.join("original")).unwrap(), b"safe");
        assert_eq!(
            std::fs::read(scratch.0.join("outside")).unwrap(),
            b"untouched"
        );
        std::os::unix::fs::symlink("original", inside.join("near")).unwrap();
        assert_eq!(root.read(Path::new("near"), 100).unwrap(), b"safe");
    }
}
