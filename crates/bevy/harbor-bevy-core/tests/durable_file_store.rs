use std::fs;

#[cfg(unix)]
use std::io;

#[cfg(unix)]
use harbor_bevy_core::DurableFileStore;
use harbor_bevy_core::{FileStore, FileStoreError};

#[cfg(unix)]
#[test]
fn lock_contention_rejects_a_second_writer_until_drop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state");
    let first = DurableFileStore::open(&path).unwrap();

    std::thread::scope(|scope| {
        assert_eq!(
            scope
                .spawn(|| DurableFileStore::open(&path).unwrap_err().kind())
                .join()
                .unwrap(),
            io::ErrorKind::WouldBlock
        );
    });
    drop(first);
    assert!(DurableFileStore::open(&path).is_ok());
}

#[cfg(unix)]
#[test]
fn rejects_opening_another_stores_sidecars_as_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let _store = DurableFileStore::open(dir.path().join("state")).unwrap();
    for sidecar in ["state.lock", "state.next"] {
        assert_eq!(
            DurableFileStore::open(dir.path().join(sidecar))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

#[cfg(unix)]
#[test]
fn replaces_the_file_and_reopens_without_recovering_staged_intent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state");
    let stage = dir.path().join("state.next");
    fs::write(&stage, b"uncommitted").unwrap();
    let mut store = DurableFileStore::open(&path).unwrap();
    assert!(!stage.exists());
    assert_eq!(store.load().unwrap(), None);
    store.commit(b"previous state").unwrap();
    let old = fs::File::open(&path).unwrap();
    store.commit(b"new").unwrap();
    assert!(!stage.exists());
    {
        use std::io::Read;
        let mut bytes = Vec::new();
        (&old).read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"previous state");
    }
    drop(store);
    fs::write(&stage, b"uncommitted").unwrap();
    assert_eq!(
        DurableFileStore::open(&path).unwrap().load().unwrap(),
        Some(b"new".to_vec())
    );
    assert!(!stage.exists());
}

#[cfg(unix)]
#[test]
fn rejects_oversized_commits_and_files_without_poisoning_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state");
    let mut store = DurableFileStore::open(&path).unwrap();
    let bytes = vec![0; DurableFileStore::MAX_BYTES + 1];
    store.commit(&bytes[..DurableFileStore::MAX_BYTES]).unwrap();
    assert_eq!(
        store.commit(&bytes).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(!store.is_uncertain());
    assert_eq!(
        store.load().unwrap().unwrap().len(),
        DurableFileStore::MAX_BYTES
    );
    fs::write(&path, bytes).unwrap();
    assert_eq!(store.load().unwrap_err().kind(), io::ErrorKind::InvalidData);
    store.commit(b"replacement").unwrap();
}

#[cfg(unix)]
#[test]
fn rename_failure_removes_owned_stage_and_allows_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state");
    fs::create_dir(&path).unwrap();
    let mut store = DurableFileStore::open(&path).unwrap();
    assert!(store.commit(b"new").is_err());
    assert!(path.is_dir());
    assert!(!dir.path().join("state.next").exists());
    assert!(!store.is_uncertain());
    fs::remove_dir(&path).unwrap();
    store.commit(b"retry").unwrap();
    assert_eq!(store.load().unwrap().as_deref(), Some(b"retry".as_slice()));
}

#[test]
fn bounded_typed_reads_respect_the_inclusive_limit() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileStore::from_dir(dir.path());
    store.set("key", &42_u32).unwrap();
    let len = fs::metadata(dir.path().join("key.bin")).unwrap().len() as usize;
    assert_eq!(store.get_bounded::<u32>("key", len).unwrap(), 42);
    assert!(matches!(
        store.get_bounded::<u32>("key", len - 1),
        Err(FileStoreError::Io(error)) if error.kind() == io::ErrorKind::InvalidData
    ));
}

#[cfg(unix)]
#[test]
fn clears_stale_stage_symlinks_but_never_follows_or_replaces_a_live_collision() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state");
    let stage = dir.path().join("state.next");
    let target = dir.path().join("target");
    fs::write(&target, b"keep").unwrap();
    symlink(&target, &stage).unwrap();

    let mut store = DurableFileStore::open(&path).unwrap();
    assert!(!stage.is_symlink());
    symlink(&target, &stage).unwrap();
    assert_eq!(
        store.commit(b"replacement").unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert!(stage.is_symlink());
    assert!(!store.is_uncertain());
    drop(store);
    let lock = dir.path().join("state.lock");
    fs::remove_file(&lock).unwrap();
    symlink(&target, lock).unwrap();
    assert_eq!(
        DurableFileStore::open(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(fs::read(target).unwrap(), b"keep");
}
