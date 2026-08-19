use tracing::debug;

use crate::domain::{
    messages::SenderMessage,
    session::{DownloadEvent, SenderEvent},
};

pub fn log_incoming_sender_message(code: &str, message: &SenderMessage) {
    match message {
        SenderMessage::Meta {
            filename,
            file_size,
            mime_type,
        } => {
            debug!(
                session_code = %code,
                event = "sender_meta",
                filename,
                file_size,
                mime_type,
                "received sender control message"
            );
        }
        SenderMessage::Complete => {
            debug!(
                session_code = %code,
                event = "sender_complete",
                "received sender control message"
            );
        }
        SenderMessage::Cancel => {
            debug!(
                session_code = %code,
                event = "sender_cancel",
                "received sender control message"
            );
        }
    }
}

pub fn log_incoming_upload_chunk(
    code: &str,
    chunk_len: usize,
    bytes_received: u64,
    expected_file_size: Option<u64>,
) {
    debug!(
        session_code = %code,
        event = "upload_chunk",
        chunk_len,
        bytes_received,
        expected_file_size,
        "received upload chunk"
    );
}

pub fn log_sender_event(code: &str, event: &SenderEvent) {
    match event {
        SenderEvent::Status(status) => {
            debug!(
                session_code = %code,
                event = "sender_status",
                status,
                "dispatching sender event"
            );
        }
        SenderEvent::Progress {
            bytes_transferred,
            total_bytes,
        } => {
            debug!(
                session_code = %code,
                event = "sender_progress",
                bytes_transferred,
                total_bytes,
                "dispatching sender event"
            );
        }
        SenderEvent::Acknowledgement { bytes_received } => {
            debug!(
                session_code = %code,
                event = "receiver_acknowledgement",
                bytes_received,
                "dispatching sender event"
            );
        }
        SenderEvent::Error(message) => {
            debug!(
                session_code = %code,
                event = "sender_error",
                message,
                "dispatching sender event"
            );
        }
    }
}

pub fn log_download_event(code: &str, event: &DownloadEvent) {
    match event {
        DownloadEvent::Status(status) => {
            debug!(
                session_code = %code,
                event = "receiver_status",
                status,
                "dispatching receiver event"
            );
        }
        DownloadEvent::Progress {
            bytes_transferred,
            total_bytes,
        } => {
            debug!(
                session_code = %code,
                event = "receiver_progress",
                bytes_transferred,
                total_bytes,
                "dispatching receiver event"
            );
        }
        DownloadEvent::Meta {
            filename,
            file_size,
            mime_type,
        } => {
            debug!(
                session_code = %code,
                event = "receiver_meta",
                filename,
                file_size,
                mime_type,
                "dispatching receiver event"
            );
        }
        DownloadEvent::Chunk { data, .. } => {
            debug!(
                session_code = %code,
                event = "receiver_chunk",
                chunk_len = data.len(),
                "dispatching receiver event"
            );
        }
        DownloadEvent::Complete => {
            debug!(
                session_code = %code,
                event = "receiver_complete",
                "dispatching receiver event"
            );
        }
        DownloadEvent::Error(message) => {
            debug!(
                session_code = %code,
                event = "receiver_error",
                message,
                "dispatching receiver event"
            );
        }
    }
}
