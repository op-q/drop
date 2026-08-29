//! A transport over any ordered byte stream.
//!
//! A WebSocket hands a transfer its framing for free: every message carries its
//! own length, and the text/binary opcode already says whether it is a control
//! frame or payload. A QUIC stream gives neither. It is an ordered, reliable
//! sequence of bytes and nothing more, so a carrier built on one has to say
//! where a frame ends and what kind it is.
//!
//! That is all this module does, which is why it is written against
//! [`AsyncRead`] and [`AsyncWrite`] rather than against QUIC: the framing can
//! then be exercised over an in-memory pipe, with no network and no server
//! anywhere in the test.
//!
//! ```text
//! ┌──────┬────────────────┬────────────────────────────┐
//! │ kind │ length (BE u32)│ payload                    │
//! │ 1 B  │ 4 B            │ `length` bytes             │
//! └──────┴────────────────┴────────────────────────────┘
//!   0x01  control — payload is UTF-8 JSON
//!   0x02  chunk   — payload is sealed bytes, opaque here
//! ```
//!
//! The length is declared before the payload, so a hostile peer could declare
//! one far larger than any real frame and make the reader allocate it. The cap
//! below is checked before a single payload byte is read.

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{Frame, Transport, TransportError};

const KIND_CONTROL: u8 = 0x01;
const KIND_CHUNK: u8 = 0x02;

const HEADER_BYTES: usize = 5;

/// The largest frame this transport will read.
///
/// The largest legitimate one is a sealed chunk: a plaintext chunk plus its
/// authentication tag. Control frames are far smaller — the biggest carries
/// hex-encoded sealed metadata, which the relay already caps at 8 KiB — so one
/// ceiling covers both without needing to trust the kind byte first.
pub const MAX_FRAME_BYTES: usize =
    (crate::crypto::CHUNK_PLAINTEXT_BYTES + crate::crypto::TAG_BYTES) as usize;

/// Carries a transfer over an ordered byte stream.
///
/// Split into a reader and a writer rather than taking one duplex value,
/// because that is the shape QUIC offers: a bidirectional stream opens as a
/// pair.
pub struct FramedTransport<R, W> {
    reader: R,
    writer: W,
}

impl<R, W> FramedTransport<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self { reader, writer }
    }

    async fn write_frame(&mut self, kind: u8, payload: &[u8]) -> Result<(), TransportError> {
        if payload.len() > MAX_FRAME_BYTES {
            return Err(TransportError::Malformed(format!(
                "refusing to send a {} byte frame; the limit is {MAX_FRAME_BYTES}",
                payload.len()
            )));
        }

        let mut header = [0u8; HEADER_BYTES];
        header[0] = kind;
        header[1..].copy_from_slice(&(payload.len() as u32).to_be_bytes());

        // Header and payload go out together. Writing them separately would
        // leave a reader parked on a length whose bytes are still buffered
        // here, which looks exactly like a stalled peer.
        let mut framed = Vec::with_capacity(HEADER_BYTES + payload.len());
        framed.extend_from_slice(&header);
        framed.extend_from_slice(payload);

        self.writer
            .write_all(&framed)
            .await
            .map_err(|error| TransportError::Io(error.to_string()))?;

        self.writer
            .flush()
            .await
            .map_err(|error| TransportError::Io(error.to_string()))
    }

    /// Reads a header, or `None` if the peer finished cleanly.
    ///
    /// A peer that stops between frames is done; one that stops part-way
    /// through a header truncated the stream. `read_exact` cannot tell those
    /// apart — both are `UnexpectedEof` — so this counts the bytes itself.
    async fn read_header(&mut self) -> Result<Option<(u8, usize)>, TransportError> {
        let mut header = [0u8; HEADER_BYTES];
        let mut filled = 0;

        while filled < HEADER_BYTES {
            let read = self
                .reader
                .read(&mut header[filled..])
                .await
                .map_err(|error| TransportError::Io(error.to_string()))?;

            if read == 0 {
                if filled == 0 {
                    return Ok(None);
                }

                return Err(TransportError::Malformed(format!(
                    "the stream ended {filled} bytes into a {HEADER_BYTES} byte frame header"
                )));
            }

            filled += read;
        }

        let length = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;

        if length > MAX_FRAME_BYTES {
            return Err(TransportError::Malformed(format!(
                "the peer declared a {length} byte frame; the limit is {MAX_FRAME_BYTES}"
            )));
        }

        Ok(Some((header[0], length)))
    }
}

impl<R, W> Transport for FramedTransport<R, W>
where
    R: AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    /// A byte pipe has no third party in it, by construction. Whatever is
    /// carrying these frames — a QUIC stream, a test's in-memory duplex — there
    /// is nobody between the peers to refuse a second guess on their behalf.
    fn peers_enforce_one_guess(&self) -> bool {
        true
    }

    async fn send_control(&mut self, frame: Value) -> Result<(), TransportError> {
        let payload = serde_json::to_vec(&frame)
            .map_err(|error| TransportError::Malformed(error.to_string()))?;

        self.write_frame(KIND_CONTROL, &payload).await
    }

    async fn send_chunk(&mut self, chunk: Vec<u8>) -> Result<(), TransportError> {
        self.write_frame(KIND_CHUNK, &chunk).await
    }

    async fn receive(&mut self) -> Result<Option<Frame>, TransportError> {
        let Some((kind, length)) = self.read_header().await? else {
            return Ok(None);
        };

        let mut payload = vec![0u8; length];
        self.reader
            .read_exact(&mut payload)
            .await
            .map_err(|error| {
                TransportError::Malformed(format!(
                    "the stream ended inside a {length} byte frame: {error}"
                ))
            })?;

        match kind {
            KIND_CONTROL => {
                let value = serde_json::from_slice(&payload).map_err(|error| {
                    TransportError::Malformed(format!(
                        "the peer sent a control frame that is not JSON: {error}"
                    ))
                })?;

                Ok(Some(Frame::Control(value)))
            }
            KIND_CHUNK => Ok(Some(Frame::Chunk(payload))),
            other => Err(TransportError::Malformed(format!(
                "the peer sent a frame of unknown kind {other:#04x}"
            ))),
        }
    }

    async fn close(&mut self) {
        let _ = self.writer.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::{FramedTransport, HEADER_BYTES, MAX_FRAME_BYTES};
    use crate::transport::{Frame, Transport, TransportError};
    use serde_json::json;
    use tokio::io::{AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf, duplex, split};

    type Pipe = FramedTransport<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

    /// Two transports joined by an in-memory pipe. No socket, no server.
    fn joined() -> (Pipe, Pipe) {
        let (left, right) = duplex(4 * 1024 * 1024);
        let (left_read, left_write) = split(left);
        let (right_read, right_write) = split(right);

        (
            FramedTransport::new(left_read, left_write),
            FramedTransport::new(right_read, right_write),
        )
    }

    #[tokio::test]
    async fn a_control_frame_and_a_chunk_stay_distinguishable() {
        let (mut sender, mut receiver) = joined();

        sender
            .send_control(json!({ "type": "meta", "ciphertext_size": 17 }))
            .await
            .expect("control frame sent");
        sender
            .send_chunk(vec![9_u8; 1024])
            .await
            .expect("chunk sent");

        let Some(Frame::Control(control)) = receiver.receive().await.expect("a frame arrived")
        else {
            panic!("expected the control frame first");
        };
        assert_eq!(control["ciphertext_size"], 17);

        let Some(Frame::Chunk(chunk)) = receiver.receive().await.expect("a frame arrived") else {
            panic!("expected the chunk second");
        };
        assert_eq!(chunk, vec![9_u8; 1024]);
    }

    /// Frames are not messages on the wire, so a chunk whose bytes happen to
    /// look like a header must not be read as one.
    #[tokio::test]
    async fn a_chunk_that_looks_like_a_header_is_still_a_chunk() {
        let (mut sender, mut receiver) = joined();

        let deceptive = vec![0x01, 0x00, 0x00, 0x00, 0x40, 0xff, 0xff];
        sender
            .send_chunk(deceptive.clone())
            .await
            .expect("chunk sent");
        sender
            .send_control(json!({ "type": "complete" }))
            .await
            .expect("control frame sent");

        let Some(Frame::Chunk(chunk)) = receiver.receive().await.expect("a frame arrived") else {
            panic!("expected a chunk");
        };
        assert_eq!(chunk, deceptive);

        let Some(Frame::Control(control)) = receiver.receive().await.expect("a frame arrived")
        else {
            panic!("the frame after it must still parse");
        };
        assert_eq!(control["type"], "complete");
    }

    #[tokio::test]
    async fn a_peer_that_stops_between_frames_has_simply_finished() {
        let (mut sender, mut receiver) = joined();

        sender.close().await;

        assert!(
            receiver
                .receive()
                .await
                .expect("a clean end is not an error")
                .is_none(),
            "a stream that ends between frames is the peer finishing"
        );
    }

    /// A stream that stops part-way through a header is not a peer finishing,
    /// and reporting it as one would turn a truncated transfer into a
    /// successful-looking short read.
    #[tokio::test]
    async fn a_stream_that_stops_inside_a_header_is_an_error() {
        let (left, right) = duplex(64);
        let (_left_read, mut left_write) = split(left);
        let (right_read, right_write) = split(right);
        let mut receiver = FramedTransport::new(right_read, right_write);

        left_write
            .write_all(&[0x02, 0x00])
            .await
            .expect("a partial header written");
        left_write.shutdown().await.expect("writer closed");

        let Err(TransportError::Malformed(message)) = receiver.receive().await else {
            panic!("a truncated header is malformed, not a clean end");
        };
        assert!(message.contains("2 bytes into"), "unexpected: {message}");
    }

    /// The length is read before the payload, so a declared size is an
    /// allocation request from a peer nobody has authenticated yet.
    #[tokio::test]
    async fn an_oversized_declared_length_is_refused_before_it_is_allocated() {
        let (left, right) = duplex(64);
        let (_left_read, mut left_write) = split(left);
        let (right_read, right_write) = split(right);
        let mut receiver = FramedTransport::new(right_read, right_write);

        let mut header = [0u8; HEADER_BYTES];
        header[0] = 0x02;
        header[1..].copy_from_slice(&u32::MAX.to_be_bytes());
        left_write
            .write_all(&header)
            .await
            .expect("an outrageous header written");

        let Err(TransportError::Malformed(message)) = receiver.receive().await else {
            panic!("a four gigabyte frame must be refused");
        };
        assert!(message.contains("4294967295"), "unexpected: {message}");
    }

    #[tokio::test]
    async fn an_unknown_frame_kind_is_refused_rather_than_ignored() {
        let (left, right) = duplex(64);
        let (_left_read, mut left_write) = split(left);
        let (right_read, right_write) = split(right);
        let mut receiver = FramedTransport::new(right_read, right_write);

        let mut header = [0u8; HEADER_BYTES];
        header[0] = 0x07;
        header[1..].copy_from_slice(&1_u32.to_be_bytes());
        left_write.write_all(&header).await.expect("header written");
        left_write
            .write_all(&[0x00])
            .await
            .expect("payload written");

        let Err(TransportError::Malformed(message)) = receiver.receive().await else {
            panic!("an unknown kind must be refused");
        };
        assert!(message.contains("0x07"), "unexpected: {message}");
    }

    /// A whole sealed chunk is the largest frame there is, so it has to fit.
    #[tokio::test]
    async fn a_full_sized_sealed_chunk_fits_the_ceiling() {
        let (mut sender, mut receiver) = joined();

        let full = vec![7_u8; MAX_FRAME_BYTES];
        sender.send_chunk(full.clone()).await.expect("chunk sent");

        let Some(Frame::Chunk(chunk)) = receiver.receive().await.expect("a frame arrived") else {
            panic!("expected a chunk");
        };
        assert_eq!(chunk.len(), MAX_FRAME_BYTES);
        assert_eq!(chunk, full);
    }

    /// A whole transfer, sender to receiver, over nothing but a byte pipe.
    ///
    /// No relay, no socket, no server — the two halves of the CLI talking
    /// straight to each other through the framing above. This is the shape the
    /// QUIC transport will have, and it is the first transfer in this project
    /// that involves no Drop-operated process at all.
    ///
    /// It also pins the vocabulary work from `decisions.md` entry 12. Over a
    /// relay the sender is told `receiver_connected`, hears `ack`, and finishes
    /// on `status: transfer_complete`. None of those exist here: the sender is
    /// ready immediately, hears the receiver's own `chunk_ack`, and finishes on
    /// the receiver's own `complete` after checking the count itself. If the
    /// sender still needed the relay's wording, this would hang.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_whole_transfer_crosses_a_bare_byte_pipe() {
        let base = std::env::temp_dir().join(format!("drop-pipe-{}", std::process::id()));
        let source = base.join("source");
        let destination = base.join("destination");
        std::fs::create_dir_all(&source).expect("source directory");
        std::fs::create_dir_all(&destination).expect("destination directory");

        // Larger than one chunk and deliberately not a multiple of one.
        let contents: Vec<u8> = (0..(2 * 1024 * 1024 + 4_321))
            .map(|index| (index % 251) as u8)
            .collect();
        let file = source.join("pipe.bin");
        std::fs::write(&file, &contents).expect("fixture written");

        let code = crate::crypto::TransferCode::generate_for("A1B2C3").expect("a code");
        let payload = crate::payload::Payload::prepare(&file, None).expect("payload prepared");
        let sealed_size = crate::crypto::ciphertext_len(payload.size);

        let (mut sending, mut receiving) = joined();

        let options = crate::recv::ReceiveOptions {
            origin: String::new(),
            out_dir: destination.clone(),
            extract: true,
            force: true,
        };

        let send = crate::send::send_transfer(&mut sending, &code, payload, sealed_size);
        let receive = crate::recv::receive_transfer(&mut receiving, &code, &options);

        let (sent, received) = tokio::join!(send, receive);
        assert!(
            matches!(
                sent.expect("the send half should succeed"),
                crate::send::Attempt::Done
            ),
            "the send half should have run to completion"
        );
        received.expect("the receive half should succeed");

        assert_eq!(
            std::fs::read(destination.join("pipe.bin")).expect("received file"),
            contents,
            "the received bytes must match the sent bytes exactly"
        );

        std::fs::remove_dir_all(&base).ok();
    }

    /// The gate this whole checkpoint exists for, over two real paths and a
    /// real byte pipe rather than a script.
    ///
    /// Both peers hold the same nameplate — the public half, which an attacker
    /// enumerates rather than guesses — and different words. That is exactly
    /// what a mistype looks like and exactly what a guess looks like, which is
    /// the point: the sender cannot tell them apart and must treat both as one
    /// consumed attempt.
    ///
    /// What is being asserted is *when* the sender stops. Before this
    /// checkpoint existed it would have streamed the entire payload first and
    /// discovered the failure from a connection that went quiet, which is what
    /// made an unlimited guessing oracle possible.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_wrong_code_stops_the_sender_before_a_single_chunk() {
        let base = std::env::temp_dir().join(format!("drop-guess-{}", std::process::id()));
        let source = base.join("source");
        let destination = base.join("destination");
        std::fs::create_dir_all(&source).expect("source directory");
        std::fs::create_dir_all(&destination).expect("destination directory");

        let file = source.join("secret.bin");
        std::fs::write(&file, vec![7u8; 3 * 1024 * 1024]).expect("fixture written");

        let sender_code = crate::crypto::TransferCode::parse("A1B2C3-abandon-ability-able")
            .expect("the sender's code");
        let guessed_code =
            crate::crypto::TransferCode::parse("A1B2C3-zone-zoo-zebra").expect("a wrong guess");
        assert_eq!(
            sender_code.nameplate(),
            guessed_code.nameplate(),
            "the guess must reach the right meeting point; only the words differ"
        );

        let payload = crate::payload::Payload::prepare(&file, None).expect("payload prepared");
        let sealed_size = crate::crypto::ciphertext_len(payload.size);

        let (mut sending, mut receiving) = joined();

        let options = crate::recv::ReceiveOptions {
            origin: String::new(),
            out_dir: destination.clone(),
            extract: true,
            force: true,
        };

        let send = crate::send::send_transfer(&mut sending, &sender_code, payload, sealed_size);
        let receive = crate::recv::receive_transfer(&mut receiving, &guessed_code, &options);

        let (sent, received) = tokio::join!(send, receive);

        let outcome = sent.expect("a failed guess is an outcome, not a transport failure");
        let crate::send::Attempt::FailedTheCode { what_happened, .. } = outcome else {
            panic!("a peer that cannot open the metadata must not look like a success");
        };
        assert!(
            what_happened.contains("did not open it"),
            "the receiver should have said so rather than simply vanishing, so that \
             the sender is not waiting out a timeout it did not need: {what_happened}"
        );
        received.expect_err("the wrong code cannot open the transfer");

        assert!(
            std::fs::read_dir(&destination)
                .expect("destination readable")
                .next()
                .is_none(),
            "a failed guess must leave nothing behind on the receiver"
        );

        std::fs::remove_dir_all(&base).ok();
    }
}
