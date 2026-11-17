//! Blocking and bounded-retry OS file locks with symlink confinement (`ADR-0002`).

use crate::fs::ensure_confined_dir_all_abs;
use anyhow::{bail, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// An exclusive or shared OS file lock guard that explicitly unlocks on drop.
///
/// Explicit `unlock()` prevents inherited file descriptors across `fork()` from
/// holding the lock after the owning operation completes.
#[derive(Debug)]
pub struct FileLockGuard {
    file: File,
    path: PathBuf,
}

/// Alias for [`FileLockGuard`] preserved for callers expecting `FileLock`.
pub type FileLock = FileLockGuard;

impl FileLockGuard {
    /// Returns the path of the locked file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn open_confined_lock_file(path: &Path) -> Result<File> {
    if path.as_os_str().is_empty() || path == Path::new(":memory:") {
        bail!("file lock requires a filesystem path");
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute
        .parent()
        .context("file lock path requires a parent directory")?;
    ensure_confined_dir_all_abs(parent)?;
    if fs::symlink_metadata(&absolute).is_ok_and(|meta| meta.file_type().is_symlink()) {
        bail!("file lock must not be a symlink: {}", absolute.display());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(&absolute)?;
    if !file.metadata()?.is_file() {
        bail!("file lock must be a regular file: {}", absolute.display());
    }
    Ok(file)
}

fn acquire_bounded(
    lock_path: &Path,
    exclusive: bool,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<FileLockGuard> {
    let file = open_confined_lock_file(lock_path)?;
    let started = Instant::now();
    loop {
        let attempt = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        match attempt {
            Ok(()) => {
                return Ok(FileLockGuard {
                    file,
                    path: lock_path.to_path_buf(),
                });
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                let elapsed = started.elapsed();
                if elapsed >= timeout {
                    bail!(
                        "timed out acquiring {} lock on '{}' after {:?}",
                        if exclusive { "exclusive" } else { "shared" },
                        lock_path.display(),
                        timeout
                    );
                }
                let remaining = timeout.saturating_sub(elapsed);
                if poll_interval.is_zero() {
                    std::thread::yield_now();
                } else {
                    std::thread::sleep(poll_interval.min(remaining));
                }
            }
            Err(std::fs::TryLockError::Error(err))
                if err.kind() == std::io::ErrorKind::Interrupted =>
            {
                continue;
            }
            Err(std::fs::TryLockError::Error(err)) => return Err(err.into()),
        }
    }
}

/// Acquires an exclusive lock on `lock_path`, retrying up to `timeout` every `poll_interval`.
pub fn acquire_exclusive_bounded(
    lock_path: &Path,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<FileLockGuard> {
    acquire_bounded(lock_path, true, timeout, poll_interval)
}

/// Acquires a shared lock on `lock_path`, retrying up to `timeout` every `poll_interval`.
pub fn acquire_shared_bounded(
    lock_path: &Path,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<FileLockGuard> {
    acquire_bounded(lock_path, false, timeout, poll_interval)
}

/// Acquires a blocking exclusive lock on `path` using `std::fs::File::lock`.
pub fn acquire_exclusive(path: &Path) -> Result<FileLockGuard> {
    let file = open_confined_lock_file(path)?;
    file.lock()?;
    Ok(FileLockGuard {
        file,
        path: path.to_path_buf(),
    })
}

/// Attempts to acquire an exclusive lock on `path` without blocking.
pub fn try_acquire_exclusive(path: &Path) -> Result<Option<FileLockGuard>> {
    let file = open_confined_lock_file(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(FileLockGuard {
            file,
            path: path.to_path_buf(),
        })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(err)) => Err(err.into()),
    }
}

/// Acquires a blocking shared lock on `path` using `std::fs::File::lock_shared`.
pub fn acquire_shared(path: &Path) -> Result<FileLockGuard> {
    let file = open_confined_lock_file(path)?;
    file.lock_shared()?;
    Ok(FileLockGuard {
        file,
        path: path.to_path_buf(),
    })
}

/// Attempts to acquire a shared lock on `path` without blocking.
pub fn try_acquire_shared(path: &Path) -> Result<Option<FileLockGuard>> {
    let file = open_confined_lock_file(path)?;
    match file.try_lock_shared() {
        Ok(()) => Ok(Some(FileLockGuard {
            file,
            path: path.to_path_buf(),
        })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(err)) => Err(err.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDirDrop(PathBuf);
    impl Drop for TempDirDrop {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn lock_guards_acquire_and_release_cleanly() {
        let dir = std::env::temp_dir().join(format!("acdd_core_lock_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let _cleanup = TempDirDrop(dir.clone());
        let lock_path = dir.join("nested/repo.lock");

        let guard = acquire_exclusive(&lock_path).unwrap();
        assert!(try_acquire_exclusive(&lock_path).unwrap().is_none());
        drop(guard);

        let shared1 = acquire_shared(&lock_path).unwrap();
        let shared2 = try_acquire_shared(&lock_path).unwrap();
        assert!(shared2.is_some());
        drop(shared1);
        drop(shared2);

        let reacquired = try_acquire_exclusive(&lock_path).unwrap();
        assert!(reacquired.is_some());
        assert!(acquire_shared_bounded(
            &lock_path,
            Duration::from_millis(15),
            Duration::from_millis(5)
        )
        .is_err());
        drop(reacquired);

        let bounded_shared = acquire_shared_bounded(
            &lock_path,
            Duration::from_millis(50),
            Duration::from_millis(5),
        )
        .unwrap();
        assert!(acquire_exclusive_bounded(
            &lock_path,
            Duration::from_millis(15),
            Duration::from_millis(5)
        )
        .is_err());
        drop(bounded_shared);

        let bounded_excl = acquire_exclusive_bounded(
            &lock_path,
            Duration::from_millis(50),
            Duration::from_millis(5),
        )
        .unwrap();
        drop(bounded_excl);

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let link_lock = dir.join("symlink.lock");
            symlink(&lock_path, &link_lock).unwrap();
            assert!(acquire_exclusive(&link_lock).is_err());
        }
    }
}
