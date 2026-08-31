//! A direct QUIC transport, with no Drop-operated server in the path.
//!
//! The framing is [`super::framed`]'s; this module is only the connection. It
//! binds an endpoint, gets the two peers attached to one bidirectional stream,
//! and hands that stream to `FramedTransport`.
//!
//! Nothing here is reachable from the CLI yet, deliberately. Selection and
//! fallback are Phase 4, and the plan's unsolved one-guess question has to be
//! settled before a user can reach this path at all.

use std::time::Duration;

use iroh::{
    Endpoint, EndpointAddr, RelayMode,
    endpoint::{Connection, RecvStream, SendStream, presets},
};
use serde_json::Value;

use super::{Frame, Transport, TransportError, framed::FramedTransport};

/// Names this protocol and its framing on the wire.
///
/// Bump the integer whenever the framing changes. A peer speaking a different
/// version is refused by iroh before either side allocates anything, which is
/// cheaper and clearer than discovering the mismatch in a frame header.
pub const DROP_ALPN: &[u8] = b"drop/transfer/1";

/// How long to wait for a home relay before giving up on being reachable.
///
/// [`Endpoint::online`] has no timeout of its own and no return value, so an
/// endpoint that never reaches a relay waits forever. Measured latency against
/// n0's production relays is a little over three seconds; this leaves room for
/// a slow network without leaving a user staring at nothing.
const ONLINE_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a finished receiver waits for the sender to close first.
///
/// Bounded because the wait is a courtesy, not a step the transfer depends on:
/// by the time a receiver reaches it the file is written, verified and
/// reported. A sender that dies without closing costs the receiver this long
/// and nothing else.
const PEER_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Which half of the transfer this endpoint took.
///
/// Carried only to decide how to close. QUIC has no polite goodbye: a
/// `CONNECTION_CLOSE` permits the peer to drop stream data it has received but
/// not yet handed to its application, *including data it already acknowledged*.
/// iroh states the rule plainly on [`Connection::close`] — only the peer last
/// **receiving** application data can be certain everything arrived, and
/// closing is the only reliable thing it can then do.
///
/// In Drop that peer is the sender: the receiver's `complete` is the last frame
/// on the wire. So the sender closes and the receiver waits to be closed.
#[derive(Clone, Copy)]
enum Role {
    /// Accepted the connection, opened the stream, spoke first — and reads
    /// last. Nothing it wrote is still owed to anyone.
    Sender,
    /// Dialled the sender. Its final frame is the one the sender is blocked
    /// reading, so it must not slam the door on it.
    Receiver,
}

/// A bound QUIC endpoint, before it carries a transfer.
pub struct QuicEndpoint {
    endpoint: Endpoint,
}

impl QuicEndpoint {
    /// Binds an endpoint that can reach peers behind NAT.
    pub async fn bind() -> Result<Self, TransportError> {
        Self::bind_with(RelayMode::Default).await
    }

    /// Binds an endpoint that will only ever talk to peers it can reach
    /// directly.
    ///
    /// Used by the tests, where both peers are on one machine and a round trip
    /// to a third party's relay would be latency and flakiness bought for
    /// nothing. It is also the honest configuration for a LAN-only transfer.
    /// Note that without relays there is no hole punching and no home relay, so
    /// [`Self::online`] must not be called on one of these.
    pub async fn bind_without_relays() -> Result<Self, TransportError> {
        Self::bind_with(RelayMode::Disabled).await
    }

    async fn bind_with(relay_mode: RelayMode) -> Result<Self, TransportError> {
        // No `.secret_key()`, on purpose. An unset key means a fresh one per
        // bind, which is the per-transfer unlinkability we want — and it keeps
        // anyone from reaching for the rendezvous seed here. That seed is
        // derived from a public nameplate, so using it as the endpoint's
        // identity would hand anybody who guessed the nameplate the ability to
        // complete this handshake *as the sender*.
        let endpoint = Endpoint::builder(presets::Minimal)
            .relay_mode(relay_mode)
            .alpns(vec![DROP_ALPN.to_vec()])
            .bind()
            .await
            .map_err(|error| {
                TransportError::Connect(format!("could not bind a QUIC endpoint: {error}"))
            })?;

        Ok(Self { endpoint })
    }

    /// Waits until this endpoint is reachable from outside its network.
    ///
    /// Binding is not being reachable: immediately after it, the address below
    /// carries local interfaces and no relay at all. Publishing that would
    /// advertise a meeting point nobody outside the LAN can use.
    pub async fn online(&self) -> Result<(), TransportError> {
        tokio::time::timeout(ONLINE_TIMEOUT, self.endpoint.online())
            .await
            .map_err(|_| {
                TransportError::Connect(format!(
                    "no relay answered within {}s, so this machine has no reachable address",
                    ONLINE_TIMEOUT.as_secs()
                ))
            })
    }

    /// Where a peer should look for this endpoint.
    pub fn addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    /// The sender's side: wait for a peer to dial, then open the stream.
    ///
    /// The sender accepts the *connection* and opens the *stream*, which reads
    /// backwards until you know why. `accept_bi` does not resolve when a peer
    /// opens a stream, only when it first writes on one, and iroh's own rule is
    /// that whichever endpoint transmits first should open it. Drop's sender
    /// speaks first — `exchange_keys` writes before it reads — while the
    /// receiver is the one that resolved an address and therefore dials. Let
    /// the dialer open the stream instead and both sides park forever.
    ///
    /// One connection per call, and the endpoint survives the call. It used to
    /// be consumed, which made a failed guess unconditionally fatal — strict
    /// one-attempt, and the behaviour `docs/decisions.md` entry 13 weighed and
    /// rejected, because a receiver who fat-fingers one word should not lose
    /// the transfer with nothing to explain why. What replaces it is not a
    /// looser rule but a differently enforced one: the caller may come back for
    /// another attempt, and entry 13 requires it to ask a human first.
    pub async fn accept_transfer(&self) -> Result<QuicTransport, TransportError> {
        let incoming = self.endpoint.accept().await.ok_or_else(|| {
            TransportError::Connect("the endpoint closed before a peer connected".into())
        })?;

        let connection = incoming.await.map_err(|error| {
            TransportError::Connect(format!("a peer failed to complete the handshake: {error}"))
        })?;

        let (send, recv) = connection.open_bi().await.map_err(|error| {
            TransportError::Io(format!("could not open a transfer stream: {error}"))
        })?;

        Ok(QuicTransport::new(Role::Sender, connection, send, recv))
    }

    /// The receiver's side: dial the sender, then accept the stream it opens.
    pub async fn connect_transfer(
        &self,
        peer: EndpointAddr,
    ) -> Result<QuicTransport, TransportError> {
        let connection = self
            .endpoint
            .connect(peer, DROP_ALPN)
            .await
            .map_err(|error| {
                TransportError::Connect(format!("could not reach the sender: {error}"))
            })?;

        // Resolves when the sender writes its first frame, not when it opens
        // the stream. There is nothing to do before then anyway.
        let (send, recv) = connection.accept_bi().await.map_err(|error| {
            TransportError::Io(format!(
                "the sender never opened a transfer stream: {error}"
            ))
        })?;

        Ok(QuicTransport::new(Role::Receiver, connection, send, recv))
    }

    /// Closes the endpoint, and with it every connection it still holds.
    ///
    /// Separate from [`QuicTransport::close`] because the two now have
    /// different lifetimes: a transport ends when one attempt ends, and the
    /// endpoint has to outlive a failed attempt so the next one has somewhere
    /// to land. Closing rather than dropping is what gives an in-flight
    /// `CONNECTION_CLOSE` time to reach the peer, so a caller that simply drops
    /// this can truncate the goodbye of a transfer that otherwise succeeded.
    pub async fn shutdown(self) {
        self.endpoint.close().await;
    }
}

/// One transfer over one bidirectional QUIC stream.
pub struct QuicTransport {
    framed: FramedTransport<RecvStream, SendStream>,
    connection: Connection,
    role: Role,
}

impl QuicTransport {
    fn new(role: Role, connection: Connection, send: SendStream, recv: RecvStream) -> Self {
        Self {
            // Reader first, writer second — the reverse of the pair iroh hands
            // back. Both orderings typecheck; only one of them works.
            framed: FramedTransport::new(recv, send),
            connection,
            role,
        }
    }

    /// Who is on the other end, as QUIC authenticated them.
    ///
    /// This says nothing about whether they know the transfer code. A peer that
    /// resolved the nameplate can complete this handshake; only the key
    /// exchange decides whether they get bytes.
    pub fn peer(&self) -> iroh::EndpointId {
        self.connection.remote_id()
    }
}

impl Transport for QuicTransport {
    /// Nobody is in the middle of a direct connection, so nothing enforces one
    /// guess unless the peers do. Without the checkpoint this answer turns on,
    /// a peer that guessed a nameplate could connect, try a password, learn one
    /// bit from whether the metadata opened, disconnect and repeat — which is
    /// not 33 bits of security but 33 bits of work at network speed.
    fn peers_enforce_one_guess(&self) -> bool {
        true
    }

    /// A QUIC connection is a connection to somebody, so by the time one of
    /// these exists the peer is already there.
    async fn await_peer(&mut self) -> Result<(), TransportError> {
        Ok(())
    }

    async fn send_control(&mut self, frame: Value) -> Result<(), TransportError> {
        self.framed.send_control(frame).await
    }

    async fn send_chunk(&mut self, chunk: Vec<u8>) -> Result<(), TransportError> {
        self.framed.send_chunk(chunk).await
    }

    async fn receive(&mut self) -> Result<Option<Frame>, TransportError> {
        self.framed.receive().await
    }

    /// Ends the stream, then — depending on which side this is — the
    /// connection.
    ///
    /// Which side matters, and this is the whole reason [`Role`] exists. A
    /// `CONNECTION_CLOSE` permits the peer to discard stream data it has
    /// received but not yet handed up, acknowledged or not. The receiver's last
    /// act is to write the `complete` that the sender is sitting in
    /// `await_completion` waiting for, so a receiver that closes here destroys
    /// that frame in transit and fails a transfer that in fact succeeded — the
    /// file on disk is already correct.
    ///
    /// So only the sender closes. The receiver finishes its stream, which
    /// flushes what it wrote and signals that no more frames are coming, and
    /// then waits for the sender to close.
    ///
    /// The endpoint is deliberately left alone — see [`QuicEndpoint::shutdown`],
    /// which is where closing it moved to when it had to start outliving a
    /// single attempt.
    async fn close(&mut self) {
        // Both sides finish the stream: it flushes, and it tells the peer that
        // nothing further is coming without touching the connection.
        self.framed.close().await;

        match self.role {
            Role::Sender => {
                self.connection.close(0u32.into(), b"complete");
            }
            Role::Receiver => {
                // Returns the reason, which is of no interest — the sender
                // having closed at all is the signal, and a timeout here is
                // survivable. The transfer is already done either way.
                let _ = tokio::time::timeout(PEER_CLOSE_TIMEOUT, self.connection.closed()).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DROP_ALPN, Duration, QuicEndpoint};
    use crate::transport::{Frame, Transport};
    use serde_json::json;

    /// The orchestration in one test: the sender accepts the connection and
    /// opens the stream, the receiver dials and accepts it. Getting this
    /// backwards deadlocks both sides rather than failing, so it is worth
    /// pinning on its own before anything larger runs through it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stream_opens_the_way_round_the_protocol_needs() {
        let sender = QuicEndpoint::bind_without_relays()
            .await
            .expect("a sender endpoint");
        let receiver = QuicEndpoint::bind_without_relays()
            .await
            .expect("a receiver endpoint");

        let address = sender.addr();

        // The write stays inside the task, and that is not tidiness. The
        // receiver's `accept_bi` resolves on the sender's first write, not when
        // the stream opens, so a version of this that joined both tasks before
        // writing anything would deadlock rather than fail.
        let sending = tokio::spawn(async move {
            let mut transport = sender.accept_transfer().await.expect("a peer dialled");
            transport
                .send_control(json!({ "type": "key_exchange", "message": "5e4de5" }))
                .await
                .expect("the sender speaks first");
            (transport, sender)
        });

        let receiving = tokio::spawn(async move {
            let mut transport = receiver
                .connect_transfer(address)
                .await
                .expect("the sender was reachable");

            let frame = transport.receive().await.expect("a frame arrived");
            (transport, receiver, frame)
        });

        let (mut sending, sender) = sending.await.expect("the sending task");
        let (mut receiving, receiver, frame) = receiving.await.expect("the receiving task");

        let Some(Frame::Control(payload)) = frame else {
            panic!("expected the sender's first frame");
        };
        assert_eq!(payload["message"], "5e4de5");

        sending.close().await;
        receiving.close().await;

        // The endpoints outlive their transports now, so closing them is the
        // caller's job rather than the transport's.
        sender.shutdown().await;
        receiver.shutdown().await;
    }

    /// A dropped endpoint takes its connections with it.
    ///
    /// Written after a real-network run failed where every loopback test
    /// passed. `QuicTransport` deliberately does not own its endpoint — the
    /// sender needs one endpoint to outlive several connections, which is what
    /// makes another attempt possible at all — so keeping it alive is the
    /// caller's job. The receiver's dial path got that wrong: it bound an
    /// endpoint, dialled, returned the transport, and let the endpoint drop on
    /// the way out. The peer saw the transfer die the instant it began.
    ///
    /// Every other test in this module holds both endpoints in its own scope
    /// for the whole test, so none of them could observe this. This one drops
    /// one on purpose, and asserts the consequence rather than the fix, so that
    /// a future refactor which reintroduces it has something to fail against.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_an_endpoint_ends_the_transfer_on_it() {
        let sender = QuicEndpoint::bind_without_relays()
            .await
            .expect("a sender endpoint");
        let receiver = QuicEndpoint::bind_without_relays()
            .await
            .expect("a receiver endpoint");
        let address = sender.addr();

        let sending = tokio::spawn(async move {
            let mut transport = sender.accept_transfer().await.expect("a peer dialled");
            transport
                .send_control(json!({ "type": "key_exchange", "message": "5e4de5" }))
                .await
                .expect("the sender speaks first");
            // Bounded rather than read to the end: the failure being pinned is
            // that nothing useful arrives, and waiting out QUIC's idle timeout
            // to prove it would cost the suite half a minute for one assertion.
            let result = tokio::time::timeout(Duration::from_secs(3), transport.receive()).await;
            (transport, sender, result)
        });

        // The shape the bug had: keep the transport, drop the endpoint.
        let mut dialled = {
            let transport = receiver
                .connect_transfer(address)
                .await
                .expect("the sender was reachable");
            drop(receiver);
            transport
        };

        let (mut sending, sender, sender_saw) = sending.await.expect("the sending task");

        // Three outcomes are all correct and all fatal to a transfer: an error,
        // a clean end, or nothing at all. What must never happen is a frame,
        // because that would mean the connection outlived its endpoint and the
        // invariant this pins does not exist.
        let carried_a_frame = matches!(sender_saw, Ok(Ok(Some(_))));
        assert!(
            !carried_a_frame,
            "a connection must not outlive the endpoint that owns it"
        );

        // The receiver side is equally dead; it must not look healthy either.
        let _ = tokio::time::timeout(Duration::from_secs(3), dialled.receive()).await;

        sending.close().await;
        sender.shutdown().await;
    }

    /// The ALPN is the version gate. Two builds whose framing disagrees must
    /// fail to connect rather than meet and misparse each other.
    #[test]
    fn the_alpn_names_a_version() {
        assert!(
            DROP_ALPN.ends_with(b"/1"),
            "the ALPN must carry a version to bump: {}",
            String::from_utf8_lossy(DROP_ALPN)
        );
    }

    /// A whole encrypted transfer over a real QUIC connection, with no Drop
    /// server anywhere in it.
    ///
    /// This is what the plan is for. Everything below the connection is the
    /// shipped code path: the same `send_transfer` and `receive_transfer` the
    /// relay uses, the same envelope, the same framing — carried by two peers
    /// talking directly to each other.
    ///
    /// It settles three things at once that nothing smaller could:
    ///
    /// - **A finished stream reads as a clean end.** `framed`'s header read
    ///   distinguishes "the peer finished" from "the peer was cut off" by
    ///   counting bytes, which is only correct if a finished `RecvStream`
    ///   surfaces as zero bytes through its `AsyncRead`. Over a pipe that is
    ///   obvious; over QUIC it was an assumption.
    /// - **Full-sized frames survive flow control.** The payload is larger than
    ///   two chunks, so a whole 1 MiB sealed chunk crosses as one frame rather
    ///   than as a convenient small one.
    /// - **Closing does not truncate.** The sender closes only after the
    ///   receiver's acknowledgement, so a `CONNECTION_CLOSE` cannot discard
    ///   bytes the peer had received but not yet handed up.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_whole_transfer_crosses_a_quic_connection() {
        let base = std::env::temp_dir().join(format!("drop-quic-{}", std::process::id()));
        let source = base.join("source");
        let destination = base.join("destination");
        std::fs::create_dir_all(&source).expect("source directory");
        std::fs::create_dir_all(&destination).expect("destination directory");

        // Over two chunks, so one crosses at its full sealed size.
        let contents: Vec<u8> = (0..(2 * 1024 * 1024 + 9_999))
            .map(|index| (index % 251) as u8)
            .collect();
        let file = source.join("quic.bin");
        std::fs::write(&file, &contents).expect("fixture written");

        let code = crate::crypto::TransferCode::generate_for("A1B2C3").expect("a code");
        let payload = crate::payload::Payload::prepare(&file, None).expect("payload prepared");
        let sealed_size = crate::crypto::ciphertext_len(payload.size);

        let sender = QuicEndpoint::bind_without_relays()
            .await
            .expect("a sender endpoint");
        let receiver = QuicEndpoint::bind_without_relays()
            .await
            .expect("a receiver endpoint");
        let address = sender.addr();

        let sending = tokio::spawn({
            let code = code.clone();

            async move {
                let mut transport = sender.accept_transfer().await.expect("a peer dialled");
                crate::send::send_transfer(&mut transport, &code, payload, sealed_size)
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|outcome| match outcome {
                        crate::send::Attempt::Done => Ok(()),
                        other => Err(format!("the transfer did not complete: {other:?}")),
                    })
            }
        });

        let receiving = tokio::spawn({
            let destination = destination.clone();

            async move {
                let mut transport = receiver
                    .connect_transfer(address)
                    .await
                    .expect("the sender was reachable");

                crate::recv::receive_transfer(
                    &mut transport,
                    &code,
                    &crate::recv::ReceiveOptions {
                        path: crate::direct::Path::Relay,
                        status: false,
                        origin: String::new(),
                        out_dir: destination,
                        extract: true,
                        force: true,
                    },
                )
                .await
                .map_err(|error| error.to_string())
            }
        });

        let (sent, received) = tokio::join!(sending, receiving);
        let sent = sent.expect("the sending task");
        let received = received.expect("the receiving task");

        // Both, not either. A close-ordering bug shows up on one side as a
        // lost connection and on the other as success, and asserting the
        // sender alone hides which of the two actually misbehaved.
        assert!(
            sent.is_ok() && received.is_ok(),
            "sender: {sent:?}\nreceiver: {received:?}"
        );

        assert_eq!(
            std::fs::read(destination.join("quic.bin")).expect("received file"),
            contents,
            "the received bytes must match the sent bytes exactly"
        );

        std::fs::remove_dir_all(&base).ok();
    }
}
