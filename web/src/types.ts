// Wire types for the relay protocol, mirroring `docs/protocol.md`.
//
// The relay no longer carries a filename, a MIME type, or a plaintext size.
// What used to be three cleartext fields on `meta` is now one opaque blob it
// forwards without being able to read, plus the sealed size it needs for
// accounting. Nothing here type-checks against the relay, so `docs/protocol.md`
// is the contract these have to match.

export interface SessionResponse {
  /** The nameplate. Six hex characters, and the only half the relay sees. */
  code: string;
}

export interface StatusState {
  code: string | null;
  lines: string[];
}

export interface StatusMessage {
  type: "status";
  status: string;
}

/** One half of the SPAKE2 exchange, hex-encoded and opaque to the relay. */
export interface KeyExchangeMessage {
  type: "key_exchange";
  message: string;
}

export interface ProgressMessage {
  type: "progress";
  /** Sealed bytes, not plaintext — the relay meters what crosses it. */
  bytes_transferred: number;
  total_bytes: number;
}

export interface ErrorMessage {
  type: "error";
  message: string;
}

export interface AcknowledgementMessage {
  type: "ack";
  /** Cumulative sealed bytes the receiver has confirmed. */
  bytes_received: number;
}

export interface MetaMessage {
  type: "meta";
  version: number;
  /** The sealed length, which must match what session creation declared. */
  ciphertext_size: number;
  /** Hex of the sealed blob holding the filename, MIME type, and real size. */
  metadata: string;
}

export interface CompleteMessage {
  type: "complete";
}

export type UploadSocketMessage =
  | StatusMessage
  | KeyExchangeMessage
  | ProgressMessage
  | AcknowledgementMessage
  | ErrorMessage;

export type DownloadSocketMessage =
  | StatusMessage
  | KeyExchangeMessage
  | ProgressMessage
  | ErrorMessage
  | MetaMessage
  | CompleteMessage;
