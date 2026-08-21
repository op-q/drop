use tracing::debug;

use crate::domain::{
    messages::SenderMessage,
    session::{DownloadEvent, SenderEvent},
};

pub fn log_incoming_sender_message(code: &str, message: &SenderMessage) {
    match message {
        // Deliberately logs the sealed size and nothing else. The filename and
        // MIME type used to be logged here at debug; they are now inside the
        // sealed metadata blob, and the blob itself is never logged — a
        // ciphertext in a log file is still an artefact of a user's transfer,
        // and debug logs are the easiest place for one to be retained.
        SenderMessage::Meta {
            version,
            ciphertext_size,
            metadata: _,
        } => {
            debug!(
                session_code = %code,
                event = "sender_meta",
                version,
                ciphertext_size,
                "received sender control message"
            );
        }
        SenderMessage::KeyExchange { .. } => {
            debug!(
                session_code = %code,
                event = "sender_key_exchange",
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
    expected_ciphertext_size: Option<u64>,
) {
    debug!(
        session_code = %code,
        event = "upload_chunk",
        chunk_len,
        bytes_received,
        expected_ciphertext_size,
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
        SenderEvent::KeyExchange(_) => {
            debug!(
                session_code = %code,
                event = "sender_key_exchange_forwarded",
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
            version,
            ciphertext_size,
            metadata: _,
        } => {
            debug!(
                session_code = %code,
                event = "receiver_meta",
                version,
                ciphertext_size,
                "dispatching receiver event"
            );
        }
        DownloadEvent::KeyExchange(_) => {
            debug!(
                session_code = %code,
                event = "receiver_key_exchange",
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
