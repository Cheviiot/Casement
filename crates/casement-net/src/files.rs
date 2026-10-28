//! Files only the owner may read: keys, password keys, trust lists.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rand_core::{OsRng, RngCore};

#[cfg(unix)]
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

/// Writes through a unique temporary file so concurrent writes cannot truncate or
/// publish one another's data, and a crash never leaves a half-written destination.
pub fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let mut temp = PrivateTemp::write(path, data)?;
    fs::rename(&temp.path, path)?;
    temp.published = true;
    Ok(())
}

/// Writes a private file only when `path` does not already exist.
///
/// Returns `false` when another writer has already published the destination.
pub(crate) fn write_private_new(path: &Path, data: &[u8]) -> io::Result<bool> {
    if let Some(dir) = path.parent() {
        create_private_dir(dir)?;
    }
    let mut temp = PrivateTemp::write(path, data)?;
    match rename_noreplace(&temp.path, path) {
        Ok(()) => {
            temp.published = true;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error),
    }
}

struct PrivateTemp {
    path: PathBuf,
    published: bool,
}

impl PrivateTemp {
    fn write(destination: &Path, data: &[u8]) -> io::Result<Self> {
        let parent = destination
            .parent()
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        loop {
            let mut nonce = [0u8; 12];
            OsRng.fill_bytes(&mut nonce);
            let name = format!(
                ".csm-{}.tmp",
                nonce
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            let path = parent.join(name);
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = match options.open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            let result = file.write_all(data).and_then(|()| file.sync_all());
            drop(file);
            if let Err(error) = result {
                let _ = fs::remove_file(&path);
                return Err(error);
            }
            return Ok(Self {
                path,
                published: false,
            });
        }
    }
}

impl Drop for PrivateTemp {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Atomically moves a file to an unused destination path.
#[cfg(target_os = "linux")]
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let to = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: both C strings are NUL-terminated and live through the call; paths share
    // a destination directory, and renameat2 atomically refuses an existing target.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        #[link_name = "MoveFileW"]
        fn move_file_w(existing: *const u16, new: *const u16) -> i32;
    }

    fn extended_path(path: &Path) -> Vec<u16> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        const EXTENDED: &[u16] = &[92, 92, 63, 92]; // \\\\?\\
        const UNC: &[u16] = &[92, 92];
        let mut result = if wide.starts_with(EXTENDED) {
            wide
        } else if wide.starts_with(UNC) {
            [EXTENDED, &[85, 78, 67, 92], &wide[2..]].concat()
        } else {
            [EXTENDED, &wide].concat()
        };
        result.push(0);
        result
    }

    let from_wide = extended_path(&fs::canonicalize(from)?);
    let parent = to
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let name = to
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let target = fs::canonicalize(parent)?.join(name);
    let to_wide = extended_path(&target);
    // SAFETY: both paths are valid, NUL-terminated UTF-16 strings for the duration of
    // the call. MoveFileW is same-volume atomic and fails if the destination exists.
    let result = unsafe { move_file_w(from_wide.as_ptr(), to_wide.as_ptr()) };
    if result != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    fs::hard_link(from, to)?;
    let _ = fs::remove_file(from);
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    fs::hard_link(from, to)?;
    let _ = fs::remove_file(from);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_writes_publish_complete_payloads() {
        const WRITERS: usize = 4;
        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("settings"));
        let barrier = Arc::new(Barrier::new(WRITERS));
        let handles: Vec<_> = (0..WRITERS)
            .map(|index| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let payload = vec![index as u8; 64 * 1024];
                    barrier.wait();
                    write_private(&path, &payload).unwrap();
                    payload
                })
            })
            .collect();
        let payloads: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        let actual = fs::read(path.as_ref()).unwrap();
        assert!(payloads
            .iter()
            .any(|payload| payload.as_slice() == actual.as_slice()));
    }

    #[test]
    fn old_extension_temp_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.ini");
        let old_temp = path.with_extension("tmp");
        fs::write(&old_temp, b"preserve me").unwrap();

        write_private(&path, b"new value").unwrap();

        assert_eq!(fs::read(&old_temp).unwrap(), b"preserve me");
        assert_eq!(fs::read(path).unwrap(), b"new value");
    }

    #[cfg(unix)]
    #[test]
    fn old_extension_symlink_is_not_followed() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.ini");
        let victim = dir.path().join("victim");
        let old_temp = path.with_extension("tmp");
        fs::write(&victim, b"preserve me").unwrap();
        symlink(&victim, &old_temp).unwrap();

        write_private(&path, b"new value").unwrap();

        assert_eq!(fs::read(&victim).unwrap(), b"preserve me");
        assert!(fs::symlink_metadata(old_temp)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn failed_replace_cleans_up_unique_temp() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("target");
        fs::create_dir(&target_dir).unwrap();

        assert!(write_private(&target_dir, b"data").is_err());

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(".csm-"))
            .collect();
        assert!(leftovers.is_empty());
    }
}
