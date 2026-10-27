//! Cleanup-on-error for instance folders created by an install/import.
//!
//! Pack imports create the final profile folder first and write the profile
//! row last. Without a guard every early `?` in between (bad zip entry, one
//! CDN 404) leaves a half-filled folder in the profiles dir that no profile
//! points to, and the next attempt picks `<name> (1)` next to it.

use log::{info, warn};
use std::path::{Path, PathBuf};

/// Removes the directory on drop unless [`CreatedDirGuard::disarm`] was called.
/// Only owns a directory this guard actually created — an already existing
/// folder is never touched.
#[must_use = "dropping the guard immediately removes the directory again"]
pub struct CreatedDirGuard {
    path: Option<PathBuf>,
}

impl CreatedDirGuard {
    /// Creates `path` (and parents). If it already existed the guard is inert.
    pub async fn create(path: &Path) -> std::io::Result<Self> {
        let existed = tokio::fs::try_exists(path).await.unwrap_or(false);
        tokio::fs::create_dir_all(path).await?;
        Ok(Self {
            path: (!existed).then(|| path.to_path_buf()),
        })
    }

    /// Keep the directory: the install succeeded.
    pub fn disarm(mut self) {
        self.path = None;
    }
}

impl Drop for CreatedDirGuard {
    fn drop(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        info!("Removing incomplete instance folder {:?}", path);
        // Removing a big half-extracted pack can take a while; don't block an
        // async worker with it when we are on one.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(move || remove(&path));
            }
            Err(_) => remove(&path),
        }
    }
}

fn remove(path: &Path) {
    if let Err(e) = std::fs::remove_dir_all(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!("Could not remove incomplete folder {:?}: {}", path, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_new_dir_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("profile");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let guard = rt.block_on(CreatedDirGuard::create(&dir)).unwrap();
        std::fs::write(dir.join("file.txt"), b"x").unwrap();
        assert!(dir.exists());
        drop(guard);
        drop(rt); // waits for the spawn_blocking removal
        assert!(!dir.exists());
    }

    #[test]
    fn keeps_dir_when_disarmed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("profile");
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            CreatedDirGuard::create(&dir).await.unwrap().disarm();
        });
        drop(rt);
        assert!(dir.exists());
    }

    #[test]
    fn never_removes_preexisting_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("existing");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keep.txt"), b"x").unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            drop(CreatedDirGuard::create(&dir).await.unwrap());
        });
        drop(rt);
        assert!(dir.join("keep.txt").exists());
    }
}
