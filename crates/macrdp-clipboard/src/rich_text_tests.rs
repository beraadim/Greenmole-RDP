use super::*;
use crate::{
    formats::FORMAT_ID_HTML,
    pasteboard::PasteboardBridge,
    publication::{PublicationState, FORMAT_REQUEST_TIMEOUT},
};
use ironrdp_cliprdr::{
    backend::{ClipboardMessage, CliprdrBackend},
    pdu::*,
};
use ironrdp_server::{ServerEvent, ServerEventRx};
use std::{sync::Arc, time::Instant};

fn backend() -> (ClipboardWorker, ServerEventRx, tempfile::TempDir) {
    let directory = tempfile::tempdir().unwrap();
    let pasteboard = PasteboardBridge::isolated();
    let shared = Arc::new(PublicationState::new(pasteboard.change_count()));
    let (sender, receiver) = ServerEvent::create_channel();
    let worker = ClipboardWorker::new(
        sender,
        directory.path().to_owned(),
        1024,
        false,
        shared,
        pasteboard,
    );
    (worker, receiver, directory)
}

fn rich_formats() -> [ClipboardFormat; 2] {
    [
        ClipboardFormat::new(ClipboardFormatId::new(0xc137)).with_name(ClipboardFormatName::HTML),
        ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT),
    ]
}

fn requested(receiver: &mut ServerEventRx) -> ClipboardFormatId {
    let ServerEvent::Clipboard(ClipboardMessage::SendInitiatePaste(id)) =
        receiver.try_recv().unwrap()
    else {
        panic!("Expected a format request");
    };
    assert!(
        receiver.try_recv().is_err(),
        "Only one untagged request may be outstanding"
    );
    id
}

fn html_response(html: &str) -> FormatDataResponse<'static> {
    FormatDataResponse::new_data(crate::html::wrap_html_format(html).unwrap())
}

#[test]
fn rich_copy_requests_serially_and_publishes_both_flavors_without_echo() {
    let (mut worker, mut receiver, _directory) = backend();
    worker.pasteboard.write_string("previous copy");
    worker.on_remote_copy(&rich_formats());
    assert_eq!(requested(&mut receiver).value(), 0xc137);
    let initial = worker.pasteboard.change_count();
    worker.on_format_data_response(html_response("<b>rich</b>"));
    assert_eq!(requested(&mut receiver), ClipboardFormatId::CF_UNICODETEXT);
    assert_eq!(worker.pasteboard.change_count(), initial);
    worker.on_format_data_response(FormatDataResponse::new_unicode_string("rich plain text"));
    assert_eq!(
        worker.pasteboard.read_html().as_deref(),
        Some("<b>rich</b>")
    );
    assert_eq!(
        worker.pasteboard.read_string().as_deref(),
        Some("rich plain text")
    );
    assert!(worker.pending_html.is_none());
    assert!(!worker.shared.local_copy_changed(&worker.pasteboard));
    assert!(receiver.try_recv().is_err());
}

#[test]
fn unavailable_or_malformed_html_falls_back_to_advertised_unicode() {
    for response in [
        FormatDataResponse::new_error(),
        FormatDataResponse::new_data(b"invalid HTML".to_vec()),
    ] {
        let (mut worker, mut receiver, _directory) = backend();
        worker.on_remote_copy(&rich_formats());
        requested(&mut receiver);
        worker.on_format_data_response(response);
        assert_eq!(requested(&mut receiver), ClipboardFormatId::CF_UNICODETEXT);
        worker.on_format_data_response(FormatDataResponse::new_unicode_string("fallback"));
        assert_eq!(worker.pasteboard.read_string().as_deref(), Some("fallback"));
        assert!(worker.pasteboard.read_html().is_none());
    }
}

#[test]
fn failed_unicode_keeps_valid_html_and_two_failed_flavors_preserve_previous_copy() {
    let (mut worker, mut receiver, _directory) = backend();
    worker.on_remote_copy(&rich_formats());
    requested(&mut receiver);
    worker.on_format_data_response(html_response("<b>keep rich</b>"));
    requested(&mut receiver);
    worker.on_format_data_response(FormatDataResponse::new_error());
    assert_eq!(
        worker.pasteboard.read_html().as_deref(),
        Some("<b>keep rich</b>")
    );
    assert!(worker.pasteboard.read_string().is_none());
    let count = worker.pasteboard.change_count();
    worker.on_remote_copy(&rich_formats());
    requested(&mut receiver);
    worker.on_format_data_response(FormatDataResponse::new_error());
    requested(&mut receiver);
    worker.on_format_data_response(FormatDataResponse::new_error());
    assert_eq!(worker.pasteboard.change_count(), count);
}

#[test]
fn newer_local_copy_between_flavors_is_preserved() {
    let (mut worker, mut receiver, _directory) = backend();
    worker.on_remote_copy(&rich_formats());
    requested(&mut receiver);
    worker.on_format_data_response(html_response("<b>stale</b>"));
    requested(&mut receiver);
    worker.pasteboard.write_string("newer local copy");
    worker.tick();
    assert!(worker.pending_html.is_none());
    worker.on_format_data_response(FormatDataResponse::new_unicode_string("stale text"));
    assert_eq!(
        worker.pasteboard.read_string().as_deref(),
        Some("newer local copy")
    );
    assert!(worker.pasteboard.read_html().is_none());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn newer_remote_copy_between_flavors_cannot_inherit_old_html() {
    let (mut worker, mut receiver, _directory) = backend();
    worker.on_remote_copy(&rich_formats());
    requested(&mut receiver);
    worker.on_format_data_response(html_response("<b>stale</b>"));
    requested(&mut receiver);
    worker.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]);
    assert!(worker.pending_html.is_none());
    assert!(receiver.try_recv().is_err());
    worker.on_format_data_response(FormatDataResponse::new_unicode_string("stale text"));
    assert_eq!(requested(&mut receiver), ClipboardFormatId::CF_UNICODETEXT);
    assert!(worker.pasteboard.available_types().is_empty());
    worker.on_format_data_response(FormatDataResponse::new_unicode_string("current text"));
    assert_eq!(
        worker.pasteboard.read_string().as_deref(),
        Some("current text")
    );
    assert!(worker.pasteboard.read_html().is_none());
}

#[test]
fn missing_second_response_fails_closed_without_publishing_retained_html() {
    let (mut worker, mut receiver, _directory) = backend();
    worker.on_remote_copy(&rich_formats());
    requested(&mut receiver);
    worker.on_format_data_response(html_response("<b>old rich copy</b>"));
    requested(&mut receiver);
    assert!(worker
        .shared
        .expire(Instant::now() + FORMAT_REQUEST_TIMEOUT));
    worker.on_format_data_response(FormatDataResponse::new_unicode_string("late plain copy"));
    assert!(worker.pending_html.is_none());
    assert!(worker.pasteboard.available_types().is_empty());
    assert!(receiver.try_recv().is_err());
}

#[test]
fn oversized_local_text_and_html_return_format_errors_and_keep_session_usable() {
    let (mut worker, mut receiver, _directory) = backend();
    // The UTF-16 NUL would put this one unit over the wire budget.
    worker
        .pasteboard
        .write_string(&"x".repeat(MAX_TEXT_PAYLOAD_BYTES / 2));
    worker.on_format_data_request(FormatDataRequest {
        format: ClipboardFormatId::CF_UNICODETEXT,
    });
    let ServerEvent::Clipboard(ClipboardMessage::SendFormatData(response)) =
        receiver.try_recv().unwrap()
    else {
        panic!("Expected text response");
    };
    assert!(response.is_error());
    worker
        .pasteboard
        .write_html(&"x".repeat(crate::html::MAX_HTML_FRAGMENT_BYTES + 1));
    worker.on_format_data_request(FormatDataRequest {
        format: ClipboardFormatId::new(FORMAT_ID_HTML),
    });
    let ServerEvent::Clipboard(ClipboardMessage::SendFormatData(response)) =
        receiver.try_recv().unwrap()
    else {
        panic!("Expected HTML response");
    };
    assert!(response.is_error());
    assert!(!worker.event_sender.is_closed());
    worker
        .pasteboard
        .write_string("normal copy after rejection");
    worker.on_format_data_request(FormatDataRequest {
        format: ClipboardFormatId::CF_UNICODETEXT,
    });
    let ServerEvent::Clipboard(ClipboardMessage::SendFormatData(response)) =
        receiver.try_recv().unwrap()
    else {
        panic!("Expected text response");
    };
    assert_eq!(
        response.to_unicode_string().unwrap(),
        "normal copy after rejection"
    );
    assert!(!worker.event_sender.is_closed());
}
