//! The sending half of a terminal-to-terminal transfer.

use std::{error::Error, path::Path};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    client::{self, Socket},
    payload::{self, Payload},
    progress::Progress,
};

/// In-flight bytes the sender allows before waiting for acknowledgements.
///
/// This is the window that decides throughput on a high-latency link: the
/// sender may keep this much data unacknowledged, so the ceiling is roughly
/// `WINDOW_BYTES / round-trip time` regardless of available bandwidth.
const WINDOW_BYTES: u64 = 16 * 1024 * 1024;

pub struct SendOptions {
    pub origin: String,
    pub compress: Option<u32>,
    /// Called once with the session code, as soon as the relay issues it.
    ///
    /// The code is what the other terminal needs, so it is handed to the caller
    /// rather than only printed: that keeps presentation in the binary and lets
    /// callers that are not a terminal observe it.
    pub on_code: Box<dyn FnMut(&str) + Send>,
}

impl SendOptions {
    /// Options that print the code to stdout, one line, nothing else, so it
    /// survives being piped into another command.
    pub fn printing(origin: String, compress: Option<u32>) -> Self {
        Self {
            origin,
            compress,
            on_code: Box::new(|code| println!("{code}")),
        }
    }
}

pub async fn run(
    path: &Path,
    mut options: SendOptions,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // A signal terminates the process without unwinding, so the spool file's
    // destructor never runs. Delete it here instead, or a cancelled compressed
    // send would leave the user's bytes behind in the temporary directory.
    tokio::spawn(async {
        payload::wait_for_termination().await;
        payload::remove_spool_files();
        std::process::exit(130);
    });

    let payload = Payload::prepare(path, options.compress)?;

    for warning in &payload.warnings {
        eprintln!("warning: {warning}");
    }

    if payload.size == 0 {
        return Err("there is nothing to send: the payload is empty".into());
    }

    eprintln!("Sending {}", payload.summary);
    eprintln!("Size    {}", crate::progress::format_bytes(payload.size));

    // Session creation is a blocking HTTP call, so it runs on the blocking
    // pool rather than stalling a runtime worker.
    let code = {
        let origin = options.origin.clone();
        let filename = payload.filename.clone();
        let size = payload.size;

        tokio::task::spawn_blocking(move || client::create_session(&origin, &filename, size))
            .await??
    };

    (options.on_code)(&code);
    eprintln!();
    eprintln!("  Run this on the other computer:");
    eprintln!();
    eprintln!("      drop recv {code}");
    eprintln!();
    eprintln!("Waiting for the receiver to connect...");

    let mut socket = client::open_upload(&options.origin, &code).await?;

    wait_for_receiver(&mut socket).await?;

    socket
        .send(Message::Text(
            json!({
                "type": "meta",
                "filename": payload.filename,
                "file_size": payload.size,
                "mime_type": payload.mime_type,
            })
            .to_string()
            .into(),
        ))
        .await?;

    let total = payload.size;
    let result = stream_payload(&mut socket, payload).await;

    if let Err(error) = result {
        // Tell the relay this transfer is over so the receiver is not left
        // waiting on a session that will never finish.
        let _ = socket
            .send(Message::Text(
                json!({ "type": "cancel" }).to_string().into(),
            ))
            .await;
        let _ = socket.close(None).await;
        return Err(error);
    }

    await_completion(&mut socket).await?;
    let _ = socket.close(None).await;

    eprintln!("Sent {}.", crate::progress::format_bytes(total));
    Ok(())
}

async fn wait_for_receiver(socket: &mut Socket) -> Result<(), Box<dyn Error + Send + Sync>> {
    while let Some(message) = socket.next().await {
        let message = message?;

        let Message::Text(text) = message else {
            continue;
        };

        let payload: Value = serde_json::from_str(&text)?;

        match payload["type"].as_str() {
            Some("status") => {
                if payload["status"].as_str() == Some("receiver_connected") {
                    eprintln!("Receiver connected.");
                    return Ok(());
                }
            }
            Some("error") => {
                return Err(relay_error(&payload).into());
            }
            _ => {}
        }
    }

    Err("the relay closed the connection before a receiver joined".into())
}

/// Streams the payload, keeping at most [`WINDOW_BYTES`] unacknowledged.
async fn stream_payload(
    socket: &mut Socket,
    payload: Payload,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let total = payload.size;
    let mut chunks = payload.into_chunks();

    let mut progress = Progress::new("Sending ", total);
    let mut sent = 0_u64;
    let mut acknowledged = 0_u64;

    loop {
        // Drain acknowledgements that have already arrived, then block only if
        // the window is actually full.
        while sent - acknowledged >= WINDOW_BYTES {
            acknowledged = next_acknowledgement(socket, acknowledged).await?;
            progress.update(acknowledged);
        }

        let Some(chunk) = chunks.recv().await else {
            break;
        };

        let chunk = chunk?;
        sent += chunk.len() as u64;
        socket.send(Message::Binary(chunk.into())).await?;
        progress.update(acknowledged);
    }

    if sent != total {
        return Err(format!(
            "the payload produced {sent} bytes but {total} were declared; \
             the source changed while it was being sent"
        )
        .into());
    }

    while acknowledged < total {
        acknowledged = next_acknowledgement(socket, acknowledged).await?;
        progress.update(acknowledged);
    }

    progress.finish(total);

    socket
        .send(Message::Text(
            json!({ "type": "complete" }).to_string().into(),
        ))
        .await?;

    Ok(())
}

async fn next_acknowledgement(
    socket: &mut Socket,
    current: u64,
) -> Result<u64, Box<dyn Error + Send + Sync>> {
    while let Some(message) = socket.next().await {
        match message? {
            Message::Text(text) => {
                let payload: Value = serde_json::from_str(&text)?;

                match payload["type"].as_str() {
                    Some("ack") => {
                        if let Some(bytes) = payload["bytes_received"].as_u64() {
                            return Ok(bytes.max(current));
                        }
                    }
                    Some("error") => return Err(relay_error(&payload).into()),
                    _ => {}
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    Err("the transfer connection closed before the receiver acknowledged the file".into())
}

async fn await_completion(socket: &mut Socket) -> Result<(), Box<dyn Error + Send + Sync>> {
    eprintln!("Waiting for the receiver to finish writing...");

    while let Some(message) = socket.next().await {
        match message? {
            Message::Text(text) => {
                let payload: Value = serde_json::from_str(&text)?;

                match payload["type"].as_str() {
                    Some("status") => match payload["status"].as_str() {
                        Some("transfer_complete") => return Ok(()),
                        Some("cancelled") => {
                            return Err("the transfer was cancelled".into());
                        }
                        _ => {}
                    },
                    Some("error") => return Err(relay_error(&payload).into()),
                    _ => {}
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    Err("the transfer connection closed before the receiver confirmed the file".into())
}

fn relay_error(payload: &Value) -> String {
    payload["message"]
        .as_str()
        .unwrap_or("the relay reported an error")
        .to_string()
}
