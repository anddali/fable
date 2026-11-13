//! Scratch manager behaviour: cleanup in all outcomes, startup sweep, quota
//! waits and the per-source size cap (spec D-8, I-2).

use std::time::Duration;

use fable_worker::scratch::MARKER_FILE;
use fable_worker::{ScratchError, ScratchManager};
use tokio::io::AsyncReadExt;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;

/// Entries in the scratch dir, excluding the marker.
fn entries(m: &ScratchManager) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(m.root())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != MARKER_FILE)
        .collect();
    names.sort();
    names
}

async fn manager(max_bytes: u64) -> (tempfile::TempDir, ScratchManager) {
    let tmp = tempfile::tempdir().unwrap();
    let (m, report) = ScratchManager::open_at(tmp.path().join("scratch"), max_bytes)
        .await
        .unwrap();
    assert_eq!(report.entries, 0);
    (tmp, m)
}

#[tokio::test]
async fn file_is_written_readable_and_deleted_on_drop() {
    let (_tmp, m) = manager(MIB).await;
    let mut w = m.create_file("job-1", MIB, None).await.unwrap();
    w.write(b"hello ").await.unwrap();
    w.copy_from(&mut &b"world"[..]).await.unwrap();
    let f = w.finish().await.unwrap();
    assert_eq!(f.len(), 11);
    assert_eq!(f.path(), m.root().join("job-1"));

    let mut s = String::new();
    f.open()
        .await
        .unwrap()
        .read_to_string(&mut s)
        .await
        .unwrap();
    assert_eq!(s, "hello world");
    // Unused reservation is released on finish: 11 bytes still holds 1 KiB.
    assert_eq!(m.available_bytes(), MIB - KIB);

    drop(f);
    assert!(entries(&m).is_empty());
    assert_eq!(m.available_bytes(), MIB);
}

#[tokio::test]
async fn partial_file_is_deleted_when_writer_is_dropped() {
    let (_tmp, m) = manager(MIB).await;
    let mut w = m.create_file("job-1", MIB, None).await.unwrap();
    w.write(&[7; 100]).await.unwrap();
    assert_eq!(entries(&m), ["job-1"]);
    drop(w);
    assert!(entries(&m).is_empty());
    assert_eq!(m.available_bytes(), MIB);
}

#[tokio::test]
async fn file_is_deleted_when_the_job_fails() {
    let (_tmp, m) = manager(MIB).await;
    async fn job(m: &ScratchManager) -> Result<(), String> {
        let mut w = m
            .create_file("job-1", MIB, None)
            .await
            .map_err(|e| e.to_string())?;
        w.write(b"data").await.map_err(|e| e.to_string())?;
        let _f = w.finish().await.map_err(|e| e.to_string())?;
        Err("parse failed".into())
    }
    assert!(job(&m).await.is_err());
    assert!(entries(&m).is_empty());
}

#[tokio::test]
async fn file_is_deleted_when_the_job_panics() {
    let (_tmp, m) = manager(MIB).await;
    let m2 = m.clone();
    let res = tokio::spawn(async move {
        let mut w = m2.create_file("job-1", MIB, None).await.unwrap();
        w.write(b"data").await.unwrap();
        let f = w.finish().await.unwrap();
        assert!(f.path().exists());
        panic!("parser blew up");
    })
    .await;
    assert!(res.unwrap_err().is_panic());
    assert!(entries(&m).is_empty());
    assert_eq!(m.available_bytes(), MIB);
}

#[tokio::test]
async fn size_cap_is_enforced_while_streaming() {
    let (_tmp, m) = manager(MIB).await;
    let mut w = m.create_file("job-1", 10, None).await.unwrap();
    w.write(&[0; 8]).await.unwrap();
    let err = w.write(&[0; 3]).await.unwrap_err();
    assert!(matches!(err, ScratchError::TooLarge { cap: 10 }), "{err}");
    assert_eq!(w.written(), 8, "the overflowing chunk is not written");
    drop(w);

    // Via copy_from: a 1 MiB reader against a 100 KiB cap.
    let mut w = m.create_file("job-2", 100 * KIB, None).await.unwrap();
    let big = vec![1u8; MIB as usize];
    let err = w.copy_from(&mut &big[..]).await.unwrap_err();
    assert!(matches!(err, ScratchError::TooLarge { .. }), "{err}");
    assert!(w.written() <= 100 * KIB);
    let on_disk = std::fs::metadata(w.path()).unwrap().len();
    assert!(on_disk <= 100 * KIB, "never wrote past the cap: {on_disk}");
    drop(w);
    assert!(entries(&m).is_empty());
}

#[tokio::test]
async fn declared_size_above_cap_fails_before_download() {
    let (_tmp, m) = manager(MIB).await;
    let err = m.create_file("job-1", 10, Some(11)).await.unwrap_err();
    assert!(matches!(err, ScratchError::TooLarge { cap: 10 }));
    assert!(!err.is_retryable());
    assert!(entries(&m).is_empty());
}

#[tokio::test]
async fn declared_size_must_match() {
    let (_tmp, m) = manager(MIB).await;
    let mut w = m.create_file("long", MIB, Some(4)).await.unwrap();
    let err = w.write(b"12345").await.unwrap_err();
    assert!(matches!(
        err,
        ScratchError::DeclaredSizeMismatch {
            declared: 4,
            actual: 5
        }
    ));
    drop(w);

    let mut w = m.create_file("short", MIB, Some(4)).await.unwrap();
    w.write(b"123").await.unwrap();
    let err = w.finish().await.unwrap_err();
    assert!(matches!(
        err,
        ScratchError::DeclaredSizeMismatch {
            declared: 4,
            actual: 3
        }
    ));
    assert!(err.is_retryable());
    // finish() consumed the writer: the file is gone either way.
    assert!(entries(&m).is_empty());
    assert_eq!(m.available_bytes(), m.max_bytes());
}

#[tokio::test]
async fn jobs_wait_for_quota_instead_of_failing() {
    let (_tmp, m) = manager(4 * KIB).await;
    let mut a = m.create_file("a", MIB, Some(3 * KIB)).await.unwrap();
    a.write(&[0; 3 * KIB as usize]).await.unwrap();
    let a = a.finish().await.unwrap();
    assert_eq!(m.available_bytes(), KIB);

    // B needs 2 KiB: it must wait, not fail.
    let m2 = m.clone();
    let b = tokio::spawn(async move { m2.create_file("b", MIB, Some(2 * KIB)).await.map(|_| ()) });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!b.is_finished(), "B should be waiting for space");

    drop(a);
    tokio::time::timeout(Duration::from_secs(5), b)
        .await
        .expect("B proceeds once A releases space")
        .unwrap()
        .unwrap();
    assert_eq!(m.available_bytes(), 4 * KIB);
}

#[tokio::test]
async fn unknown_size_reserves_the_cap_clamped_to_quota() {
    let (_tmp, m) = manager(4 * KIB).await;
    // Cap above the quota: reserves the whole quota and fails cleanly past it.
    let mut w = m.create_file("a", MIB, None).await.unwrap();
    assert_eq!(m.available_bytes(), 0);
    w.write(&[0; 4 * KIB as usize]).await.unwrap();
    let err = w.write(b"x").await.unwrap_err();
    assert!(matches!(err, ScratchError::ExceedsQuota { .. }), "{err}");
    drop(w);
    assert_eq!(m.available_bytes(), 4 * KIB);
}

#[tokio::test]
async fn requests_larger_than_the_quota_fail_instead_of_waiting_forever() {
    let (_tmp, m) = manager(4 * KIB).await;
    let err = m.create_file("a", MIB, Some(5 * KIB)).await.unwrap_err();
    assert!(matches!(err, ScratchError::ExceedsQuota { .. }));
    let err = m.create_dir("d", 5 * KIB).await.unwrap_err();
    assert!(matches!(err, ScratchError::ExceedsQuota { .. }));
}

#[tokio::test]
async fn names_are_validated_and_unique() {
    let (_tmp, m) = manager(MIB).await;
    for bad in ["../escape", "a/b", ".fable-scratch", ""] {
        let err = m.create_file(bad, 10, None).await.unwrap_err();
        assert!(matches!(err, ScratchError::InvalidName(_)), "{bad:?}");
    }
    let _a = m.create_file("same", 10, None).await.unwrap();
    let err = m.create_file("same", 10, None).await.unwrap_err();
    assert!(matches!(err, ScratchError::AlreadyExists(_)));
    // The failed attempt released its reservation.
    assert_eq!(m.available_bytes(), MIB - KIB);
}

#[tokio::test]
async fn scratch_dir_is_removed_recursively_on_drop() {
    let (_tmp, m) = manager(MIB).await;
    let d = m.create_dir("clone-1", 512 * KIB).await.unwrap();
    assert_eq!(m.available_bytes(), 512 * KIB);
    std::fs::create_dir_all(d.path().join("a/b")).unwrap();
    std::fs::write(d.path().join("a/b/file"), b"blob").unwrap();
    drop(d);
    assert!(entries(&m).is_empty());
    assert_eq!(m.available_bytes(), MIB);
}

#[tokio::test]
async fn startup_sweep_removes_leftovers() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("scratch");
    {
        let (m, _) = ScratchManager::open_at(&dir, MIB).await.unwrap();
        // Simulate a crash: guards leaked, files left behind.
        let mut w = m.create_file("job-1", MIB, None).await.unwrap();
        w.write(&[1; 300]).await.unwrap();
        std::mem::forget(w.finish().await.unwrap());
        let d = m.create_dir("clone", KIB).await.unwrap();
        std::fs::write(d.path().join("blob"), [2; 200]).unwrap();
        std::mem::forget(d);
    }
    // A symlink must be removed without touching its target.
    let outside = tmp.path().join("outside.txt");
    std::fs::write(&outside, b"keep me").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();

    let (m, report) = ScratchManager::open_at(&dir, MIB).await.unwrap();
    assert_eq!(report.bytes, 500);
    assert_eq!(report.entries, if cfg!(unix) { 3 } else { 2 });
    assert!(entries(&m).is_empty());
    assert!(dir.join(MARKER_FILE).exists());
    assert_eq!(std::fs::read(&outside).unwrap(), b"keep me");
}

#[tokio::test]
async fn refuses_to_sweep_an_unmanaged_non_empty_dir() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("precious.db"), b"data").unwrap();
    let err = ScratchManager::open_at(tmp.path(), MIB).await.unwrap_err();
    assert!(matches!(err, ScratchError::Unmanaged { .. }), "{err}");
    assert!(tmp.path().join("precious.db").exists());
}

#[tokio::test]
async fn refuses_the_filesystem_root() {
    let err = ScratchManager::open_at("/", MIB).await.unwrap_err();
    assert!(matches!(err, ScratchError::RootDir), "{err}");
}

#[cfg(unix)]
#[tokio::test]
async fn scratch_is_private_to_the_process_user() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, m) = manager(MIB).await;
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(m.root()), 0o700);
    let w = m.create_file("f", 10, None).await.unwrap();
    assert_eq!(mode(w.path()), 0o600);
    let d = m.create_dir("d", 10).await.unwrap();
    assert_eq!(mode(d.path()), 0o700);
}
