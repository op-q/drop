//! The sending half of a terminal-to-terminal transfer.

use std::{error::Error, path::Path};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    client::{self, Socket},
    crypto,
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

    // The relay bounds and accounts for what actually crosses it, which is
    // ciphertext, so that is what the session reserves.
    let sealed_size = crypto::ciphertext_len(payload.size);

    // Session creation is a blocking HTTP call, so it runs on the blocking
    // pool rather than stalling a runtime worker.
    let nameplate = {
        let origin = options.origin.clone();

        tokio::task::spawn_blocking(move || client::create_session(&origin, sealed_size)).await??
    };

    // The relay allocated the nameplate; the words are drawn here and never
    // sent anywhere. Together they are what the receiver types.
    let code = crypto::TransferCode::generate_for(&nameplate)?;
    let shareable = code.to_shareable();

    (options.on_code)(&shareable);
    eprintln!();
    eprintln!("  Run this on the other computer:");
    eprintln!();
    eprintln!("      drop recv {shareable}");
    eprintln!();
    eprintln!("Waiting for the receiver to connect...");

    let mut socket = client::open_upload(&options.origin, code.nameplate()).await?;

    wait_for_receiver(&mut socket).await?;

    let keys = exchange_keys(&mut socket, &code).await?;
    let mut sealer = crypto::Sealer::new(&keys, payload.size);

    let metadata = crypto::seal_metadata(
        &keys,
        sealed_size,
        &crypto::Metadata {
            filename: payload.filename.clone(),
            mime_type: payload.mime_type.clone(),
            plaintext_size: payload.size,
        },
    )?;

    socket
        .send(Message::Text(
            json!({
                "type": "meta",
                "version": crypto::ENVELOPE_VERSION,
                "ciphertext_size": sealed_size,
                "metadata": crypto::to_hex(&metadata),
            })
            .to_string()
            .into(),
        ))
        .await?;

    let total = payload.size;
    let result = stream_payload(&mut socket, payload, &mut sealer, sealed_size).await;

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

/// Runs the key exchange and returns the derived session keys.
///
/// The relay carries both messages without being able to use either: the
/// password they authenticate against never leaves this process.
async fn exchange_keys(
    socket: &mut Socket,
    code: &crypto::TransferCode,
) -> Result<crypto::SessionKeys, Box<dyn Error + Send + Sync>> {
    let (handshake, outbound) = crypto::Handshake::start(code);

    socket
        .send(Message::Text(
            json!({
                "type": "key_exchange",
                "message": crypto::to_hex(&outbound),
            })
            .to_string()
            .into(),
        ))
        .await?;

    while let Some(message) = socket.next().await {
        let Message::Text(text) = message? else {
            continue;
        };

        let payload: Value = serde_json::from_str(&text)?;

        match payload["type"].as_str() {
            Some("key_exchange") => {
                let peer = payload["message"]
                    .as_str()
                    .ok_or("the receiver sent a malformed key exchange message")?;

                return Ok(handshake.finish(&crypto::from_hex(peer)?)?);
            }
            Some("error") => return Err(relay_error(&payload).into()),
            _ => {}
        }
    }

    Err("the connection closed before the receiver completed the key exchange".into())
}

/// Streams the payload, keeping at most [`WINDOW_BYTES`] unacknowledged.
///
/// The window and the acknowledgements are counted in sealed bytes, because
/// that is what the relay sees and meters. Progress is reported against the
/// same scale so the two never disagree on screen.
async fn stream_payload(
    socket: &mut Socket,
    payload: Payload,
    sealer: &mut crypto::Sealer,
    total: u64,
) -> Result<(), Box<dyn Error + Send + Sync>> {
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

        let sealed = sealer.seal_chunk(&chunk?)?;
        sent += sealed.len() as u64;
        socket.send(Message::Binary(sealed.into())).await?;
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
