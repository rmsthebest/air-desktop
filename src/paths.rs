//! Where air-desktop keeps its files.
//!
//! `AIR_DESKTOP_CONFIG_DIR` and `AIR_DESKTOP_DATA_DIR` override the
//! locations, e.g. to run two instances side by side during development.

use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;

pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("AIR_DESKTOP_CONFIG_DIR") {
        return Some(dir.into());
    }
    ProjectDirs::from("", "", "air-desktop").map(|dirs| dirs.config_dir().to_path_buf())
}

/// Holds the account databases (keys, messages). Must stay private to the
/// user: see [`ensure_private_dir`].
pub fn data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("AIR_DESKTOP_DATA_DIR") {
        return Some(dir.into());
    }
    ProjectDirs::from("", "", "air-desktop").map(|dirs| dirs.data_dir().to_path_buf())
}

/// Creates `dir` (and parents) and makes it accessible to the owner only
/// (0700), tightening an existing directory that is more open.
///
/// SQLite creates its files with the process umask, so the directory mode is
/// what keeps the databases private; [`restrict_files`] tightens the files
/// themselves too.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = std::fs::metadata(dir)?;
        if !metadata.is_dir() {
            return Err(io::Error::other(format!(
                "{} is not a directory",
                dir.display()
            )));
        }
        if metadata.permissions().mode() & 0o777 != 0o700 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}

/// Sets every regular file directly inside `dir` to owner read/write (0600).
pub fn restrict_files(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            // `file_type` doesn't follow symlinks, so a planted link can't
            // redirect the chmod elsewhere.
            if entry.file_type()?.is_file()
                && entry.metadata()?.permissions().mode() & 0o777 != 0o600
            {
                std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o600))?;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn data_dir_is_created_private_and_tightened() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a/b");
        ensure_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);

        // An existing, world-readable directory is tightened.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
    }

    #[test]
    fn files_are_restricted_but_symlink_targets_are_not() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("air.db");
        std::fs::write(&db, b"").unwrap();
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::set_permissions(outside.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();

        restrict_files(root.path()).unwrap();
        assert_eq!(mode(&db), 0o600);
        assert_eq!(mode(outside.path()), 0o644);
    }
}
