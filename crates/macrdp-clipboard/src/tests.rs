use super::*;

fn test_backend(
    sender: ServerEventTx,
    directory: PathBuf,
    max_file_size: u64,
    file_transfer: bool,
) -> ClipboardWorker {
    let pb = PasteboardBridge::isolated();
    let shared = Arc::new(PublicationState::new(pb.change_count()));
    ClipboardWorker::new(sender, directory, max_file_size, file_transfer, shared, pb)
}

fn pending(backend: &ClipboardWorker, id: ClipboardFormatId, kind: FormatKind) -> RemoteOffer {
    RemoteOffer {
        format: Some(crate::remote_formats::RemoteFormat { id, kind }),
        text_fallback: None,
        token: backend.shared.remote_copy(&backend.pasteboard),
    }
}

#[test]
fn malformed_images_unsolicited_responses_and_errors_do_not_touch_pasteboard() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 1024, false);
    let initial = backend.pasteboard.change_count();
    backend.pending_paste_format = Some(pending(
        &backend,
        ClipboardFormatId::CF_DIB,
        FormatKind::Dib,
    ));
    backend.on_format_data_response(FormatDataResponse::new_data(vec![0; 40]));
    assert!(backend.pending_paste_format.is_none());
    backend.on_format_data_response(FormatDataResponse::new_data(vec![65, 0, 0, 0]));
    backend.pending_paste_format = Some(pending(
        &backend,
        ClipboardFormatId::CF_DIB,
        FormatKind::Dib,
    ));
    backend.on_format_data_response(FormatDataResponse::new_error());
    assert!(backend.pending_paste_format.is_none());
    assert_eq!(backend.pasteboard.change_count(), initial);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn short_file_response_schedules_bounded_remainder_on_clipboard_channel() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 1024, true);
    backend
        .transfer_manager
        .set_incoming_descriptors(vec![FileDescriptor {
            name: "test.txt".to_string(),
            file_size: Some(5),
            attributes: None,
            last_write_time: None,
        }]);
    let size_request = backend
        .transfer_manager
        .generate_contents_requests()
        .remove(0);
    let requests = backend.transfer_manager.handle_contents_response(
        &FileContentsResponse::new_size_response(size_request.stream_id, 5),
    );
    backend.on_file_contents_response(FileContentsResponse::new_data_response(
        requests[0].stream_id,
        b"he".to_vec(),
    ));
    let ServerEvent::ClipboardFileContentsRequest(remainder) = receiver.try_recv().unwrap() else {
        panic!("Expected missing-range request");
    };
    assert_eq!(remainder.position, 2);
    assert_eq!(remainder.requested_size, 3);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn disabling_file_transfer_disables_capability_and_ignores_remote_file_copy() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 1024, false);
    assert!(!backend
        .client_capabilities()
        .contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED));
    backend.on_remote_copy(&[
        ClipboardFormat::new(ClipboardFormatId::new(FORMAT_ID_FILE_LIST))
            .with_name(ClipboardFormatName::FILE_LIST),
    ]);
    assert!(receiver.try_recv().is_err());
    assert!(backend.pending_paste_format.is_none());
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::new(13))]);
    assert_eq!(
        backend.pending_paste_format.and_then(|offer| offer.format),
        Some(crate::remote_formats::RemoteFormat {
            id: ClipboardFormatId::CF_UNICODETEXT,
            kind: FormatKind::Text
        })
    );
    assert!(receiver.try_recv().is_ok());
}

fn offered_format(id: u32, name: ClipboardFormatName) -> ClipboardFormat {
    ClipboardFormat::new(ClipboardFormatId::new(id)).with_name(name)
}

#[test]
fn requests_peer_file_id_and_decodes_its_descriptor_response_as_files() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 1024, true);
    // Deliberately collide with our HTML ID: the peer's name determines the kind.
    backend.on_remote_copy(&[offered_format(
        FORMAT_ID_HTML,
        ClipboardFormatName::FILE_LIST,
    )]);
    let ServerEvent::Clipboard(ClipboardMessage::SendInitiatePaste(id)) =
        receiver.try_recv().unwrap()
    else {
        panic!("Expected a format data request");
    };
    assert_eq!(id.value(), FORMAT_ID_HTML);
    let list = PackedFileList {
        files: vec![FileDescriptor {
            name: "Blåbær.txt".into(),
            file_size: Some(5),
            attributes: None,
            last_write_time: None,
        }],
    };
    backend.on_format_data_response(FormatDataResponse::new_file_list(&list).unwrap());
    assert!(backend.pending_paste_format.is_none());
    let ServerEvent::ClipboardFileContentsRequest(size) = receiver.try_recv().unwrap() else {
        panic!("Expected a file size request");
    };
    assert!(size.flags.contains(FileContentsFlags::SIZE));
    assert!(receiver.try_recv().is_err());
    backend.on_file_contents_response(FileContentsResponse::new_size_response(size.stream_id, 5));
    let ServerEvent::ClipboardFileContentsRequest(data) = receiver.try_recv().unwrap() else {
        panic!("Expected a file contents request");
    };
    assert!(data.flags.contains(FileContentsFlags::DATA));
    assert_eq!(data.requested_size, 5);
    assert!(receiver.try_recv().is_err());
    assert!(backend.pasteboard.available_types().is_empty());
}

#[test]
fn coalesces_new_offers_without_misinterpreting_or_publishing_old_responses() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 1024, true);
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
    receiver.try_recv().unwrap();
    backend.on_remote_copy(&[offered_format(0xc001, ClipboardFormatName::FILE_LIST)]);
    backend.on_remote_copy(&[offered_format(0xc002, ClipboardFormatName::HTML)]);
    assert!(
        receiver.try_recv().is_err(),
        "Only one request may be in flight"
    );
    // A valid old text payload would touch the general clipboard if not discarded.
    backend.on_format_data_response(FormatDataResponse::new_unicode_string("old copy"));
    let ServerEvent::Clipboard(ClipboardMessage::SendInitiatePaste(id)) =
        receiver.try_recv().unwrap()
    else {
        panic!("Expected the newest offer to be requested");
    };
    assert_eq!(id.value(), 0xc002);
    assert_eq!(
        backend.pending_paste_format.unwrap().format.unwrap().kind,
        FormatKind::Html
    );
    backend.on_format_data_response(FormatDataResponse::new_error());
    assert!(backend.pending_paste_format.is_none());
    assert!(backend.queued_paste_format.is_none());
    assert!(backend.pasteboard.available_types().is_empty());
}

#[test]
fn empty_offer_cancels_pending_publication_and_incomplete_file_transfers() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 1024, true);
    backend.on_remote_copy(&[offered_format(0xc001, ClipboardFormatName::FILE_LIST)]);
    receiver.try_recv().unwrap();
    let list = PackedFileList {
        files: vec![FileDescriptor {
            name: "old.txt".into(),
            file_size: Some(5),
            attributes: None,
            last_write_time: None,
        }],
    };
    backend.on_format_data_response(FormatDataResponse::new_file_list(&list).unwrap());
    let ServerEvent::ClipboardFileContentsRequest(request) = receiver.try_recv().unwrap() else {
        panic!("Expected a file size request");
    };
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
    receiver.try_recv().unwrap();
    backend.on_remote_copy(&[]);
    backend.on_file_contents_response(FileContentsResponse::new_data_response(
        request.stream_id,
        b"hello".to_vec(),
    ));
    backend.on_format_data_response(FormatDataResponse::new_unicode_string("old text"));
    assert!(backend.pending_paste_format.is_none());
    assert!(backend.queued_paste_format.is_none());
    assert!(!backend.transfer_manager.all_incoming_complete());
    assert!(receiver.try_recv().is_err());
    assert!(backend.pasteboard.available_types().is_empty());
}

#[test]
fn failed_send_does_not_leave_an_unanswerable_pending_request() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, receiver) = ServerEvent::create_channel();
    drop(receiver);
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 1024, true);
    backend.on_remote_copy(&[offered_format(0xc001, ClipboardFormatName::HTML)]);
    assert!(backend.pending_paste_format.is_none());
}

#[test]
fn hostile_file_list_count_is_rejected_before_decoding_or_requesting_files() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 1024, true);
    backend.on_remote_copy(&[offered_format(0xc001, ClipboardFormatName::FILE_LIST)]);
    receiver.try_recv().unwrap();
    backend.on_format_data_response(FormatDataResponse::new_data(
        u32::MAX.to_le_bytes().to_vec(),
    ));
    assert!(backend.pending_paste_format.is_none());
    assert!(receiver.try_recv().is_err());
    assert!(backend.pasteboard.available_types().is_empty());
}

#[test]
fn large_file_copy_enqueues_only_the_request_window_without_closing_session_queue() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 32 * 1024 * 1024, true);
    backend.on_remote_copy(&[offered_format(0xc001, ClipboardFormatName::FILE_LIST)]);
    receiver.try_recv().unwrap();
    let list = PackedFileList {
        files: vec![FileDescriptor {
            name: "large.bin".to_owned(),
            file_size: Some(20 * 1024 * 1024),
            attributes: None,
            last_write_time: None,
        }],
    };
    backend.on_format_data_response(FormatDataResponse::new_file_list(&list).unwrap());
    let ServerEvent::ClipboardFileContentsRequest(size) = receiver.try_recv().unwrap() else {
        panic!("Expected size request")
    };
    assert!(receiver.try_recv().is_err());
    backend.on_file_contents_response(FileContentsResponse::new_size_response(
        size.stream_id,
        20 * 1024 * 1024,
    ));
    let mut count = 0;
    while let Ok(event) = receiver.try_recv() {
        let ServerEvent::ClipboardFileContentsRequest(request) = event else {
            panic!("Expected data request")
        };
        assert_eq!(
            request.requested_size,
            crate::file::FILE_TRANSFER_CHUNK_SIZE
        );
        count += 1;
    }
    assert_eq!(count, 8);
    assert!(!backend.event_sender.is_closed());
    assert!(backend.pasteboard.available_types().is_empty());
}

#[test]
fn tagged_contents_consumes_lock_association_and_unlock_keeps_incoming_transfer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source");
    std::fs::write(&path, b"source").unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 1024, true);
    backend.transfer_manager.set_local_files(vec![path]);
    backend.transfer_manager.build_file_list();
    backend.transfer_manager.lock(7);
    backend.pending_descriptor_lock = Some(7);
    backend
        .transfer_manager
        .set_incoming_descriptors(vec![FileDescriptor {
            name: "incoming".to_owned(),
            file_size: Some(5),
            attributes: None,
            last_write_time: None,
        }]);
    backend.on_file_contents_request(FileContentsRequest {
        stream_id: 42,
        index: 0,
        flags: FileContentsFlags::DATA,
        position: 0,
        requested_size: 6,
        data_id: Some(7),
    });
    assert!(backend.pending_descriptor_lock.is_none());
    let ServerEvent::ClipboardFileContents(response) = receiver.try_recv().unwrap() else {
        panic!("Expected contents response")
    };
    assert_eq!(response.data(), b"source");
    backend.on_unlock(LockDataId(7));
    assert_eq!(
        backend.transfer_manager.generate_contents_requests().len(),
        1
    );
}

#[test]
fn local_copy_before_text_response_is_preserved_and_next_remote_copy_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 1024, false);
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
    receiver.try_recv().unwrap();
    backend.pasteboard.write_string("new local copy");
    backend.on_format_data_response(FormatDataResponse::new_unicode_string("stale remote"));
    assert_eq!(
        backend.pasteboard.read_string().as_deref(),
        Some("new local copy")
    );
    assert!(backend.pending_paste_format.is_none());

    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
    receiver.try_recv().unwrap();
    backend.on_format_data_response(FormatDataResponse::new_unicode_string("new remote copy"));
    assert_eq!(
        backend.pasteboard.read_string().as_deref(),
        Some("new remote copy")
    );
    assert!(!backend.shared.local_copy_changed(&backend.pasteboard));
}

#[test]
fn local_copy_supersedes_queued_remote_offer_without_issuing_another_request() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 1024, true);
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
    receiver.try_recv().unwrap();
    backend.on_remote_copy(&[offered_format(0xc002, ClipboardFormatName::HTML)]);
    backend.pasteboard.write_string("latest local");
    backend.on_format_data_response(FormatDataResponse::new_unicode_string("older remote"));
    assert!(receiver.try_recv().is_err());
    assert!(backend.pending_paste_format.is_none());
    assert_eq!(
        backend.pasteboard.read_string().as_deref(),
        Some("latest local")
    );
}

#[test]
fn local_copy_before_descriptor_response_does_not_start_obsolete_download() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 1024, true);
    backend.on_remote_copy(&[offered_format(0xc001, ClipboardFormatName::FILE_LIST)]);
    receiver.try_recv().unwrap();
    backend.pasteboard.write_string("keep this");
    let list = PackedFileList {
        files: vec![FileDescriptor {
            name: "stale.txt".into(),
            file_size: Some(1),
            attributes: None,
            last_write_time: None,
        }],
    };
    backend.on_format_data_response(FormatDataResponse::new_file_list(&list).unwrap());
    assert!(receiver.try_recv().is_err());
    assert!(backend.incoming_token.is_none());
    assert_eq!(
        backend.pasteboard.read_string().as_deref(),
        Some("keep this")
    );
}

#[test]
fn timed_out_untagged_response_never_retries_or_decodes_using_a_newer_format() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = test_backend(sender, dir.path().to_path_buf(), 1024, true);
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
    receiver.try_recv().unwrap();
    backend.on_remote_copy(&[offered_format(0xc002, ClipboardFormatName::HTML)]);
    assert!(backend
        .shared
        .expire(Instant::now() + crate::publication::FORMAT_REQUEST_TIMEOUT));
    backend.on_format_data_response(FormatDataResponse::new_unicode_string("late old text"));
    backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_DIB)]);
    assert!(receiver.try_recv().is_err());
    assert!(backend.pending_paste_format.is_none());
    assert!(backend.pasteboard.available_types().is_empty());
}
