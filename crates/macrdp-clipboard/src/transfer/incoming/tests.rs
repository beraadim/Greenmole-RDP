use super::*;

fn descriptor(name: &str, size: Option<u64>) -> FileDescriptor {
    FileDescriptor {
        name: name.to_owned(),
        file_size: size,
        attributes: None,
        last_write_time: None,
    }
}

fn manager(directory: &tempfile::TempDir) -> IncomingTransfers {
    IncomingTransfers::new(Some(directory.path().to_owned()), MAX_INCOMING_BYTES)
}

fn start_file(
    manager: &mut IncomingTransfers,
    name: &str,
    size: u64,
    now: Instant,
) -> Vec<FileContentsRequest> {
    manager.start(vec![descriptor(name, Some(size))], now);
    let requests = manager.requests(now);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].flags, FileContentsFlags::SIZE);
    manager.respond(
        &FileContentsResponse::new_size_response(requests[0].stream_id, size),
        now,
    )
}

#[test]
fn twenty_megabyte_transfer_uses_eight_request_window_and_disk_staging() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = manager(&directory);
    let now = Instant::now();
    let size = 20 * 1024 * 1024 + 3;
    let mut requests = VecDeque::from(start_file(&mut manager, "Blåbær #1%.bin", size, now));
    assert_eq!(requests.len(), REQUEST_WINDOW);
    let mut count = 0;
    while let Some(request) = requests.pop_back() {
        assert!(manager.batch.as_ref().unwrap().outstanding.len() <= REQUEST_WINDOW);
        let byte = (request.position / u64::from(FILE_TRANSFER_CHUNK_SIZE)) as u8;
        requests.extend(manager.respond(
            &FileContentsResponse::new_data_response(
                request.stream_id,
                vec![byte; request.requested_size as usize],
            ),
            now,
        ));
        count += 1;
    }
    assert!(count > 256);
    assert!(manager.complete());
    let files = manager.finish();
    assert_eq!(files.len(), 1);
    let bytes = std::fs::read(&files[0]).unwrap();
    assert_eq!(bytes.len() as u64, size);
    for (index, chunk) in bytes.chunks(FILE_TRANSFER_CHUNK_SIZE as usize).enumerate() {
        assert!(chunk.iter().all(|byte| *byte == index as u8));
    }
    assert_eq!(files[0].file_name().unwrap(), "Blåbær #1%.bin");
}

#[test]
fn partial_out_of_order_and_duplicate_responses_preserve_exact_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = manager(&directory);
    let now = Instant::now();
    let requests = start_file(
        &mut manager,
        "partial",
        u64::from(FILE_TRANSFER_CHUNK_SIZE) + 3,
        now,
    );
    let tail = FileContentsResponse::new_data_response(requests[1].stream_id, b"end".to_vec());
    assert!(manager.respond(&tail, now).is_empty());
    assert!(manager.respond(&tail, now).is_empty());
    let prefix = FileContentsResponse::new_data_response(requests[0].stream_id, vec![1; 16_384]);
    let remainder = manager.respond(&prefix, now).remove(0);
    assert_eq!(remainder.position, 16_384);
    assert_eq!(remainder.requested_size, FILE_TRANSFER_CHUNK_SIZE - 16_384);
    assert!(manager.respond(&prefix, now).is_empty());
    manager.respond(
        &FileContentsResponse::new_data_response(
            remainder.stream_id,
            vec![2; remainder.requested_size as usize],
        ),
        now,
    );
    let bytes = std::fs::read(&manager.finish()[0]).unwrap();
    assert!(bytes[..16_384].iter().all(|byte| *byte == 1));
    assert!(bytes[16_384..FILE_TRANSFER_CHUNK_SIZE as usize]
        .iter()
        .all(|byte| *byte == 2));
    assert_eq!(&bytes[FILE_TRANSFER_CHUNK_SIZE as usize..], b"end");
}

#[test]
fn missing_response_expires_without_peer_activity_and_releases_staging() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = manager(&directory);
    let now = Instant::now();
    let requests = start_file(&mut manager, "timeout", 10, now);
    let batch_directory = manager.batch.as_ref().unwrap().directory.path().to_owned();
    manager.expire(now + REQUEST_TIMEOUT);
    assert!(manager.batch.is_none());
    assert!(!batch_directory.exists());
    assert!(manager
        .respond(
            &FileContentsResponse::new_data_response(requests[0].stream_id, vec![1; 10]),
            now + REQUEST_TIMEOUT
        )
        .is_empty());
    assert!(manager.finish().is_empty());
}

#[test]
fn partial_progress_does_not_renew_original_deadline_and_total_deadline_is_bounded() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = manager(&directory);
    let now = Instant::now();
    let requests = start_file(&mut manager, "slow", 10, now);
    manager.respond(
        &FileContentsResponse::new_data_response(requests[0].stream_id, vec![1]),
        now + Duration::from_secs(29),
    );
    manager.expire(now + REQUEST_TIMEOUT);
    assert!(manager.batch.is_none());
    manager.start(vec![descriptor("total", Some(10))], now);
    manager.expire(now + TRANSFER_TIMEOUT);
    assert!(manager.batch.is_none());
}

#[test]
fn replacement_copy_cancels_old_requests_without_stream_id_reuse() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = manager(&directory);
    let now = Instant::now();
    let old = start_file(&mut manager, "old", 5, now).remove(0);
    let old_directory = manager.batch.as_ref().unwrap().directory.path().to_owned();
    let new = start_file(&mut manager, "new", 5, now).remove(0);
    assert_ne!(old.stream_id, new.stream_id);
    assert!(!old_directory.exists());
    assert!(manager
        .respond(
            &FileContentsResponse::new_data_response(old.stream_id, b"wrong".to_vec()),
            now
        )
        .is_empty());
    assert!(!manager.complete());
    manager.respond(
        &FileContentsResponse::new_data_response(new.stream_id, b"right".to_vec()),
        now,
    );
    assert_eq!(std::fs::read(&manager.finish()[0]).unwrap(), b"right");
}

#[test]
fn error_size_mismatch_empty_and_oversized_chunks_never_publish_files() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = manager(&directory);
    let now = Instant::now();
    manager.start(vec![descriptor("wrong-size", Some(5))], now);
    let size_request = manager.requests(now).remove(0);
    assert!(manager
        .respond(
            &FileContentsResponse::new_size_response(size_request.stream_id, 6),
            now
        )
        .is_empty());
    assert!(manager.complete());
    assert!(manager.finish().is_empty());
    for length in [0, 6] {
        let request = start_file(&mut manager, "bad-data", 5, now).remove(0);
        manager.respond(
            &FileContentsResponse::new_data_response(request.stream_id, vec![1; length]),
            now,
        );
        assert!(manager.complete());
        assert!(manager.finish().is_empty());
    }
    let request = start_file(&mut manager, "error", 5, now).remove(0);
    manager.respond(&FileContentsResponse::new_error(request.stream_id), now);
    assert!(manager.complete());
    assert!(manager.finish().is_empty());
}

#[test]
fn response_failure_flag_with_plausible_payload_is_still_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = manager(&directory);
    let now = Instant::now();
    let request = start_file(&mut manager, "failed", 5, now).remove(0);
    let mut encoded = ironrdp_core::encode_vec(&FileContentsResponse::new_data_response(
        request.stream_id,
        b"bytes".to_vec(),
    ))
    .unwrap();
    // Direct FileContentsResponse encoding omits the outer PDU type; flags start at byte 0.
    encoded[..2].copy_from_slice(&2_u16.to_le_bytes());
    let response: FileContentsResponse<'_> = ironrdp_core::decode(&encoded).unwrap();
    assert!(manager.respond(&response, now).is_empty());
    assert!(manager.complete());
    assert!(manager.finish().is_empty());
}

#[test]
fn excessive_short_reads_and_stream_id_exhaustion_are_bounded() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = manager(&directory);
    let now = Instant::now();
    let mut request = start_file(&mut manager, "tiny", 1024, now).remove(0);
    for position in 1..=MAX_RANGE_FOLLOWUPS {
        request = manager
            .respond(
                &FileContentsResponse::new_data_response(request.stream_id, vec![1]),
                now,
            )
            .remove(0);
        assert_eq!(request.position, u64::from(position));
    }
    assert!(manager
        .respond(
            &FileContentsResponse::new_data_response(request.stream_id, vec![1]),
            now
        )
        .is_empty());
    assert!(manager.finish().is_empty());
    manager.next_stream = u64::from(u32::MAX) + 1;
    manager.start(vec![descriptor("exhausted", Some(1))], now);
    assert!(manager.requests(now).is_empty());
    assert!(manager.batch.is_none());
}

#[test]
fn counts_aggregate_session_limits_and_invalid_names_are_rejected_before_data() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = manager(&directory);
    let now = Instant::now();
    manager.start(
        (0..=MAX_CLIPBOARD_FILES)
            .map(|index| descriptor(&index.to_string(), Some(0)))
            .collect(),
        now,
    );
    assert!(manager.batch.is_none());
    manager.start(
        vec![
            descriptor("big", Some(MAX_INCOMING_BYTES)),
            descriptor("extra", Some(1)),
        ],
        now,
    );
    assert_eq!(manager.requests(now).len(), 1);
    assert!(manager.batch.as_ref().unwrap().files[1].state == State::Failed);
    manager.stored_bytes = MAX_SESSION_BYTES;
    manager.start(vec![descriptor("extra", Some(1))], now);
    assert!(manager.requests(now).is_empty());
    assert!(manager.finish().is_empty());
    manager.stored_files = MAX_SESSION_FILES;
    manager.start(vec![descriptor("empty", Some(0))], now);
    assert!(manager.batch.is_none());
    manager.stored_files = 0;
    manager.start(
        [
            "../escape",
            "nested/file",
            "nested\\file",
            "..",
            ".",
            "",
            "nul\0name",
        ]
        .iter()
        .map(|name| descriptor(name, Some(0)))
        .collect(),
        now,
    );
    assert!(manager.requests(now).is_empty());
    assert!(manager.finish().is_empty());
}

#[test]
fn unknown_sizes_are_bounded_and_empty_files_and_batches_preserve_names() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = manager(&directory);
    let now = Instant::now();
    let mut paths = Vec::new();
    for _ in 0..2 {
        manager.start(vec![descriptor("same.txt", None)], now);
        let request = manager.requests(now).remove(0);
        assert!(manager
            .respond(
                &FileContentsResponse::new_size_response(request.stream_id, 0),
                now
            )
            .is_empty());
        paths.extend(manager.finish());
    }
    assert_ne!(paths[0], paths[1]);
    assert_eq!(paths[0].file_name(), paths[1].file_name());
    assert_eq!(std::fs::metadata(&paths[0]).unwrap().len(), 0);
    manager.start(vec![descriptor("unknown", None)], now);
    let request = manager.requests(now).remove(0);
    assert!(manager
        .respond(
            &FileContentsResponse::new_size_response(request.stream_id, MAX_INCOMING_BYTES + 1),
            now
        )
        .is_empty());
    assert!(manager.finish().is_empty());
}

#[test]
fn maximum_file_count_closes_completed_handles_before_batch_publication() {
    let directory = tempfile::tempdir().unwrap();
    let mut manager = manager(&directory);
    let now = Instant::now();
    manager.start(
        (0..MAX_CLIPBOARD_FILES)
            .map(|index| descriptor(&format!("file-{index}"), Some(0)))
            .collect(),
        now,
    );
    let mut requests = VecDeque::from(manager.requests(now));
    while let Some(request) = requests.pop_front() {
        requests.extend(manager.respond(
            &FileContentsResponse::new_size_response(request.stream_id, 0),
            now,
        ));
        assert!(manager
            .batch
            .as_ref()
            .unwrap()
            .files
            .iter()
            .all(|file| file.staging.is_none()));
    }
    assert!(manager.complete());
    assert_eq!(manager.finish().len(), MAX_CLIPBOARD_FILES);
}
