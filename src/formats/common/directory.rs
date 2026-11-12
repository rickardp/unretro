#[cfg(any(feature = "macintosh", feature = "dos"))]
use std::borrow::Cow;
use std::fs;
#[cfg(any(feature = "macintosh", feature = "dos"))]
use std::path::Component;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::format::ContainerFormat;
use crate::{Container, ContainerInfo, Entry};

#[cfg(feature = "macintosh")]
use crate::formats::macintosh::resource_fork;

struct DirEntry {
    path: PathBuf,
    is_directory: bool,
}

pub struct DirectoryContainer {
    #[cfg(any(feature = "macintosh", feature = "dos"))]
    root: PathBuf,
    prefix: String,
    entries: Vec<DirEntry>,
    depth: u32,
}

impl DirectoryContainer {
    pub fn open(path: impl AsRef<Path>, depth: u32) -> Result<Self> {
        let path = path.as_ref();
        let prefix = path.display().to_string();
        Self::open_with_prefix(path, prefix, depth)
    }

    pub fn open_with_prefix(path: impl AsRef<Path>, prefix: String, depth: u32) -> Result<Self> {
        let path = path.as_ref();

        if depth == 0 {
            return Err(Error::MaxDepthExceeded);
        }

        if !path.is_dir() {
            return Err(Error::invalid_format(format!(
                "Not a directory: {}",
                path.display()
            )));
        }

        let mut entries = Vec::new();

        // Read directory contents
        let read_dir = fs::read_dir(path)
            .map_err(|e| Error::invalid_format(format!("Cannot read directory: {e}")))?;

        for entry_result in read_dir {
            let Ok(entry) = entry_result else { continue };

            let entry_path = entry.path();
            let is_dir = entry_path.is_dir();
            entries.push(DirEntry {
                path: entry_path,
                is_directory: is_dir,
            });
        }

        // Sort entries by path for consistent ordering
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(Self {
            #[cfg(any(feature = "macintosh", feature = "dos"))]
            root: path.to_path_buf(),
            prefix,
            entries,
            depth,
        })
    }
}

#[cfg(any(feature = "macintosh", feature = "dos"))]
impl DirectoryContainer {
    // Sibling paths are relative to the root directory; anything that could
    // escape it is rejected.
    fn sibling_path(&self, rel: &str) -> Option<PathBuf> {
        let rel = Path::new(rel);
        if rel
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        {
            Some(self.root.join(rel))
        } else {
            None
        }
    }
}

impl Container for DirectoryContainer {
    fn prefix(&self) -> &str {
        &self.prefix
    }

    fn info(&self) -> ContainerInfo {
        ContainerInfo {
            path: self.prefix().to_string(),
            format: ContainerFormat::Directory,
            entry_count: Some(self.entries.len()),
        }
    }

    #[cfg(any(feature = "macintosh", feature = "dos"))]
    fn read_file(&self, path: &str) -> Option<Cow<'_, [u8]>> {
        let full = self.sibling_path(path)?;
        if !full.is_file() {
            return None;
        }
        fs::read(full).ok().map(Cow::Owned)
    }

    // The fork itself is read when the file is visited; the header and the
    // length are enough to tell whether it will be yielded.
    #[cfg(feature = "macintosh")]
    fn has_resource_fork(&self, path: &str) -> bool {
        use std::io::Read;
        let Some(full) = self.sibling_path(path) else {
            return false;
        };
        let fork_path = format!("{}{}", full.display(), resource_fork::RESOURCE_FORK_SUFFIX);
        let Ok(mut file) = fs::File::open(fork_path) else {
            return false;
        };
        let Some(fork_len) = file
            .metadata()
            .ok()
            .and_then(|meta| usize::try_from(meta.len()).ok())
        else {
            return false;
        };
        let mut header = [0u8; 16];
        file.read_exact(&mut header).is_ok()
            && resource_fork::ResourceFork::is_valid_header(&header, fork_len)
    }

    #[cfg(any(feature = "macintosh", feature = "dos"))]
    fn has_file(&self, path: &str) -> bool {
        self.sibling_path(path).is_some_and(|p| p.is_file())
    }

    #[cfg(feature = "macintosh")]
    fn has_dir(&self, path: &str) -> bool {
        self.sibling_path(path).is_some_and(|p| p.is_dir())
    }

    fn visit(&self, visitor: &mut dyn FnMut(&Entry<'_>) -> Result<bool>) -> Result<()> {
        for dir_entry in &self.entries {
            if dir_entry.is_directory {
                // Subdirectory - recurse into it
                if let Ok(container) = Self::open(&dir_entry.path, self.depth - 1) {
                    container.visit(visitor)?;
                }
            } else {
                // Regular file - read and yield
                let data = match fs::read(&dir_entry.path) {
                    Ok(d) => d,
                    Err(e) => {
                        return Err(Error::invalid_format(format!(
                            "Cannot read file {}: {e}",
                            dir_entry.path.display()
                        )));
                    }
                };

                let path_str = dir_entry.path.to_string_lossy();
                let entry = Entry::new(&path_str, &self.prefix, &data);
                visitor(&entry)?;

                // Native fork (macOS). Only the fork entry is yielded; the
                // loader descends into it like any other `..namedfork/rsrc`
                // entry, honoring its options.
                #[cfg(feature = "macintosh")]
                if let Some(rsrc) = resource_fork::read_native_resource_fork(&dir_entry.path) {
                    let rsrc_path = format!("{path_str}{}", resource_fork::RESOURCE_FORK_SUFFIX);
                    visitor(&Entry::new(&rsrc_path, &self.prefix, &rsrc))?;
                }
            }
        }
        Ok(())
    }
}
