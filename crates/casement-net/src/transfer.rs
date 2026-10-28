//! Files between client and host: one bidirectional QUIC stream per request.
//!
//! Upload — client: FileOffer{name, size} → host: FileReply{accepted} → client: the
//! bytes, then FileDone{sha256} → host: FileReply{accepted, message = saved name}.
//! Browse — client: FileList{path} → host: FileListing.
//! Download — client: FileFetch{path} → host: FileOffer{name, size}, the bytes, then
//! FileDone{sha256}.
//! Refusals on browse and download are a Failure. The receiving side writes to a hidden
//! temporary file and renames it only when the size and SHA-256 match, never overwriting
//! an existing file. Browsing and downloading stay inside the host's shared folder.

use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(target_os = "linux")]
use std::{ffi::CString, os::unix::ffi::OsStrExt};

use casement_proto::{
    control::Kind, Control, ErrorCode, FileDone, FileEntry, FileFetch, FileList, FileListing,
    FileOffer, FileReply, Message, MAX_CONTROL_FRAME,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{framing, NetError};

const CHUNK: usize = 256 * 1024;
const IO_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Most entries in one listing; keeps the answer well below the control frame limit.
pub const MAX_LISTING: usize = 4000;
static TEMP_FILE_ID: AtomicU64 = AtomicU64::new(0);

/// A uniquely-created temporary file that is removed if its transfer fails or is
/// cancelled. The file lives beside its destination so publishing it is atomic.
struct TransferTemp {
    path: PathBuf,
}

impl TransferTemp {
    async fn create(dir: &Path) -> Result<(Self, tokio::fs::File), NetError> {
        let dir = dir.to_path_buf();
        // Construct the guard inside the blocking operation: cancellation while opening
        // must not leave an unowned file after the open completes. A short ASCII name
        // also leaves room for long Unicode destination names.
        let (file, guard) = tokio::task::spawn_blocking(move || loop {
            let id = TEMP_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!(
                ".casement-{}-{id}.casement-part",
                std::process::id()
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok::<_, io::Error>((file, Self { path })),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        })
        .await
        .map_err(|e| io::Error::other(e.to_string()))??;
        Ok((guard, tokio::fs::File::from_std(file)))
    }

    /// Publishes without replacing a path created by another transfer or process.
    /// Same-directory no-replace rename works on NTFS, FAT/exFAT and Linux filesystems.
    fn publish(mut self, dir: &Path, name: &str) -> Result<PathBuf, NetError> {
        let (stem, ext) = match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
            _ => (name.to_string(), String::new()),
        };
        for n in 0u64.. {
            let candidate = if n == 0 {
                dir.join(name)
            } else {
                dir.join(format!("{stem} ({n}){ext}"))
            };
            match rename_noreplace(&self.path, &candidate) {
                Ok(()) => {
                    self.remove();
                    return Ok(candidate);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        unreachable!("a free destination name must eventually be found")
    }

    fn remove(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Atomically moves a completed temporary file to an unused destination path.
#[cfg(target_os = "linux")]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
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
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        #[link_name = "MoveFileW"]
        fn move_file_w(existing: *const u16, new: *const u16) -> i32;
    }

    fn extended_path(path: &Path) -> Vec<u16> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        const EXTENDED: &[u16] = &[92, 92, 63, 92]; // \\?\
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

    let from_wide = extended_path(&std::fs::canonicalize(from)?);
    let parent = to
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let name = to
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let target = std::fs::canonicalize(parent)?.join(name);
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
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::hard_link(from, to)?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::hard_link(from, to)?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

impl Drop for TransferTemp {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Where the host keeps files: uploads go to `incoming`, browsing and downloads stay in
/// `shared`; None refuses that direction.
#[derive(Clone, Debug, Default)]
pub struct HostFiles {
    pub incoming: Option<PathBuf>,
    pub shared: Option<PathBuf>,
}

/// What a host-side file stream did, for the host's owner.
pub(crate) enum Served {
    Received(PathBuf, u64),
    Sent(PathBuf, u64),
    Listed,
}

/// The file name to use on the host, or None if nothing usable is left: no directories,
/// no control characters, at most 200 characters.
pub fn safe_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base.chars().filter(|c| !c.is_control()).take(200).collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    (!clean.is_empty()).then_some(clean)
}

/// `dir/name`, or `dir/stem (N).ext` when that exists.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem.to_string(), format!(".{ext}")),
        _ => (name.to_string(), String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

async fn reply(
    send: &mut quinn::SendStream,
    accepted: bool,
    message: &str,
) -> Result<(), NetError> {
    let reply = FileReply {
        accepted,
        message: message.to_string(),
    };
    write_control(send, &Control::new(Kind::FileReply(reply)), IO_IDLE_TIMEOUT).await
}

/// Reads one QUIC read at a time so the idle deadline is restarted by every received byte.
async fn read_exact_progress(
    recv: &mut quinn::RecvStream,
    bytes: &mut [u8],
    idle_timeout: std::time::Duration,
) -> Result<bool, NetError> {
    let mut offset = 0;
    while offset < bytes.len() {
        let read = tokio::time::timeout(idle_timeout, recv.read(&mut bytes[offset..]))
            .await
            .map_err(|_| NetError::Timeout)?
            .map_err(|e| NetError::Stream(e.to_string()))?;
        match read {
            Some(0) => {
                return Err(NetError::Protocol(
                    "the peer closed the stream early".into(),
                ))
            }
            Some(read) => offset += read,
            None if offset == 0 => return Ok(false),
            None => {
                return Err(NetError::Protocol(
                    "the peer closed the stream early".into(),
                ))
            }
        }
    }
    Ok(true)
}

/// Like framing::expect, with a per-read idle deadline rather than a whole-operation cap.
async fn read_control(
    recv: &mut quinn::RecvStream,
    idle_timeout: std::time::Duration,
) -> Result<Option<Control>, NetError> {
    let mut len = [0u8; 4];
    if !read_exact_progress(recv, &mut len, idle_timeout).await? {
        return Ok(None);
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_CONTROL_FRAME {
        return Err(NetError::Protocol(format!("control frame of {len} bytes")));
    }
    let mut body = vec![0u8; len];
    if !read_exact_progress(recv, &mut body, idle_timeout).await? {
        return Err(NetError::Protocol(
            "the peer closed the stream early".into(),
        ));
    }
    Control::decode(body.as_slice())
        .map(Some)
        .map_err(|e| NetError::Protocol(format!("undecodable message: {e}")))
}

async fn expect_control(
    recv: &mut quinn::RecvStream,
    idle_timeout: std::time::Duration,
) -> Result<Control, NetError> {
    read_control(recv, idle_timeout)
        .await?
        .ok_or_else(|| NetError::Protocol("the peer closed the stream".into()))
}

async fn write_all_progress(
    send: &mut quinn::SendStream,
    bytes: &[u8],
    idle_timeout: std::time::Duration,
) -> Result<(), NetError> {
    let mut offset = 0;
    while offset < bytes.len() {
        let written = tokio::time::timeout(idle_timeout, send.write(&bytes[offset..]))
            .await
            .map_err(|_| NetError::Timeout)?
            .map_err(|e| NetError::Stream(e.to_string()))?;
        if written == 0 {
            return Err(NetError::Stream(
                "the peer stopped reading the stream".into(),
            ));
        }
        offset += written;
    }
    Ok(())
}

async fn write_control(
    send: &mut quinn::SendStream,
    control: &Control,
    idle_timeout: std::time::Duration,
) -> Result<(), NetError> {
    let body = control.encode_to_vec();
    if body.len() > MAX_CONTROL_FRAME {
        return Err(NetError::Protocol(format!(
            "control frame of {} bytes",
            body.len()
        )));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    write_all_progress(send, &frame, idle_timeout).await
}

/// Host side: answers one file stream.
pub(crate) async fn serve(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    files: &HostFiles,
) -> Result<Served, NetError> {
    let first = expect_control(&mut recv, IO_IDLE_TIMEOUT).await?.kind;
    let result = match (first, files) {
        (
            Some(Kind::FileOffer(offer)),
            HostFiles {
                incoming: Some(dir),
                ..
            },
        ) => {
            return receive(send, recv, offer, dir)
                .await
                .map(|(p, n)| Served::Received(p, n))
        }
        (Some(Kind::FileOffer(_)), _) => {
            reply(&mut send, false, "this computer does not accept files").await?;
            let _ = send.finish();
            return Err(NetError::Protocol("files are not accepted".into()));
        }
        (
            Some(Kind::FileList(list)),
            HostFiles {
                shared: Some(root), ..
            },
        ) => list_folder(&mut send, root, &list.path)
            .await
            .map(|_| Served::Listed),
        (
            Some(Kind::FileFetch(fetch)),
            HostFiles {
                shared: Some(root), ..
            },
        ) => {
            let _ = send.set_priority(-1);
            send_shared(&mut send, root, &fetch.path).await
        }
        (Some(Kind::FileList(_) | Kind::FileFetch(_)), _) => Err(NetError::Protocol(
            "this computer does not share its files".into(),
        )),
        _ => Err(NetError::Protocol("unexpected first message".into())),
    };
    if let Err(e) = &result {
        let message = match e {
            NetError::Protocol(m) => m.clone(),
            other => other.to_string(),
        };
        let _ = write_control(
            &mut send,
            &Control::failure(ErrorCode::NotAllowed, message),
            IO_IDLE_TIMEOUT,
        )
        .await;
    }
    let _ = send.finish();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), send.stopped()).await;
    result
}

/// `rel` ('/'-separated, relative) inside `root`, with links resolved; refuses anything
/// that ends up outside.
pub(crate) async fn resolve(root: &Path, rel: &str) -> Result<PathBuf, NetError> {
    let outside = || NetError::Protocol("that is outside the shared folder".into());
    let mut path = root.to_path_buf();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(outside()),
            p if p.contains(['\\', '\0']) || (cfg!(windows) && p.contains(':')) => {
                return Err(outside())
            }
            p => path.push(p),
        }
    }
    let missing = |_| NetError::Protocol("no such file or folder".into());
    let real = tokio::fs::canonicalize(&path).await.map_err(missing)?;
    let root = tokio::fs::canonicalize(root).await.map_err(missing)?;
    if !real.starts_with(&root) {
        return Err(outside());
    }
    Ok(real)
}

fn seconds(time: std::io::Result<std::time::SystemTime>) -> u64 {
    time.ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

async fn list_folder(send: &mut quinn::SendStream, root: &Path, rel: &str) -> Result<(), NetError> {
    let dir = resolve(root, rel).await?;
    let real_root = tokio::fs::canonicalize(root).await?;
    let mut read = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| NetError::Protocol(format!("cannot open the folder: {e}")))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    while let Some(entry) = read.next_entry().await? {
        if entries.len() == MAX_LISTING {
            truncated = true;
            break;
        }
        // Follow links so a linked folder opens like a folder; skip broken ones and
        // those that lead out of the shared folder.
        let Ok(meta) = tokio::fs::metadata(entry.path()).await else {
            continue;
        };
        if entry.file_type().await.is_ok_and(|t| t.is_symlink()) {
            match tokio::fs::canonicalize(entry.path()).await {
                Ok(real) if real.starts_with(&real_root) => {}
                _ => continue,
            }
        }
        if !(meta.is_dir() || meta.is_file()) {
            continue;
        }
        entries.push(FileEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir: meta.is_dir(),
            size: if meta.is_file() { meta.len() } else { 0 },
            modified: seconds(meta.modified()),
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let listing = FileListing {
        path: rel.trim_matches('/').to_string(),
        entries,
        truncated,
    };
    framing::write(send, &Control::new(Kind::FileListing(listing))).await
}

async fn send_shared(
    send: &mut quinn::SendStream,
    root: &Path,
    rel: &str,
) -> Result<Served, NetError> {
    let path = resolve(root, rel).await?;
    let mut file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| NetError::Protocol(format!("cannot open the file: {e}")))?;
    let meta = file.metadata().await?;
    if !meta.is_file() {
        return Err(NetError::Protocol("that is a folder, not a file".into()));
    }
    let size = meta.len();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    write_control(
        send,
        &Control::new(Kind::FileOffer(FileOffer { name, size })),
        IO_IDLE_TIMEOUT,
    )
    .await?;
    let sha256 = write_bytes(send, &mut file, size, |_, _| {}).await?;
    write_control(
        send,
        &Control::new(Kind::FileDone(FileDone { sha256 })),
        IO_IDLE_TIMEOUT,
    )
    .await?;
    Ok(Served::Sent(path, size))
}

/// Copies exactly `size` bytes of `file` to the stream; returns their SHA-256.
async fn write_bytes(
    send: &mut quinn::SendStream,
    file: &mut tokio::fs::File,
    size: u64,
    progress: impl FnMut(u64, u64) + Send,
) -> Result<Vec<u8>, NetError> {
    write_bytes_with_timeout(send, file, size, progress, IO_IDLE_TIMEOUT).await
}

async fn write_bytes_with_timeout(
    send: &mut quinn::SendStream,
    file: &mut tokio::fs::File,
    size: u64,
    mut progress: impl FnMut(u64, u64) + Send,
    idle_timeout: std::time::Duration,
) -> Result<Vec<u8>, NetError> {
    let mut hasher = Sha256::new();
    let mut sent = 0u64;
    let mut buf = vec![0u8; CHUNK];
    while sent < size {
        let want = buf
            .len()
            .min(usize::try_from(size - sent).unwrap_or(usize::MAX));
        let got = file.read(&mut buf[..want]).await?;
        if got == 0 {
            return Err(NetError::Protocol(
                "the file changed while it was being sent".into(),
            ));
        }
        hasher.update(&buf[..got]);
        write_all_progress(send, &buf[..got], idle_timeout).await?;
        sent += got as u64;
        progress(sent, size);
    }
    let mut extra = [0u8; 1];
    if file.read(&mut extra).await? != 0 {
        return Err(NetError::Protocol(
            "the file changed while it was being sent".into(),
        ));
    }
    Ok(hasher.finalize().to_vec())
}

/// Reads exactly `size` bytes from the stream into `file`, then the FileDone that must
/// carry their SHA-256.
async fn read_bytes(
    recv: &mut quinn::RecvStream,
    file: &mut tokio::fs::File,
    size: u64,
    progress: impl FnMut(u64, u64) + Send,
) -> Result<(), NetError> {
    read_bytes_with_timeout(recv, file, size, progress, IO_IDLE_TIMEOUT).await
}

async fn read_bytes_with_timeout(
    recv: &mut quinn::RecvStream,
    file: &mut tokio::fs::File,
    size: u64,
    mut progress: impl FnMut(u64, u64) + Send,
    idle_timeout: std::time::Duration,
) -> Result<(), NetError> {
    let mut hasher = Sha256::new();
    let mut left = size;
    let mut buf = vec![0u8; CHUNK];
    while left > 0 {
        let want = buf.len().min(left as usize);
        let got = tokio::time::timeout(idle_timeout, recv.read(&mut buf[..want]))
            .await
            .map_err(|_| NetError::Timeout)?
            .map_err(|e| NetError::Stream(e.to_string()))?
            .ok_or_else(|| NetError::Protocol("the file ended early".into()))?;
        hasher.update(&buf[..got]);
        file.write_all(&buf[..got]).await?;
        left -= got as u64;
        progress(size - left, size);
    }
    file.flush().await?;
    file.sync_all().await?;
    let done = match expect_control(recv, idle_timeout).await?.kind {
        Some(Kind::FileDone(done)) => done,
        _ => return Err(NetError::Protocol("expected the file checksum".into())),
    };
    if done.sha256 != hasher.finalize().as_slice() {
        return Err(NetError::Protocol(
            "the file arrived damaged (checksum differs)".into(),
        ));
    }
    Ok(())
}

/// Host side: receives one offered file into `dir`; returns where it was saved and its
/// size.
async fn receive(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    offer: FileOffer,
    dir: &Path,
) -> Result<(PathBuf, u64), NetError> {
    let Some(name) = safe_name(&offer.name) else {
        reply(&mut send, false, "unusable file name").await?;
        return Err(NetError::Protocol("unusable file name".into()));
    };
    tokio::fs::create_dir_all(dir).await?;
    let (partial, mut file) = TransferTemp::create(dir).await?;
    reply(&mut send, true, "").await?;

    let result: Result<(PathBuf, u64), NetError> = async {
        read_bytes(&mut recv, &mut file, offer.size, |_, _| {}).await?;
        drop(file);
        let path = partial.publish(dir, &name)?;
        Ok((path, offer.size))
    }
    .await;
    match &result {
        Ok((path, _)) => {
            let saved = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            reply(&mut send, true, &saved).await?;
        }
        Err(e) => {
            let _ = reply(&mut send, false, &e.to_string()).await;
        }
    }
    let _ = send.finish();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), send.stopped()).await;
    result
}

/// Client side: sends `path`; `progress(sent, total)` is called as it goes. Returns the
/// name the host saved it under.
pub async fn send(
    connection: &quinn::Connection,
    path: &Path,
    progress: impl FnMut(u64, u64) + Send,
) -> Result<String, NetError> {
    send_with_timeout(connection, path, progress, IO_IDLE_TIMEOUT).await
}

async fn send_with_timeout(
    connection: &quinn::Connection,
    path: &Path,
    progress: impl FnMut(u64, u64) + Send,
    idle_timeout: std::time::Duration,
) -> Result<String, NetError> {
    let mut file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (mut send, mut recv) = tokio::time::timeout(idle_timeout, connection.open_bi())
        .await
        .map_err(|_| NetError::Timeout)??;
    let _ = send.set_priority(-1);
    write_control(
        &mut send,
        &Control::new(Kind::FileOffer(FileOffer { name, size })),
        idle_timeout,
    )
    .await?;
    match expect_control(&mut recv, idle_timeout).await?.kind {
        Some(Kind::FileReply(reply)) if reply.accepted => {}
        Some(Kind::FileReply(reply)) => {
            return Err(NetError::Protocol(format!(
                "the host refused the file: {}",
                reply.message
            )))
        }
        Some(Kind::Failure(failure)) => return Err(NetError::Protocol(failure.message)),
        _ => return Err(NetError::Protocol("expected the host's answer".into())),
    }
    let sha256 =
        write_bytes_with_timeout(&mut send, &mut file, size, progress, idle_timeout).await?;
    write_control(
        &mut send,
        &Control::new(Kind::FileDone(FileDone { sha256 })),
        idle_timeout,
    )
    .await?;
    match expect_control(&mut recv, idle_timeout).await?.kind {
        Some(Kind::FileReply(reply)) if reply.accepted => Ok(reply.message),
        Some(Kind::FileReply(reply)) => Err(NetError::Protocol(reply.message)),
        _ => Err(NetError::Protocol(
            "expected the host's confirmation".into(),
        )),
    }
}

fn refusal(kind: Option<Kind>, expected: &str) -> NetError {
    match kind {
        Some(Kind::Failure(failure)) => NetError::Protocol(failure.message),
        _ => NetError::Protocol(format!("expected {expected}")),
    }
}

/// Client side: the entries of `path` in the host's shared folder.
pub async fn list(connection: &quinn::Connection, path: &str) -> Result<FileListing, NetError> {
    let (mut send, mut recv) = connection.open_bi().await?;
    let request = FileList {
        path: path.to_string(),
    };
    framing::write(&mut send, &Control::new(Kind::FileList(request))).await?;
    let _ = send.finish();
    match framing::expect(&mut recv).await?.kind {
        Some(Kind::FileListing(listing)) => Ok(listing),
        other => Err(refusal(other, "the folder listing")),
    }
}

/// Client side: downloads `path` from the host's shared folder into `dir`, never
/// overwriting; `progress(received, total)` as it goes. Returns where it was saved.
pub async fn fetch(
    connection: &quinn::Connection,
    path: &str,
    dir: &Path,
    progress: impl FnMut(u64, u64) + Send,
) -> Result<PathBuf, NetError> {
    fetch_with_timeout(connection, path, dir, progress, IO_IDLE_TIMEOUT).await
}

async fn fetch_with_timeout(
    connection: &quinn::Connection,
    path: &str,
    dir: &Path,
    progress: impl FnMut(u64, u64) + Send,
    idle_timeout: std::time::Duration,
) -> Result<PathBuf, NetError> {
    let (mut send, mut recv) = tokio::time::timeout(idle_timeout, connection.open_bi())
        .await
        .map_err(|_| NetError::Timeout)??;
    let request = FileFetch {
        path: path.to_string(),
    };
    write_control(
        &mut send,
        &Control::new(Kind::FileFetch(request)),
        idle_timeout,
    )
    .await?;
    let _ = send.finish();
    let offer = match expect_control(&mut recv, idle_timeout).await?.kind {
        Some(Kind::FileOffer(offer)) => offer,
        other => return Err(refusal(other, "the file")),
    };
    let name = safe_name(&offer.name).unwrap_or_else(|| "file".into());
    tokio::fs::create_dir_all(dir).await?;
    let (partial, mut file) = TransferTemp::create(dir).await?;
    let result = async {
        read_bytes_with_timeout(&mut recv, &mut file, offer.size, progress, idle_timeout).await?;
        drop(file);
        let path = partial.publish(dir, &name)?;
        Ok(path)
    }
    .await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::AsyncSeekExt;

    async fn loopback_pair() -> (
        quinn::Endpoint,
        quinn::Endpoint,
        quinn::Connection,
        quinn::Connection,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let server_identity = crate::Identity::load_or_create(&dir.path().join("server")).unwrap();
        let server_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(
            crate::tls::server_config(&server_identity).unwrap(),
        )
        .unwrap();
        let mut server_config = quinn::ServerConfig::with_crypto(Arc::new(server_crypto));
        server_config.transport_config(Arc::new(crate::transport_config()));
        let server =
            quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
        let address = server.local_addr().unwrap();
        let server_accept = server.clone();
        let accepted =
            tokio::spawn(async move { server_accept.accept().await.unwrap().await.unwrap() });

        let client_identity = crate::Identity::load_or_create(&dir.path().join("client")).unwrap();
        let seen = crate::tls::SeenKey::default();
        let client_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(
            crate::tls::client_config(
                &client_identity,
                crate::tls::HostKeyVerifier::new(None, seen),
            )
            .unwrap(),
        )
        .unwrap();
        let mut client_config = quinn::ClientConfig::new(Arc::new(client_crypto));
        client_config.transport_config(Arc::new(crate::transport_config()));
        let client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        client.set_default_client_config(client_config);
        let connection = client
            .connect(address, crate::tls::SERVER_NAME)
            .unwrap()
            .await
            .unwrap();
        let peer = accepted.await.unwrap();
        (client, server, connection, peer)
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(safe_name("../../etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(
            safe_name("C:\\Users\\x\\отчёт.pdf").as_deref(),
            Some("отчёт.pdf")
        );
        assert_eq!(safe_name(".bashrc").as_deref(), Some("bashrc"));
        assert_eq!(safe_name("a\u{7}b.txt").as_deref(), Some("ab.txt"));
        assert_eq!(safe_name(".."), None);
        assert_eq!(safe_name("dir/"), None);
    }

    #[test]
    fn existing_files_are_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unique_path(dir.path(), "a.txt"), dir.path().join("a.txt"));
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "a.txt"),
            dir.path().join("a (1).txt")
        );
        std::fs::write(dir.path().join("a (1).txt"), b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "a.txt"),
            dir.path().join("a (2).txt")
        );
        std::fs::write(dir.path().join("README"), b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "README"),
            dir.path().join("README (1)")
        );
    }

    #[tokio::test]
    async fn publish_never_overwrites_and_allocates_collision_name() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("same.bin");
        std::fs::write(&original, b"keep existing").unwrap();

        let (temp, mut file) = TransferTemp::create(dir.path()).await.unwrap();
        file.write_all(b"incoming").await.unwrap();
        file.sync_all().await.unwrap();
        drop(file);

        let published = temp.publish(dir.path(), "same.bin").unwrap();
        assert_eq!(published, dir.path().join("same (1).bin"));
        assert_eq!(std::fs::read(&original).unwrap(), b"keep existing");
        assert_eq!(std::fs::read(published).unwrap(), b"incoming");
    }

    #[tokio::test]
    async fn stalled_initial_response_times_out() {
        let (_client, _server, connection, peer) = loopback_pair().await;
        let peer_task = tokio::spawn(async move {
            let (_send, mut recv) = peer.accept_bi().await.unwrap();
            assert!(read_control(&mut recv, std::time::Duration::from_secs(1))
                .await
                .unwrap()
                .is_some());
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        });
        let dir = tempfile::tempdir().unwrap();
        let result = fetch_with_timeout(
            &connection,
            "missing.bin",
            dir.path(),
            |_, _| {},
            std::time::Duration::from_millis(100),
        )
        .await;
        assert!(matches!(result, Err(NetError::Timeout)));
        peer_task.await.unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn stalled_body_times_out_and_removes_partial_file() {
        let (_client, _server, connection, peer) = loopback_pair().await;
        let peer_task = tokio::spawn(async move {
            let (mut send, mut recv) = peer.accept_bi().await.unwrap();
            assert!(read_control(&mut recv, std::time::Duration::from_secs(1))
                .await
                .unwrap()
                .is_some());
            write_control(
                &mut send,
                &Control::new(Kind::FileOffer(FileOffer {
                    name: "partial.bin".into(),
                    size: 16,
                })),
                std::time::Duration::from_secs(1),
            )
            .await
            .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        });
        let dir = tempfile::tempdir().unwrap();
        let result = fetch_with_timeout(
            &connection,
            "partial.bin",
            dir.path(),
            |_, _| {},
            std::time::Duration::from_millis(100),
        )
        .await;
        assert!(matches!(result, Err(NetError::Timeout)));
        peer_task.await.unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn slow_transfer_with_regular_progress_exceeds_idle_timeout() {
        let (_client, _server, connection, peer) = loopback_pair().await;
        let (send, mut recv) = connection.open_bi().await.unwrap();
        drop(send);
        let peer_task = tokio::spawn(async move {
            let (mut send, _recv) = peer.accept_bi().await.unwrap();
            let hasher = Sha256::digest(b"abcd");
            for byte in b"abcd" {
                write_all_progress(
                    &mut send,
                    std::slice::from_ref(byte),
                    std::time::Duration::from_secs(1),
                )
                .await
                .unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            }
            write_control(
                &mut send,
                &Control::new(Kind::FileDone(FileDone {
                    sha256: hasher.to_vec(),
                })),
                std::time::Duration::from_secs(1),
            )
            .await
            .unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let mut file = tokio::fs::File::create(dir.path().join("slow.bin"))
            .await
            .unwrap();
        let result = read_bytes_with_timeout(
            &mut recv,
            &mut file,
            4,
            |_, _| {},
            std::time::Duration::from_millis(100),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(std::fs::read(dir.path().join("slow.bin")).unwrap(), b"abcd");
        peer_task.await.unwrap();
    }

    #[tokio::test]
    async fn stalled_final_upload_reply_times_out() {
        let (_client, _server, connection, peer) = loopback_pair().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("upload.bin");
        std::fs::write(&path, b"upload").unwrap();
        let peer_task = tokio::spawn(async move {
            let (mut send, mut recv) = peer.accept_bi().await.unwrap();
            let offer = read_control(&mut recv, std::time::Duration::from_secs(1))
                .await
                .unwrap()
                .unwrap();
            let size = match offer.kind.unwrap() {
                Kind::FileOffer(offer) => offer.size,
                other => panic!("expected file offer, got {other:?}"),
            };
            write_control(
                &mut send,
                &Control::new(Kind::FileReply(FileReply {
                    accepted: true,
                    message: String::new(),
                })),
                std::time::Duration::from_secs(1),
            )
            .await
            .unwrap();
            let mut bytes = vec![0; size as usize];
            assert!(
                read_exact_progress(&mut recv, &mut bytes, std::time::Duration::from_secs(1),)
                    .await
                    .unwrap()
            );
            assert!(matches!(
                read_control(&mut recv, std::time::Duration::from_secs(1))
                    .await
                    .unwrap()
                    .unwrap()
                    .kind,
                Some(Kind::FileDone(_))
            ));
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        });
        let result = send_with_timeout(
            &connection,
            &path,
            |_, _| {},
            std::time::Duration::from_millis(100),
        )
        .await;
        assert!(matches!(result, Err(NetError::Timeout)));
        peer_task.await.unwrap();
    }

    #[tokio::test]
    async fn sending_stops_at_declared_size_and_rejects_an_extra_byte() {
        let (_client, _server, connection, peer) = loopback_pair().await;
        let (mut send, _recv) = connection.open_bi().await.unwrap();
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
        let (bytes_tx, bytes_rx) = tokio::sync::oneshot::channel();
        let peer_task = tokio::spawn(async move {
            let (_peer_send, mut peer_recv) = peer.accept_bi().await.unwrap();
            let mut bytes = [0u8; 4];
            assert!(read_exact_progress(
                &mut peer_recv,
                &mut bytes,
                std::time::Duration::from_secs(1),
            )
            .await
            .unwrap());
            bytes_tx.send(bytes).unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("growing.bin");
        let mut bytes = b"head".to_vec();
        bytes.resize(1024 * 1024, b'x');
        std::fs::write(&path, bytes).unwrap();
        let mut file = tokio::fs::File::open(path).await.unwrap();
        let result = write_bytes_with_timeout(
            &mut send,
            &mut file,
            4,
            move |sent, total| progress_tx.send((sent, total)).unwrap(),
            std::time::Duration::from_secs(1),
        )
        .await;
        assert!(matches!(result, Err(NetError::Protocol(_))));
        assert_eq!(bytes_rx.await.unwrap(), *b"head");
        assert_eq!(file.stream_position().await.unwrap(), 5);
        peer_task.await.unwrap();
        let mut progress_seen = false;
        while let Ok((sent, total)) = progress_rx.try_recv() {
            progress_seen = true;
            assert!(sent <= total);
            assert_eq!(total, 4);
        }
        assert!(progress_seen);
    }

    #[tokio::test]
    async fn sending_detects_a_truncated_source() {
        let (_client, _server, connection, peer) = loopback_pair().await;
        let (mut send, _recv) = connection.open_bi().await.unwrap();
        let (bytes_tx, bytes_rx) = tokio::sync::oneshot::channel();
        let peer_task = tokio::spawn(async move {
            let (_peer_send, mut peer_recv) = peer.accept_bi().await.unwrap();
            let mut bytes = Vec::new();
            let mut buf = [0u8; 8];
            loop {
                match peer_recv.read(&mut buf).await {
                    Ok(Some(n)) => bytes.extend_from_slice(&buf[..n]),
                    Ok(None) | Err(_) => break,
                }
            }
            bytes_tx.send(bytes).unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.bin");
        std::fs::write(&path, b"short").unwrap();
        let mut file = tokio::fs::File::open(path).await.unwrap();
        let result = write_bytes_with_timeout(
            &mut send,
            &mut file,
            10,
            |_, _| {},
            std::time::Duration::from_secs(1),
        )
        .await;
        assert!(matches!(result, Err(NetError::Protocol(_))));
        drop(send);
        assert_eq!(bytes_rx.await.unwrap(), b"short");
        peer_task.await.unwrap();
    }
}
