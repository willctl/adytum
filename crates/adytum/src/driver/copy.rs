//! Copies between this machine and a cell, for drivers that reach a cell's
//! programs through stdin and stdout: the receiving program runs in the
//! cell as the copy's owner, so it writes only where that user may.

use std::path::Path;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
#[cfg(feature = "container")]
use tokio::io::{AsyncRead, AsyncWrite};

use crate::exec::{tail, Command, Output, Runner};
use crate::Error;

/// The most a copy out of a cell carries; more is refused, so a cell
/// cannot fill this machine's disk through one.
pub const MAX_COPY: u64 = 8 * 1024 * 1024 * 1024;

pub(crate) fn too_big() -> Error {
    Error::Unsupported { what: format!("copying more than {} GiB out of a cell", MAX_COPY / (1024 * 1024 * 1024)) }
}

pub(crate) fn absolute(host: &Path) -> Result<&Path, Error> {
    if host.is_absolute() {
        Ok(host)
    } else {
        Err(Error::BadPath { path: host.display().to_string(), rule: "a host path must be absolute" })
    }
}

/// The folder's entries, not the folder, so unpacking never changes the
/// mode of a folder that exists. No extended attributes or AppleDouble files.
pub(crate) fn host_tar(folder: &Path) -> Result<Option<Command>, Error> {
    let entries = std::fs::read_dir(folder).map_err(|source| Error::Io { what: "read the folder to copy", source })?;
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| Error::Io { what: "read the folder to copy", source })?;
        names.push(Path::new(".").join(entry.file_name()));
    }
    if names.is_empty() {
        return Ok(None);
    }
    names.sort();
    Ok(Some(Command::new("tar").args(["-c", "-f", "-", "--no-xattrs", "--no-acls", "-C"]).arg(folder).args(names).env("COPYFILE_DISABLE", "1").pass_env(&["PATH"])))
}

fn strs(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

/// How a driver reaches the cell for one copy: `as_owner` turns a guest
/// command into one that runs there as the copy's owner, through `remote`;
/// the host's tar runs through `local`. `fail` names what failed.
pub(crate) struct Channel<'a> {
    pub(crate) local: &'a dyn Runner,
    pub(crate) remote: &'a dyn Runner,
    pub(crate) as_owner: &'a (dyn Fn(Vec<String>) -> Command + Sync),
    pub(crate) fail: &'a (dyn Fn(&str, &Output) -> Error + Sync),
    pub(crate) quick: Duration,
    pub(crate) deadline: Duration,
}

/// A file lands at `guest`, a folder's contents in the folder `guest`, with
/// missing folders on the way made. sudo's umask and tar's
/// `--no-same-permissions` keep anything from landing writable by group or
/// others; a busybox tar applies the umask too.
pub(crate) async fn send(ch: &Channel<'_>, host: &Path, guest: &str) -> Result<(), Error> {
    let host = absolute(host)?;
    let is_dir = std::fs::metadata(host).map_err(|source| Error::Io { what: "read what is to be copied", source })?.is_dir();
    let tar = if is_dir { host_tar(host)? } else { None };
    let (folder, receive) = if is_dir {
        (guest.to_owned(), strs(&["tar", "-x", "-f", "-", "--no-same-owner", "--no-same-permissions", "-C", guest]))
    } else {
        let parent = Path::new(guest).parent().map_or_else(|| "/".to_owned(), |p| p.to_string_lossy().into_owned());
        (parent, vec!["dd".to_owned(), format!("of={guest}"), "bs=1M".into(), "status=none".into()])
    };
    let mkdir = (ch.as_owner)(strs(&["mkdir", "-p", "--", &folder])).deadline(ch.quick);
    let made = ch.remote.run(&mkdir).await?;
    if !made.success() {
        return Err((ch.fail)("mkdir in the cell", &made));
    }
    if is_dir && tar.is_none() {
        return Ok(());
    }
    let mut into = ch.remote.duplex(&(ch.as_owner)(receive).deadline(ch.deadline)).await?;
    let mut sender = None;
    let pumped = tokio::time::timeout(ch.deadline, async {
        if let Some(tar) = &tar {
            let tar = sender.insert(ch.local.duplex(tar).await?);
            tokio::io::copy(tar, &mut into).await.map_err(|source| Error::Io { what: "send the folder into the cell", source })?;
        } else {
            let mut file = tokio::fs::File::open(host).await.map_err(|source| Error::Io { what: "open the file to copy", source })?;
            tokio::io::copy(&mut file, &mut into).await.map_err(|source| Error::Io { what: "send the file into the cell", source })?;
        }
        into.shutdown().await.map_err(|source| Error::Io { what: "close the copy into the cell", source })
    })
    .await;
    let timed_out = pumped.is_err();
    if timed_out {
        into.kill();
        if let Some(tar) = sender.as_mut() {
            tar.kill();
        }
    }
    let received = into.finish().await?;
    if let Some(tar) = sender {
        let sent = tar.finish().await?;
        if !sent.success() && !timed_out {
            return Err(Error::Tool { what: "tar on the host".to_owned(), status: sent.status, stderr: tail(&sent.stderr_lossy()) });
        }
    }
    if timed_out {
        return Err(late("the copy into the cell"));
    }
    if !received.success() {
        return Err((ch.fail)("the copy into the cell", &received));
    }
    pumped.unwrap_or(Ok(()))
}

fn late(what: &str) -> Error {
    Error::Tool { what: what.to_owned(), status: None, stderr: "It did not finish before its deadline and was stopped.".to_owned() }
}

#[cfg(feature = "container")]
/// The guest's file lands at `host`; a folder's contents land in the folder
/// `host`, made if missing. The guest side reads as the owner `as_owner`
/// gives, so it gets only what that user may read, and the host's tar
/// refuses entries that climb out of `host`.
pub(crate) async fn fetch(ch: &Channel<'_>, guest: &str, host: &Path) -> Result<(), Error> {
    let host = absolute(host)?;
    let probe = (ch.as_owner)(strs(&["test", "-d", guest])).deadline(ch.quick);
    let probe = ch.remote.run(&probe).await?;
    let is_dir = match probe.status {
        Some(0) => true,
        Some(1) => false,
        _ => return Err((ch.fail)("test in the cell", &probe)),
    };
    let mut from = if is_dir {
        ch.remote.duplex(&(ch.as_owner)(strs(&["tar", "-c", "-f", "-", "-C", guest, "."])).deadline(ch.deadline)).await?
    } else {
        ch.remote.duplex(&(ch.as_owner)(strs(&["cat", guest])).deadline(ch.deadline)).await?
    };
    from.shutdown().await.map_err(|source| Error::Io { what: "close the copy's input", source })?;
    let mut unpack = None;
    let pumped = tokio::time::timeout(ch.deadline, async {
        if is_dir {
            std::fs::create_dir_all(host).map_err(|source| Error::Io { what: "make the folder to copy into", source })?;
            let tar = Command::new("tar").args(["-x", "-f", "-", "-C"]).arg(host).pass_env(&["PATH"]);
            let tar = unpack.insert(ch.local.duplex(&tar).await?);
            pump(&mut from, tar).await
        } else {
            let mut file = tokio::fs::File::create(host).await.map_err(|source| Error::Io { what: "make the file to copy into", source })?;
            pump(&mut from, &mut file).await
        }
    })
    .await;
    let timed_out = pumped.is_err();
    if timed_out {
        from.kill();
        if let Some(tar) = unpack.as_mut() {
            tar.kill();
        }
    }
    let sent = from.finish().await?;
    if let Some(tar) = unpack {
        let unpacked = tar.finish().await?;
        if !unpacked.success() && !timed_out {
            return Err(Error::Tool { what: "tar on the host".to_owned(), status: unpacked.status, stderr: tail(&unpacked.stderr_lossy()) });
        }
    }
    if timed_out {
        return Err(late("the copy out of the cell"));
    }
    if !sent.success() {
        return Err((ch.fail)("the copy out of the cell", &sent));
    }
    pumped.unwrap_or(Ok(()))
}

#[cfg(feature = "container")]
async fn pump<W: AsyncWrite + Unpin>(from: &mut (impl AsyncRead + Unpin), to: &mut W) -> Result<(), Error> {
    use tokio::io::AsyncReadExt as _;
    let carried = tokio::io::copy(&mut from.take(MAX_COPY + 1), to).await.map_err(|source| Error::Io { what: "carry the copy out of the cell", source })?;
    if carried > MAX_COPY {
        return Err(too_big());
    }
    to.shutdown().await.map_err(|source| Error::Io { what: "finish the copy out of the cell", source })
}
