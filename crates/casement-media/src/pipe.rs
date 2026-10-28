//! Clipboard data moves through pipes Mutter hands out non-blocking. Read and write them
//! blocking on a worker thread, bounded in size and time.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(3);

fn set_blocking(fd: &OwnedFd) {
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags & !libc::O_NONBLOCK);
        }
    }
}

pub async fn read_all(fd: OwnedFd, limit: usize) -> std::io::Result<Vec<u8>> {
    let task = tokio::task::spawn_blocking(move || {
        set_blocking(&fd);
        let mut bytes = Vec::new();
        std::fs::File::from(fd)
            .take(limit as u64)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    match tokio::time::timeout(TIMEOUT, task).await {
        Ok(Ok(result)) => result,
        _ => Err(std::io::ErrorKind::TimedOut.into()),
    }
}

pub async fn write_all(fd: OwnedFd, data: Vec<u8>) -> bool {
    let task = tokio::task::spawn_blocking(move || {
        set_blocking(&fd);
        std::fs::File::from(fd).write_all(&data).is_ok()
    });
    matches!(tokio::time::timeout(TIMEOUT, task).await, Ok(Ok(true)))
}
