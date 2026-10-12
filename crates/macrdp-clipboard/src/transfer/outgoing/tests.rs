use super::*;

fn request(id: Option<u32>, index: u32) -> FileContentsRequest {
    FileContentsRequest {
        stream_id: 1,
        index,
        flags: FileContentsFlags::DATA,
        position: 0,
        requested_size: 64,
        data_id: id,
    }
}

#[test]
fn locks_preserve_original_copy_and_duplicate_ids_cannot_retarget_it() {
    let directory = tempfile::tempdir().unwrap();
    let a = directory.path().join("Blåbær.txt");
    let b = directory.path().join("new.txt");
    std::fs::write(&a, b"original").unwrap();
    std::fs::write(&b, b"new").unwrap();
    let mut snapshots = OutgoingSnapshots::new(1024);
    snapshots.set_files(vec![a.clone()]);
    snapshots.lock(7);
    snapshots.set_files(vec![b]);
    snapshots.lock(7);
    snapshots.lock(8);
    assert_eq!(
        snapshots.locked_file_list(7).unwrap().files[0].name,
        "Blåbær.txt"
    );
    assert_eq!(snapshots.contents(&request(Some(7), 0)).data(), b"original");
    assert_eq!(snapshots.contents(&request(Some(8), 0)).data(), b"new");
    assert_eq!(snapshots.contents(&request(None, 0)).data(), b"original");
    snapshots.file_list();
    assert_eq!(snapshots.contents(&request(None, 0)).data(), b"new");
    snapshots.unlock(7);
    assert_eq!(
        snapshots.contents(&request(Some(7), 0)),
        FileContentsResponse::new_error(1)
    );
    assert_eq!(
        snapshots.contents(&request(Some(999), 0)),
        FileContentsResponse::new_error(1)
    );
    assert_eq!(snapshots.contents(&request(Some(8), 0)).data(), b"new");
}

#[test]
fn open_handles_never_read_replacement_paths_and_detect_in_place_changes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source");
    std::fs::write(&path, b"original").unwrap();
    let mut snapshots = OutgoingSnapshots::new(1024);
    snapshots.set_files(vec![path.clone()]);
    snapshots.lock(1);
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"replacement").unwrap();
    #[cfg(unix)]
    assert_eq!(
        snapshots.contents(&request(Some(1), 0)),
        FileContentsResponse::new_error(1)
    );
    #[cfg(not(unix))]
    assert_eq!(snapshots.contents(&request(Some(1), 0)).data(), b"original");
    snapshots.set_files(vec![path.clone()]);
    snapshots.lock(2);
    std::fs::write(&path, b"modified").unwrap();
    assert_eq!(
        snapshots.contents(&request(Some(2), 0)),
        FileContentsResponse::new_error(1)
    );
}

#[test]
fn filtering_preserves_descriptor_indices_and_caps_hostile_range_lengths() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    std::fs::write(&first, b"first").unwrap();
    std::fs::write(&second, vec![2; FILE_TRANSFER_CHUNK_SIZE as usize + 1]).unwrap();
    let mut snapshots = OutgoingSnapshots::new(1024 * 1024);
    snapshots.set_files(vec![
        directory.path().join("missing"),
        first,
        directory.path().to_owned(),
        second,
    ]);
    assert_eq!(snapshots.file_list().files.len(), 2);
    assert_eq!(snapshots.contents(&request(None, 0)).data(), b"first");
    let mut range = request(None, 1);
    range.requested_size = u32::MAX;
    assert_eq!(
        snapshots.contents(&range).data().len(),
        FILE_TRANSFER_CHUNK_SIZE as usize
    );
    range.position = u64::MAX;
    assert!(snapshots.contents(&range).data().is_empty());
}

#[cfg(unix)]
#[test]
fn same_size_edits_with_restored_mtime_are_rejected_by_ctime() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source");
    std::fs::write(&path, b"original").unwrap();
    let original_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
    let mut snapshots = OutgoingSnapshots::new(1024);
    snapshots.set_files(vec![path.clone()]);
    snapshots.lock(1);
    std::thread::sleep(std::time::Duration::from_millis(2));
    std::fs::write(&path, b"modified").unwrap();
    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(original_mtime))
        .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        original_mtime
    );
    assert_eq!(
        snapshots.contents(&request(Some(1), 0)),
        FileContentsResponse::new_error(1)
    );
}

#[test]
fn lock_and_pinned_file_limits_are_bounded_and_unlock_releases_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("file");
    std::fs::write(&path, b"x").unwrap();
    let mut snapshots = OutgoingSnapshots::new(1024);
    snapshots.set_files(vec![path.clone(); MAX_PINNED_FILES / 2]);
    for id in 0..MAX_LOCKS as u32 {
        snapshots.lock(id);
    }
    snapshots.lock(1000);
    assert!(snapshots.locked_file_list(1000).is_none());
    assert_eq!(snapshots.pinned_files(), MAX_PINNED_FILES / 2);
    for id in 1..MAX_LOCKS as u32 {
        snapshots.unlock(id);
    }
    snapshots.set_files(vec![path.clone(); MAX_PINNED_FILES / 2]);
    snapshots.lock(1001);
    assert_eq!(snapshots.pinned_files(), MAX_PINNED_FILES);
    snapshots.set_files(vec![path.clone()]);
    assert!(snapshots.file_list().files.is_empty());
    snapshots.unlock(0);
    snapshots.set_files(vec![path.clone()]);
    assert_eq!(snapshots.file_list().files.len(), 1);
    snapshots.set_files(vec![path; MAX_CLIPBOARD_FILES + 1]);
    assert!(snapshots.file_list().files.is_empty());
}

#[cfg(unix)]
#[test]
fn named_pipe_is_rejected_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pipe");
    let native = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(native.as_ptr(), 0o600) }, 0);
    let mut snapshots = OutgoingSnapshots::new(1024);
    snapshots.set_files(vec![path]);
    assert!(snapshots.file_list().files.is_empty());
}
