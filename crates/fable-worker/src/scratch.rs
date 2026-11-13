//! Scratch manager (spec §2 D-8, §10.2 I-2).
//!
//! `RAG_SCRATCH_DIR` holds only ephemeral files: in-flight downloads, pending
//! uploads and partial git clones. Nothing in it is ever needed to keep
//! indexed data, so the manager:
//!
//! * **sweeps** the directory at startup ([`ScratchManager::open`]), removing
//!   whatever a previous process left behind (crash, `kill -9`, abort);
//! * hands out entries only through RAII guards ([`ScratchFile`],
//!   [`ScratchWriter`], [`ScratchDir`]) that delete them on drop, so the
//!   bytes go away on success, on error and on panic alike;
//! * enforces the `RAG_SCRATCH_MAX_BYTES` **quota** with a fair (FIFO)
//!   semaphore: a job that needs space it can't get yet *waits* for other
//!   jobs to release theirs instead of failing;
//! * enforces the **per-source size cap** while streaming, so an oversized
//!   source fails after at most `cap` bytes are written, never after the
//!   whole download.
//!
//! # Quota accounting
//!
//! Space is reserved *before* the first byte is written: the declared size
//! when the source reports one (e.g. `Content-Length`), otherwise the
//! per-source cap (clamped to the quota). When a write finishes, the unused
//! part of the reservation is returned straight away. Reserving up front
//! (instead of growing while writing) means two jobs can never deadlock
//! each holding half the quota and waiting for the other.
//!
//! A source that streams more than its declared size fails with
//! [`ScratchError::DeclaredSizeMismatch`] (a protocol error, retryable)
//! rather than growing its reservation.
//!
//! [`ScratchDir`] (git clones) reserves a caller-chosen budget; what the
//! child process writes inside it is not metered byte by byte.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fable_core::config::ScratchConfig;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Accounting unit of the quota semaphore: one permit = 1 KiB. Keeps permit
/// counts well inside `u32` (a single acquire can cover up to 4 TiB).
const UNIT: u64 = 1024;

/// Marker file that identifies a directory as fable scratch space. The
/// startup sweep refuses to empty a non-empty directory without it, so a
/// misconfigured `RAG_SCRATCH_DIR` (e.g. pointing at `/data`) can't wipe it.
pub const MARKER_FILE: &str = ".fable-scratch";

/// Longest accepted entry name.
const MAX_NAME_LEN: usize = 200;

/// Buffer size used by [`ScratchWriter::copy_from`].
const COPY_BUF: usize = 64 * 1024;

/// Errors from the scratch manager.
#[derive(Debug, thiserror::Error)]
pub enum ScratchError {
    /// Filesystem error on a scratch path.
    #[error("scratch I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// Entry names must be a single safe path component.
    #[error("invalid scratch entry name {0:?}")]
    InvalidName(String),
    /// Another live entry already uses this name.
    #[error("scratch entry {0:?} already exists")]
    AlreadyExists(String),
    /// The source is larger than the per-source size cap.
    #[error("source exceeds the size cap of {cap} bytes")]
    TooLarge { cap: u64 },
    /// The request can never fit in `RAG_SCRATCH_MAX_BYTES`.
    #[error("needs {requested} bytes of scratch space but the quota is {max} bytes")]
    ExceedsQuota { requested: u64, max: u64 },
    /// The stream delivered a different number of bytes than it declared.
    #[error("source declared {declared} bytes but delivered {actual}")]
    DeclaredSizeMismatch { declared: u64, actual: u64 },
    /// The scratch dir is not empty and was not created by fable.
    #[error(
        "refusing to sweep {path}: it is not empty and has no {MARKER_FILE} marker; \
         point RAG_SCRATCH_DIR at an empty or dedicated directory"
    )]
    Unmanaged { path: PathBuf },
    /// The scratch dir resolves to the filesystem root.
    #[error("refusing to use the filesystem root as scratch dir")]
    RootDir,
}

impl ScratchError {
    fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// Whether retrying the job later may succeed. Size-cap and quota errors
    /// are permanent for the same source; I/O and stream errors are not.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Io { .. } | Self::DeclaredSizeMismatch { .. } | Self::AlreadyExists(_)
        )
    }
}

/// What the startup sweep removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Top-level entries removed (files, dirs, symlinks).
    pub entries: u64,
    /// Total size of the regular files removed.
    pub bytes: u64,
}

/// Owner of `RAG_SCRATCH_DIR`. Cheap to clone; all clones share the quota.
///
/// Exactly one process may own a scratch dir: the startup sweep deletes
/// everything in it.
#[derive(Clone)]
pub struct ScratchManager {
    inner: Arc<Inner>,
}

struct Inner {
    root: PathBuf,
    max_bytes: u64,
    total_units: u32,
    quota: Arc<Semaphore>,
}

impl fmt::Debug for ScratchManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScratchManager")
            .field("root", &self.inner.root)
            .field("max_bytes", &self.inner.max_bytes)
            .field("available_bytes", &self.available_bytes())
            .finish()
    }
}

impl ScratchManager {
    /// Open the scratch dir from bootstrap config: create it (mode 0700),
    /// sweep leftovers from previous runs and mark it as fable scratch.
    pub async fn open(config: &ScratchConfig) -> Result<(Self, SweepReport), ScratchError> {
        Self::open_at(config.dir.clone(), config.max_bytes).await
    }

    /// [`Self::open`] with explicit settings.
    pub async fn open_at(
        dir: impl Into<PathBuf>,
        max_bytes: u64,
    ) -> Result<(Self, SweepReport), ScratchError> {
        let dir = dir.into();
        let (root, report) = tokio::task::spawn_blocking(move || prepare_and_sweep(&dir))
            .await
            .map_err(|e| ScratchError::io("<scratch sweep>", io::Error::other(e)))??;

        // Floor to whole units (never over-commit), but allow at least one.
        let total_units = u32::try_from((max_bytes / UNIT).max(1)).unwrap_or(u32::MAX);
        let max_bytes = u64::from(total_units) * UNIT;
        let manager = Self {
            inner: Arc::new(Inner {
                root,
                max_bytes,
                total_units,
                quota: Arc::new(Semaphore::new(total_units as usize)),
            }),
        };
        if report.entries > 0 {
            tracing::warn!(
                dir = %manager.root().display(),
                entries = report.entries,
                bytes = report.bytes,
                "removed leftover scratch files from a previous run"
            );
        } else {
            tracing::debug!(dir = %manager.root().display(), "scratch dir ready");
        }
        Ok((manager, report))
    }

    /// The (canonical) scratch directory.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// The effective quota (`RAG_SCRATCH_MAX_BYTES` floored to 1 KiB).
    pub fn max_bytes(&self) -> u64 {
        self.inner.max_bytes
    }

    /// Unreserved space right now.
    pub fn available_bytes(&self) -> u64 {
        self.inner.quota.available_permits() as u64 * UNIT
    }

    /// Create `RAG_SCRATCH_DIR/{name}` for streaming a source into.
    ///
    /// * `cap`: per-source size cap, enforced while writing.
    /// * `declared_size`: the size the source announced, if any. A declared
    ///   size above `cap` fails immediately, before anything is downloaded.
    ///
    /// Waits (FIFO) until enough quota is free. The returned writer deletes
    /// the file if dropped before [`ScratchWriter::finish`]; the finished
    /// [`ScratchFile`] deletes it when dropped.
    pub async fn create_file(
        &self,
        name: &str,
        cap: u64,
        declared_size: Option<u64>,
    ) -> Result<ScratchWriter, ScratchError> {
        validate_name(name)?;
        let reserve = match declared_size {
            Some(size) if size > cap => return Err(ScratchError::TooLarge { cap }),
            Some(size) => size,
            // Unknown size: reserve up to the cap, but never more than the
            // whole quota; the writer fails with ExceedsQuota past that.
            None => cap.min(self.inner.max_bytes),
        };
        let permit = self.reserve(name, reserve).await?;

        let path = self.inner.root.join(name);
        let file = create_new_file(&path).await.map_err(|e| match e.kind() {
            io::ErrorKind::AlreadyExists => ScratchError::AlreadyExists(name.to_owned()),
            _ => ScratchError::io(&path, e),
        })?;
        Ok(ScratchWriter {
            file,
            written: 0,
            cap,
            declared: declared_size,
            reserved: reserve,
            quota_max: self.inner.max_bytes,
            entry: ScratchFile {
                path,
                len: 0,
                permit,
            },
        })
    }

    /// Create the directory `RAG_SCRATCH_DIR/{name}` (mode 0700), reserving
    /// `budget` bytes of quota for whatever gets written into it (e.g. a
    /// partial git clone). Waits (FIFO) for the space. The directory and
    /// everything in it is removed when the guard drops.
    pub async fn create_dir(&self, name: &str, budget: u64) -> Result<ScratchDir, ScratchError> {
        validate_name(name)?;
        let permit = self.reserve(name, budget).await?;
        let path = self.inner.root.join(name);
        create_private_dir(&path)
            .await
            .map_err(|e| match e.kind() {
                io::ErrorKind::AlreadyExists => ScratchError::AlreadyExists(name.to_owned()),
                _ => ScratchError::io(&path, e),
            })?;
        Ok(ScratchDir {
            path,
            budget,
            _permit: permit,
        })
    }

    /// Reserve `bytes` of quota, waiting for space if needed.
    async fn reserve(&self, name: &str, bytes: u64) -> Result<OwnedSemaphorePermit, ScratchError> {
        let units = units_for(bytes);
        if units > u64::from(self.inner.total_units) {
            return Err(ScratchError::ExceedsQuota {
                requested: bytes,
                max: self.inner.max_bytes,
            });
        }
        // Checked above: units <= total_units <= u32::MAX.
        let units = units as u32;
        let quota = Arc::clone(&self.inner.quota);
        if let Ok(permit) = Arc::clone(&quota).try_acquire_many_owned(units) {
            return Ok(permit);
        }
        tracing::info!(
            entry = name,
            requested_bytes = bytes,
            available_bytes = self.available_bytes(),
            "scratch quota full; waiting for space"
        );
        // The semaphore is never closed while `self` is alive.
        let permit = quota
            .acquire_many_owned(units)
            .await
            .expect("scratch quota semaphore is never closed");
        tracing::debug!(entry = name, "scratch space acquired");
        Ok(permit)
    }
}

/// A finished scratch file. Deleted (and its quota released) on drop.
pub struct ScratchFile {
    path: PathBuf,
    len: u64,
    // Dropped after `Drop::drop` removed the file, so the space is only
    // released once it is actually free on disk.
    permit: OwnedSemaphorePermit,
}

impl ScratchFile {
    /// Path to hand to a parser (or a parse-worker child).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Size in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the file is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Open the file for reading.
    pub async fn open(&self) -> Result<tokio::fs::File, ScratchError> {
        tokio::fs::File::open(&self.path)
            .await
            .map_err(|e| ScratchError::io(&self.path, e))
    }
}

impl fmt::Debug for ScratchFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScratchFile")
            .field("path", &self.path)
            .field("len", &self.len)
            .finish()
    }
}

impl Drop for ScratchFile {
    fn drop(&mut self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                path = %self.path.display(),
                error = %e,
                "failed to delete scratch file; the startup sweep will remove it"
            ),
        }
    }
}

/// Streams a source into a scratch file, enforcing the per-source cap, the
/// declared size and the quota. Dropping it without calling
/// [`Self::finish`] deletes the partial file.
pub struct ScratchWriter {
    // Field order matters: the handle closes before `entry` deletes the file.
    file: tokio::fs::File,
    written: u64,
    cap: u64,
    declared: Option<u64>,
    reserved: u64,
    quota_max: u64,
    entry: ScratchFile,
}

impl ScratchWriter {
    /// Path of the file being written.
    pub fn path(&self) -> &Path {
        self.entry.path()
    }

    /// Bytes written so far.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Append `buf`. Fails, without writing, if it would cross the size cap,
    /// the declared size or the reservation.
    pub async fn write(&mut self, buf: &[u8]) -> Result<(), ScratchError> {
        let total = self.written.saturating_add(buf.len() as u64);
        if total > self.cap {
            return Err(ScratchError::TooLarge { cap: self.cap });
        }
        if let Some(declared) = self.declared {
            if total > declared {
                return Err(ScratchError::DeclaredSizeMismatch {
                    declared,
                    actual: total,
                });
            }
        }
        if total > self.reserved {
            // Only reachable without a declared size when cap > quota.
            return Err(ScratchError::ExceedsQuota {
                requested: total,
                max: self.quota_max,
            });
        }
        self.file
            .write_all(buf)
            .await
            .map_err(|e| ScratchError::io(&self.entry.path, e))?;
        self.written = total;
        Ok(())
    }

    /// Copy `reader` to the end of the file, enforcing the same limits as
    /// [`Self::write`]. Returns the number of bytes copied.
    pub async fn copy_from<R>(&mut self, reader: &mut R) -> Result<u64, ScratchError>
    where
        R: AsyncRead + Unpin + ?Sized,
    {
        let mut buf = vec![0u8; COPY_BUF];
        let start = self.written;
        loop {
            let n = reader
                .read(&mut buf)
                .await
                .map_err(|e| ScratchError::io(&self.entry.path, e))?;
            if n == 0 {
                return Ok(self.written - start);
            }
            self.write(&buf[..n]).await?;
        }
    }

    /// Flush, check the declared size and return the finished file. Unused
    /// quota is released immediately.
    pub async fn finish(mut self) -> Result<ScratchFile, ScratchError> {
        if let Some(declared) = self.declared {
            if self.written != declared {
                return Err(ScratchError::DeclaredSizeMismatch {
                    declared,
                    actual: self.written,
                });
            }
        }
        self.file
            .flush()
            .await
            .map_err(|e| ScratchError::io(&self.entry.path, e))?;
        let Self {
            file,
            written,
            mut entry,
            ..
        } = self;
        drop(file);
        entry.len = written;
        let keep = units_for(written) as usize;
        let excess = entry.permit.num_permits().saturating_sub(keep);
        if excess > 0 {
            drop(entry.permit.split(excess));
        }
        Ok(entry)
    }
}

impl fmt::Debug for ScratchWriter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScratchWriter")
            .field("path", &self.entry.path)
            .field("written", &self.written)
            .field("cap", &self.cap)
            .field("declared", &self.declared)
            .finish()
    }
}

/// A scratch directory (e.g. a partial git clone). Removed recursively, and
/// its budget released, on drop.
pub struct ScratchDir {
    path: PathBuf,
    budget: u64,
    _permit: OwnedSemaphorePermit,
}

impl ScratchDir {
    /// The directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The quota reserved for it.
    pub fn budget(&self) -> u64 {
        self.budget
    }
}

impl fmt::Debug for ScratchDir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScratchDir")
            .field("path", &self.path)
            .field("budget", &self.budget)
            .finish()
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        match std::fs::remove_dir_all(&self.path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!(
                path = %self.path.display(),
                error = %e,
                "failed to delete scratch dir; the startup sweep will remove it"
            ),
        }
    }
}

fn units_for(bytes: u64) -> u64 {
    bytes.div_ceil(UNIT)
}

/// Names are a single, non-hidden path component of `[A-Za-z0-9._-]`, so
/// they can't escape the scratch dir or clash with the marker file.
fn validate_name(name: &str) -> Result<(), ScratchError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if ok {
        Ok(())
    } else {
        Err(ScratchError::InvalidName(name.to_owned()))
    }
}

async fn create_new_file(path: &Path) -> io::Result<tokio::fs::File> {
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    opts.mode(0o600);
    opts.open(path).await
}

async fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(path).await
}

/// Create the scratch root if needed, remove everything in it and drop the
/// marker file. Runs on a blocking thread.
fn prepare_and_sweep(dir: &Path) -> Result<(PathBuf, SweepReport), ScratchError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir).map_err(|e| ScratchError::io(dir, e))?;
    let root = std::fs::canonicalize(dir).map_err(|e| ScratchError::io(dir, e))?;
    if root.parent().is_none() {
        return Err(ScratchError::RootDir);
    }

    let marker = root.join(MARKER_FILE);
    let mut leftovers = Vec::new();
    for entry in std::fs::read_dir(&root).map_err(|e| ScratchError::io(&root, e))? {
        let entry = entry.map_err(|e| ScratchError::io(&root, e))?;
        if entry.file_name() != MARKER_FILE {
            leftovers.push(entry.path());
        }
    }
    let managed = marker.exists();
    if !leftovers.is_empty() && !managed {
        return Err(ScratchError::Unmanaged { path: root });
    }

    let mut report = SweepReport::default();
    for path in leftovers {
        report.bytes += remove_entry(&path)?;
        report.entries += 1;
    }
    if !managed {
        std::fs::write(
            &marker,
            b"fable scratch space: everything here is deleted at startup\n",
        )
        .map_err(|e| ScratchError::io(&marker, e))?;
    }
    Ok((root, report))
}

/// Remove a file, symlink (not its target) or directory tree; returns the
/// bytes of regular files removed.
fn remove_entry(path: &Path) -> Result<u64, ScratchError> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| ScratchError::io(path, e))?;
    if meta.is_dir() {
        let bytes = tree_size(path);
        std::fs::remove_dir_all(path).map_err(|e| ScratchError::io(path, e))?;
        Ok(bytes)
    } else {
        std::fs::remove_file(path).map_err(|e| ScratchError::io(path, e))?;
        Ok(if meta.is_file() { meta.len() } else { 0 })
    }
}

/// Best-effort size of regular files under `dir` (symlinks not followed).
fn tree_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_must_be_one_safe_component() {
        for ok in ["job-1", "0193f0c2-7a.pdf", "a_b.c"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            ".fable-scratch",
            "a/b",
            "../x",
            "a\\b",
            "sp ace",
            "ü",
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_name(&"a".repeat(MAX_NAME_LEN + 1)).is_err());
    }

    #[test]
    fn units_round_up() {
        assert_eq!(units_for(0), 0);
        assert_eq!(units_for(1), 1);
        assert_eq!(units_for(1024), 1);
        assert_eq!(units_for(1025), 2);
    }

    #[test]
    fn retryable_classification() {
        assert!(!ScratchError::TooLarge { cap: 1 }.is_retryable());
        assert!(!ScratchError::ExceedsQuota {
            requested: 2,
            max: 1
        }
        .is_retryable());
        assert!(ScratchError::DeclaredSizeMismatch {
            declared: 1,
            actual: 2
        }
        .is_retryable());
    }
}
