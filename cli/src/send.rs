//! The sending half of a terminal-to-terminal transfer.

use std::{error::Error, path::Path};

use serde_json::{Value, json};

use crate::{
    client, crypto,
    payload::{self, Payload},
    progress::Progress,
    transport::{Frame, Transport, relay},
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

    let mut transport = relay::connect_sender(&options.origin, code.nameplate()).await?;

    send_transfer(&mut transport, &code, payload, sealed_size).await
}

/// The sender's half of a transfer, over whatever is carrying it.
///
/// Everything below this line is written against the conversation rather than
/// against a socket, so a second carrier is a different `T` and not a second
/// copy of this function.
async fn send_transfer<T: Transport>(
    transport: &mut T,
    code: &crypto::TransferCode,
    payload: Payload,
    sealed_size: u64,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    wait_for_receiver(transport).await?;

    let keys = exchange_keys(transport, code).await?;
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

    transport
        .send_control(json!({
            "type": "meta",
            "version": crypto::ENVELOPE_VERSION,
            "ciphertext_size": sealed_size,
            "metadata": crypto::to_hex(&metadata),
        }))
        .await?;

    let total = payload.size;
    let result = stream_payload(transport, payload, &mut sealer, sealed_size).await;

    if let Err(error) = result {
        // Tell the peer this transfer is over so the receiver is not left
        // waiting on a session that will never finish.
        let _ = transport.send_control(json!({ "type": "cancel" })).await;
        transport.close().await;
        return Err(error);
    }

    await_completion(transport).await?;
    transport.close().await;

    eprintln!("Sent {}.", crate::progress::format_bytes(total));
    Ok(())
}

async fn wait_for_receiver<T: Transport>(
    transport: &mut T,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    while let Some(frame) = transport.receive().await? {
        let Frame::Control(payload) = frame else {
            continue;
        };

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
async fn exchange_keys<T: Transport>(
    transport: &mut T,
    code: &crypto::TransferCode,
) -> Result<crypto::SessionKeys, Box<dyn Error + Send + Sync>> {
    let (handshake, outbound) = crypto::Handshake::start(code);

    transport
        .send_control(json!({
            "type": "key_exchange",
            "message": crypto::to_hex(&outbound),
        }))
        .await?;

    while let Some(frame) = transport.receive().await? {
        let Frame::Control(payload) = frame else {
            continue;
        };

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
async fn stream_payload<T: Transport>(
    transport: &mut T,
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
            acknowledged = next_acknowledgement(transport, acknowledged).await?;
            progress.update(acknowledged);
        }

        let Some(chunk) = chunks.recv().await else {
            break;
        };

        let sealed = sealer.seal_chunk(&chunk?)?;
        sent += sealed.len() as u64;
        transport.send_chunk(sealed).await?;
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
        acknowledged = next_acknowledgement(transport, acknowledged).await?;
        progress.update(acknowledged);
    }

    progress.finish(total);

    transport
        .send_control(json!({ "type": "complete" }))
        .await?;

    Ok(())
}

async fn next_acknowledgement<T: Transport>(
    transport: &mut T,
    current: u64,
) -> Result<u64, Box<dyn Error + Send + Sync>> {
    while let Some(frame) = transport.receive().await? {
        let Frame::Control(payload) = frame else {
            continue;
        };

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

    Err("the transfer connection closed before the receiver acknowledged the file".into())
}

async fn await_completion<T: Transport>(
    transport: &mut T,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    eprintln!("Waiting for the receiver to finish writing...");

    while let Some(frame) = transport.receive().await? {
        let Frame::Control(payload) = frame else {
            continue;
        };

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

    Err("the transfer connection closed before the receiver confirmed the file".into())
}

fn relay_error(payload: &Value) -> String {
    payload["message"]
        .as_str()
        .unwrap_or("the relay reported an error")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{await_completion, next_acknowledgement, wait_for_receiver};
    use crate::transport::scripted::ScriptedTransport;
    use serde_json::json;

    /// The sender's paths run over a transport that is not a socket, which is
    /// the whole claim the trait makes.
    #[tokio::test]
    async fn waits_past_frames_that_are_not_the_one_it_needs() {
        let mut transport = ScriptedTransport::saying(vec![
            json!({ "type": "status", "status": "waiting_for_receiver" }),
            json!({ "type": "progress", "bytes_transferred": 0 }),
            json!({ "type": "status", "status": "receiver_connected" }),
        ]);

        wait_for_receiver(&mut transport)
            .await
            .expect("the receiver did connect");
    }

    #[tokio::test]
    async fn a_peer_that_stops_before_the_receiver_arrives_is_an_error() {
        let mut transport = ScriptedTransport::silent();

        let error = wait_for_receiver(&mut transport)
            .await
            .expect_err("a peer that says nothing cannot have connected a receiver");
        assert!(error.to_string().contains("before a receiver joined"));
    }

    /// Acknowledgements only ever move forward. A relayed one that is behind
    /// what the sender already counted would otherwise reopen the window.
    #[tokio::test]
    async fn an_acknowledgement_never_moves_backwards() {
        let mut transport =
            ScriptedTransport::saying(vec![json!({ "type": "ack", "bytes_received": 10 })]);

        let acknowledged = next_acknowledgement(&mut transport, 40)
            .await
            .expect("an acknowledgement arrived");
        assert_eq!(acknowledged, 40);
    }

    #[tokio::test]
    async fn an_error_frame_ends_the_wait_with_its_message() {
        let mut transport = ScriptedTransport::saying(vec![
            json!({ "type": "error", "message": "receiver disconnected" }),
        ]);

        let error = await_completion(&mut transport)
            .await
            .expect_err("an error frame is not a completion");
        assert!(error.to_string().contains("receiver disconnected"));
    }

    #[tokio::test]
    async fn a_cancelled_transfer_is_distinct_from_a_dropped_connection() {
        let mut transport =
            ScriptedTransport::saying(vec![json!({ "type": "status", "status": "cancelled" })]);

        let error = await_completion(&mut transport)
            .await
            .expect_err("a cancellation is not a completion");
        assert!(error.to_string().contains("cancelled"));
    }
}
