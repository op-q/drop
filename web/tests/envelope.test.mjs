// Checks the WebAssembly envelope the browser client depends on.
//
// Compiling `drop-crypto` to wasm removes the risk that the browser and the
// CLI disagree about the protocol — it is the same code, so it cannot drift.
// What it does not remove is the risk that the *build* is wrong in ways the
// Rust test suite cannot see, and those are what this file covers:
//
//   - entropy. `getrandom` needs a backend named explicitly for wasm32, and a
//     missing one is the dangerous case, because the failure mode of a stubbed
//     RNG is a key that is not random rather than an error. Two handshakes
//     producing identical messages would prove that.
//   - the JavaScript glue. Chunks cross the boundary as `Uint8Array`, and a
//     copy that truncates or shares memory wrongly would corrupt payloads.
//
// Uses `node:test` rather than a test framework: the web client has no runtime
// dependencies and this keeps it that way.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

import init, {
  Handshake,
  Metadata,
  Opener,
  Sealer,
  TransferCode,
  chunkPlaintextBytes,
  ciphertextLen,
  envelopeVersion,
  openMetadata,
  sealMetadata,
  tagBytes,
} from "../src/wasm/drop_crypto_wasm.js";

await init({
  module_or_path: await readFile(
    new URL("../src/wasm/drop_crypto_wasm_bg.wasm", import.meta.url),
  ),
});

const NAMEPLATE = "7F2A91";
const CODE = `${NAMEPLATE}-abandon-ability-able`;

/** Runs both halves of an exchange and returns the two derived key sets. */
function agree(senderCode, receiverCode) {
  const sender = Handshake.start(TransferCode.parse(senderCode));
  const receiver = Handshake.start(TransferCode.parse(receiverCode));

  const senderMessage = sender.message;
  const receiverMessage = receiver.message;

  return [sender.finish(receiverMessage), receiver.finish(senderMessage)];
}

test("a handshake draws fresh randomness on every run", () => {
  // The load-bearing test for the wasm build. If getrandom had no backend and
  // silently produced zeros, these would be equal and every transfer would
  // share one key.
  const first = Handshake.start(TransferCode.parse(CODE)).message;
  const second = Handshake.start(TransferCode.parse(CODE)).message;

  assert.notEqual(first, second, "two handshakes produced the same message");
  assert.match(first, /^[0-9a-f]+$/, "the handshake message is not hex");
});

test("generated codes differ from each other", () => {
  const first = TransferCode.generateFor(NAMEPLATE).toString();
  const second = TransferCode.generateFor(NAMEPLATE).toString();

  assert.notEqual(first, second, "two generated codes were identical");
  assert.ok(first.startsWith(`${NAMEPLATE}-`), "the nameplate was not kept");
});

test("the nameplate is the only half a relay is told", () => {
  const code = TransferCode.generateFor(NAMEPLATE);

  assert.equal(code.nameplate, NAMEPLATE);
  assert.ok(
    !code.nameplate.includes("-"),
    "the nameplate carries part of the secret",
  );
});

test("a nameplate is accepted whatever case it is typed in", () => {
  assert.equal(
    TransferCode.parse("7f2a91-abandon-ability-able").nameplate,
    NAMEPLATE,
  );
});

test("a malformed code is refused with a reason", () => {
  assert.throws(() => TransferCode.parse("7F2A91-abandon"), /word/i);
  assert.throws(
    () => TransferCode.parse("7F2A91-abandon-ability-notaword"),
    /notaword/i,
  );
});

test("matching codes open each other's metadata", () => {
  const [sender, receiver] = agree(CODE, CODE);

  const size = 4096;
  const sealedSize = ciphertextLen(size);
  const blob = sealMetadata(
    sender,
    sealedSize,
    new Metadata("holiday.jpg", "image/jpeg", size),
  );

  const opened = openMetadata(receiver, sealedSize, blob);

  assert.equal(opened.filename, "holiday.jpg");
  assert.equal(opened.mimeType, "image/jpeg");
  assert.equal(opened.plaintextSize, size);
});

test("one wrong word fails at the metadata, which is the first thing opened", () => {
  const [sender, receiver] = agree(CODE, `${NAMEPLATE}-abandon-ability-above`);

  const size = 4096;
  const sealedSize = ciphertextLen(size);
  const blob = sealMetadata(
    sender,
    sealedSize,
    new Metadata("holiday.jpg", "image/jpeg", size),
  );

  // The handshake itself succeeded on both sides — SPAKE2 does not reveal a
  // mismatch. This is where a mistyped code is actually caught.
  assert.throws(() => openMetadata(receiver, sealedSize, blob), /decrypt|code/i);
});

test("the same words under a different nameplate do not agree", () => {
  const [sender, receiver] = agree(CODE, "B4C3D2-abandon-ability-able");

  const sealedSize = ciphertextLen(64);
  const blob = sealMetadata(
    sender,
    sealedSize,
    new Metadata("a.txt", "text/plain", 64),
  );

  assert.throws(() => openMetadata(receiver, sealedSize, blob));
});

test("a multi-chunk payload round trips through the boundary", () => {
  const [sender, receiver] = agree(CODE, CODE);

  const chunkSize = chunkPlaintextBytes();
  const size = chunkSize + 1234;
  const plaintext = new Uint8Array(size);
  for (let index = 0; index < size; index += 1) {
    plaintext[index] = (index * 31) % 251;
  }

  const sealer = new Sealer(sender, size);
  const opener = new Opener(receiver, size);
  assert.equal(sealer.totalChunks, 2);

  const recovered = new Uint8Array(size);
  let written = 0;
  for (let offset = 0; offset < size; offset += chunkSize) {
    const slice = plaintext.subarray(offset, Math.min(offset + chunkSize, size));
    const opened = opener.openChunk(sealer.sealChunk(slice));
    recovered.set(opened, written);
    written += opened.length;
  }
  opener.finish();

  assert.equal(written, size, "the recovered length does not match");
  assert.deepEqual(recovered, plaintext, "the payload did not survive");
});

test("the declared sealed size matches what is actually produced", () => {
  const [sender] = agree(CODE, CODE);

  const chunkSize = chunkPlaintextBytes();
  const size = chunkSize + 1;
  const sealer = new Sealer(sender, size);

  let produced = 0;
  for (let offset = 0; offset < size; offset += chunkSize) {
    const slice = new Uint8Array(Math.min(chunkSize, size - offset));
    produced += sealer.sealChunk(slice).length;
  }

  assert.equal(produced, ciphertextLen(size));
  assert.equal(produced, size + 2 * tagBytes());
});

test("an altered chunk fails to open", () => {
  const [sender, receiver] = agree(CODE, CODE);

  const sealer = new Sealer(sender, 32);
  const opener = new Opener(receiver, 32);

  const sealed = sealer.sealChunk(new Uint8Array(32));
  sealed[0] ^= 0x01;

  assert.throws(() => opener.openChunk(sealed), /authentication|altered/i);
});

test("a truncated stream is detected even though each chunk was authentic", () => {
  const [sender, receiver] = agree(CODE, CODE);

  const chunkSize = chunkPlaintextBytes();
  const size = chunkSize * 2;
  const sealer = new Sealer(sender, size);
  const opener = new Opener(receiver, size);

  opener.openChunk(sealer.sealChunk(new Uint8Array(chunkSize)));

  assert.throws(() => opener.finish(), /ended early|1 of 2/i);
});

test("sizes that are not whole byte counts are refused at the boundary", () => {
  assert.throws(() => ciphertextLen(-1), /whole, non-negative/);
  assert.throws(() => ciphertextLen(1.5), /whole, non-negative/);
  assert.throws(() => ciphertextLen(Number.NaN), /whole, non-negative/);
  assert.throws(() => ciphertextLen(5 * 1024 * 1024 * 1024), /transfer limit/);
});

test("the envelope version is a number the relay can compare", () => {
  assert.equal(typeof envelopeVersion(), "number");
  assert.ok(Number.isInteger(envelopeVersion()));
});
