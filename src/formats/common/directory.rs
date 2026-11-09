#[cfg(feature = "macintosh")]
use std::borrow::Cow;
use std::fs;
#[cfg(feature = "macintosh")]
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
    #[cfg(feature = "macintosh")]
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
            #[cfg(feature = "macintosh")]
            root: path.to_path_buf(),
            prefix,
            entries,
            depth,
        })
    }
}

#[cfg(feature = "macintosh")]
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

    #[cfg(feature = "macintosh")]
    fn read_file(&self, path: &str) -> Option<Cow<'_, [u8]>> {
        let full = self.sibling_path(path)?;
        if !full.is_file() {
            return None;
        }
        fs::read(full).ok().map(Cow::Owned)
    }

    #[cfg(feature = "macintosh")]
    fn has_file(&self, path: &str) -> bool {
        self.sibling_path(path).is_some_and(|p| p.is_file())
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

                // Handle resource fork (macOS) - use names, not numeric IDs
                #[cfg(feature = "macintosh")]
                resource_fork::visit_resource_fork(&dir_entry.path, visitor, false)?;
            }
        }
        Ok(())
    }
}
