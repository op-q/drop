// The encryption envelope, as the browser sees it.
//
// Every function here is the same Rust the CLI runs, compiled to WebAssembly.
// Nothing about the protocol is implemented in TypeScript, so the browser and
// the CLI cannot drift apart on the SPAKE2 transcript, the chunk framing, or
// the wordlist. See `docs/decisions.md` entry 11.
//
// What this does not do is make a browser transfer as strong as a CLI one. The
// page loads this module from the same origin that serves the JavaScript, so a
// server that will serve modified client code will serve a modified envelope
// too. Entry 7 draws that line and this does not move it.

import init, {
  Handshake,
  Metadata,
  Opener,
  Sealer,
  SessionKeys,
  TransferCode,
  chunkPlaintextBytes,
  ciphertextLen,
  envelopeVersion,
  openMetadata,
  sealMetadata,
} from "./wasm/drop_crypto_wasm.js";

export {
  Handshake,
  Metadata,
  Opener,
  Sealer,
  SessionKeys,
  TransferCode,
  ciphertextLen,
  envelopeVersion,
  openMetadata,
  sealMetadata,
};

// Instantiating the module is asynchronous and must finish before any export
// above is called. One shared promise rather than a boolean, so concurrent
// callers await the same instantiation instead of racing to start a second.
let started: Promise<unknown> | null = null;

export function loadEnvelope(): Promise<unknown> {
  started ??= init();
  return started;
}

/** Plaintext bytes per chunk. Read once, after the module is loaded. */
export function chunkSize(): number {
  return chunkPlaintextBytes();
}
