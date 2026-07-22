use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMPORARY_FILE_ID: AtomicU64 = AtomicU64::new(1);

pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        set_unix_mode(parent, 0o700)?;
        if !fs::symlink_metadata(parent)?.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "atomic-write parent is not a directory",
            ));
        }
    }
    if let Ok(metadata) = fs::symlink_metadata(path)
        && !metadata.file_type().is_file()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "atomic-write destination is not a regular file",
        ));
    }

    let temporary = temporary_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        set_unix_mode(&temporary, 0o600)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        if let Some(parent) = path.parent() {
            OpenOptions::new().read(true).open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn temporary_path(path: &Path) -> PathBuf {
    let id = TEMPORARY_FILE_ID.fetch_add(1, Ordering::Relaxed);
    let suffix = format!("{}.{}.{}.tmp", std::process::id(), id, current_time_nanos());
    path.with_file_name(format!(
        ".{}.{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        suffix
    ))
}

fn current_time_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn set_unix_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::write_atomic;

    #[test]
    fn replaces_regular_file_without_leaving_temporary_files() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("config.toml");
        write_atomic(&path, b"first").expect("first write");
        write_atomic(&path, b"second").expect("second write");
        assert_eq!(fs::read_to_string(&path).expect("read file"), "second");
        assert_eq!(
            fs::read_dir(directory.path())
                .expect("read directory")
                .count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_destination() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().expect("tempdir");
        let target = directory.path().join("target");
        fs::write(&target, b"original").expect("target");
        let link = directory.path().join("link");
        symlink(&target, &link).expect("symlink");
        assert!(write_atomic(&link, b"replacement").is_err());
        assert_eq!(fs::read(&target).expect("read target"), b"original");
    }
}
