use super::*;

#[test]
fn malformed_images_unsolicited_responses_and_errors_do_not_touch_pasteboard() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = MacClipboardBackend::new(sender, dir.path().to_path_buf(), 1024, false);
    let initial = backend.last_change_count.load(Ordering::SeqCst);
    backend.pending_paste_format = Some(RemoteFormat {
        id: ClipboardFormatId::CF_DIB,
        kind: FormatKind::Dib,
    });
    backend.on_format_data_response(FormatDataResponse::new_data(vec![0; 40]));
    assert!(backend.pending_paste_format.is_none());
    backend.on_format_data_response(FormatDataResponse::new_data(vec![65, 0, 0, 0]));
    backend.pending_paste_format = Some(RemoteFormat {
        id: ClipboardFormatId::CF_DIB,
        kind: FormatKind::Dib,
    });
    backend.on_format_data_response(FormatDataResponse::new_error());
    assert!(backend.pending_paste_format.is_none());
    assert_eq!(backend.last_change_count.load(Ordering::SeqCst), initial);
    assert!(receiver.try_recv().is_err());
}

#[test]
fn short_file_response_schedules_bounded_remainder_on_clipboard_channel() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = MacClipboardBackend::new(sender, dir.path().to_path_buf(), 1024, true);
    backend
        .transfer_manager
        .set_incoming_descriptors(vec![FileDescriptor {
            name: "test.txt".to_string(),
            file_size: Some(5),
            attributes: None,
            last_write_time: None,
        }]);
    let requests = backend.transfer_manager.generate_contents_requests();
    backend.on_file_contents_response(FileContentsResponse::new_data_response(
        requests[1].stream_id,
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
    let mut backend = MacClipboardBackend::new(sender, dir.path().to_path_buf(), 1024, false);
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
        backend.pending_paste_format,
        Some(RemoteFormat {
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
    let mut backend = MacClipboardBackend::new(sender, dir.path().to_path_buf(), 1024, true);
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
    let ServerEvent::ClipboardFileContentsRequest(data) = receiver.try_recv().unwrap() else {
        panic!("Expected a file contents request");
    };
    assert!(data.flags.contains(FileContentsFlags::DATA));
    assert_eq!(data.requested_size, 5);
    assert!(receiver.try_recv().is_err());
    assert_eq!(backend.last_change_count.load(Ordering::SeqCst), 0);
}

#[test]
fn coalesces_new_offers_without_misinterpreting_or_publishing_old_responses() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = MacClipboardBackend::new(sender, dir.path().to_path_buf(), 1024, true);
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
    assert_eq!(backend.pending_paste_format.unwrap().kind, FormatKind::Html);
    backend.on_format_data_response(FormatDataResponse::new_error());
    assert!(backend.pending_paste_format.is_none());
    assert!(backend.queued_paste_format.is_none());
    assert_eq!(backend.last_change_count.load(Ordering::SeqCst), 0);
}

#[test]
fn empty_offer_cancels_pending_publication_and_incomplete_file_transfers() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = MacClipboardBackend::new(sender, dir.path().to_path_buf(), 1024, true);
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
    receiver.try_recv().unwrap();
    let ServerEvent::ClipboardFileContentsRequest(request) = receiver.try_recv().unwrap() else {
        panic!("Expected a file contents request");
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
    assert_eq!(backend.last_change_count.load(Ordering::SeqCst), 0);
}

#[test]
fn failed_send_does_not_leave_an_unanswerable_pending_request() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, receiver) = ServerEvent::create_channel();
    drop(receiver);
    let mut backend = MacClipboardBackend::new(sender, dir.path().to_path_buf(), 1024, true);
    backend.on_remote_copy(&[offered_format(0xc001, ClipboardFormatName::HTML)]);
    assert!(backend.pending_paste_format.is_none());
}

#[test]
fn hostile_file_list_count_is_rejected_before_decoding_or_requesting_files() {
    let dir = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = ServerEvent::create_channel();
    let mut backend = MacClipboardBackend::new(sender, dir.path().to_path_buf(), 1024, true);
    backend.on_remote_copy(&[offered_format(0xc001, ClipboardFormatName::FILE_LIST)]);
    receiver.try_recv().unwrap();
    backend.on_format_data_response(FormatDataResponse::new_data(
        u32::MAX.to_le_bytes().to_vec(),
    ));
    assert!(backend.pending_paste_format.is_none());
    assert!(receiver.try_recv().is_err());
    assert_eq!(backend.last_change_count.load(Ordering::SeqCst), 0);
}
