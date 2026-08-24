//! A transport that replays a fixed conversation, for tests.
//!
//! The point of [`super::Transport`] is that the transfer paths do not know
//! what is carrying them, and the only way to show that is to carry them with
//! something that is not a socket. This also lets a path be tested against a
//! peer that says something awkward — stops early, answers out of order —
//! which is expensive to arrange over a real relay and cheap here.

use std::collections::VecDeque;

use serde_json::Value;

use super::{Frame, Transport, TransportError};

pub struct ScriptedTransport {
    inbound: VecDeque<Frame>,
    sent: Vec<Frame>,
}

impl ScriptedTransport {
    /// A peer that will say these things, in this order, and then stop.
    pub fn new(inbound: Vec<Frame>) -> Self {
        Self {
            inbound: inbound.into(),
            sent: Vec::new(),
        }
    }

    /// The common case: a peer that only sends control frames.
    pub fn saying(inbound: Vec<Value>) -> Self {
        Self::new(inbound.into_iter().map(Frame::Control).collect())
    }

    /// A peer that says nothing and closes.
    pub fn silent() -> Self {
        Self::new(Vec::new())
    }

    /// The control frames written to this transport, in order.
    pub fn sent_control(&self) -> Vec<&Value> {
        self.sent
            .iter()
            .filter_map(|frame| match frame {
                Frame::Control(value) => Some(value),
                Frame::Chunk(_) => None,
            })
            .collect()
    }

    /// The chunks written to this transport, in order.
    pub fn sent_chunks(&self) -> Vec<&Vec<u8>> {
        self.sent
            .iter()
            .filter_map(|frame| match frame {
                Frame::Chunk(chunk) => Some(chunk),
                Frame::Control(_) => None,
            })
            .collect()
    }
}

impl Transport for ScriptedTransport {
    async fn send_control(&mut self, frame: Value) -> Result<(), TransportError> {
        self.sent.push(Frame::Control(frame));
        Ok(())
    }

    async fn send_chunk(&mut self, chunk: Vec<u8>) -> Result<(), TransportError> {
        self.sent.push(Frame::Chunk(chunk));
        Ok(())
    }

    async fn receive(&mut self) -> Result<Option<Frame>, TransportError> {
        Ok(self.inbound.pop_front())
    }

    async fn close(&mut self) {}
}
