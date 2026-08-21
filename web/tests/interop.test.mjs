// Browser-to-CLI and CLI-to-browser interoperation, over a real relay.
//
// This is the Phase 4 validation item. The envelope is shared source compiled
// twice, so the protocol cannot drift — but "cannot drift" is a claim about
// the code, not about the wiring, and the wiring is what this exercises: the
// hex encoding of the control frames, the order of the handshake, the sealed
// metadata, and above all the two byte scales. The relay meters ciphertext and
// the receiver acknowledges ciphertext, while progress and what lands on disk
// are plaintext. Crossing them stalls a transfer exactly one tag short of
// finishing, which no unit test on either side would catch.
//
// The browser half runs here in Node against the same WebAssembly module the
// page loads. It is not a browser, so it does not cover the Svelte client; it
// covers the envelope and the protocol the client speaks.
//
// Skips itself when the Rust binaries are absent, so `npm test` still works
// without a Rust toolchain.

import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, mkdir, readFile, writeFile, rm, access } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import init, {
  Handshake,
  Metadata,
  Opener,
  Sealer,
  TransferCode,
  ciphertextLen,
  envelopeVersion,
  openMetadata,
  sealMetadata,
} from "../src/wasm/drop_crypto_wasm.js";

const ROOT = new URL("../../", import.meta.url).pathname;
const RELAY = join(ROOT, "target/debug/api");
const CLI = join(ROOT, "target/debug/drop");

const present = async (path) =>
  access(path).then(
    () => true,
    () => false,
  );

const haveBinaries = (await present(RELAY)) && (await present(CLI));

let relay;
let origin;
let wsOrigin;
let workDir;

before(async () => {
  if (!haveBinaries) return;

  await init({
    module_or_path: await readFile(
      new URL("../src/wasm/drop_crypto_wasm_bg.wasm", import.meta.url),
    ),
  });

  const port = 34000 + Math.floor(Math.random() * 4000);
  origin = `http://127.0.0.1:${port}`;
  wsOrigin = `ws://127.0.0.1:${port}`;
  workDir = await mkdtemp(join(tmpdir(), "drop-interop-"));

  relay = spawn(RELAY, [], {
    env: { ...process.env, DROP_BIND_ADDR: `127.0.0.1:${port}`, RUST_LOG: "warn" },
    stdio: ["ignore", "pipe", "pipe"],
  });

  // Poll rather than sleep: the relay is ready when /health answers.
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      const response = await fetch(`${origin}/health`);
      if (response.ok) return;
    } catch {
      // not up yet
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }

  throw new Error("the relay did not become ready");
});

after(async () => {
  relay?.kill("SIGTERM");
  if (workDir) await rm(workDir, { recursive: true, force: true });
});

/** The browser half of a receive, using the same wasm the page loads. */
function receiveWithEnvelope(codeText) {
  const code = TransferCode.parse(codeText);
  const socket = new WebSocket(`${wsOrigin}/ws/download/${code.nameplate}`);
  socket.binaryType = "arraybuffer";

  const handshake = Handshake.start(code);
  let keys = null;
  let opener = null;
  let sealedTotal = 0;
  let plaintextTotal = 0;
  let filename = null;
  const parts = [];

  return new Promise((resolve, reject) => {
    socket.onopen = () =>
      socket.send(
        JSON.stringify({ type: "key_exchange", message: handshake.message }),
      );
    socket.onerror = () => reject(new Error("the download socket failed"));

    socket.onmessage = (event) => {
      try {
        if (typeof event.data !== "string") {
          const sealed = new Uint8Array(event.data);
          const plaintext = opener.openChunk(sealed);
          parts.push(plaintext);
          sealedTotal += sealed.byteLength;
          plaintextTotal += plaintext.byteLength;
          socket.send(
            JSON.stringify({ type: "chunk_ack", bytes_received: sealedTotal }),
          );
          return;
        }

        const message = JSON.parse(event.data);

        if (message.type === "key_exchange") {
          keys = handshake.finish(message.message);
          return;
        }

        if (message.type === "meta") {
          assert.equal(message.version, envelopeVersion());
          const details = openMetadata(
            keys,
            message.ciphertext_size,
            message.metadata,
          );
          filename = details.filename;
          opener = new Opener(keys, details.plaintextSize);
          return;
        }

        if (message.type === "complete") {
          opener.finish();
          socket.send(
            JSON.stringify({ type: "complete", bytes_received: sealedTotal }),
          );
          const joined = new Uint8Array(plaintextTotal);
          let at = 0;
          for (const part of parts) {
            joined.set(part, at);
            at += part.byteLength;
          }
          resolve({ filename, bytes: joined });
          return;
        }

        if (message.type === "error") reject(new Error(message.message));
      } catch (error) {
        reject(error);
      }
    };
  });
}

test("CLI to browser: the browser opens what the CLI sealed", { skip: !haveBinaries }, async () => {
  const payload = Buffer.alloc(2 * 1024 * 1024 + 7);
  for (let i = 0; i < payload.length; i += 1) payload[i] = (i * 17) % 253;
  const source = join(workDir, "holiday.bin");
  await writeFile(source, payload);

  const sender = spawn(CLI, ["send", source, "--server", origin], {
    stdio: ["ignore", "pipe", "pipe"],
  });

  const codeText = await new Promise((resolve, reject) => {
    let out = "";
    sender.stdout.on("data", (d) => {
      out += d.toString();
      const line = out.split("\n").find((l) => l.trim().length > 0);
      if (line) resolve(line.trim());
    });
    sender.on("exit", (c) => reject(new Error(`sender exited early: ${c}`)));
  });

  // The code the CLI printed carries three words the relay never saw.
  assert.match(codeText, /^[0-9A-F]{6}-[a-z]+-[a-z]+-[a-z]+$/);

  const received = await receiveWithEnvelope(codeText);

  assert.equal(received.filename, "holiday.bin");
  assert.equal(received.bytes.length, payload.length);
  assert.ok(Buffer.from(received.bytes).equals(payload), "payload differs");

  await new Promise((resolve) => sender.on("exit", resolve));
});

test("browser to CLI: the CLI opens what the browser sealed", { skip: !haveBinaries }, async () => {
  const payload = Buffer.alloc(1024 * 1024 + 11);
  for (let i = 0; i < payload.length; i += 1) payload[i] = (i * 29) % 251;

  const sealedSize = ciphertextLen(payload.length);
  const created = await fetch(`${origin}/api/session/create`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ ciphertext_size: sealedSize }),
  });
  assert.ok(created.ok, `session creation failed: ${created.status}`);
  const { code: nameplate } = await created.json();

  const code = TransferCode.generateFor(nameplate);
  const socket = new WebSocket(`${wsOrigin}/ws/upload/${nameplate}`);
  socket.binaryType = "arraybuffer";

  const handshake = Handshake.start(code);
  let peerMessage = null;
  let receiverPresent = false;
  let started = false;
  let acknowledged = 0;

  const destination = join(workDir, "inbox");
  await mkdir(destination, { recursive: true });
  const receiver = spawn(
    CLI,
    ["recv", code.toString(), "--server", origin, "--out", destination],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  let receiverErr = "";
  receiver.stderr.on("data", (d) => (receiverErr += d.toString()));

  const finished = new Promise((resolve, reject) => {
    socket.onerror = () => reject(new Error("the upload socket failed"));
    receiver.on("exit", (exit) => {
      if (exit !== 0) {
        reject(new Error(`the CLI receiver exited ${exit}: ${receiverErr}`));
      }
    });

    const begin = () => {
      if (started || !receiverPresent || !peerMessage) return;
      started = true;

      const keys = handshake.finish(peerMessage);
      socket.send(
        JSON.stringify({
          type: "meta",
          version: envelopeVersion(),
          ciphertext_size: sealedSize,
          metadata: sealMetadata(
            keys,
            sealedSize,
            new Metadata("from-browser.bin", "application/octet-stream", payload.length),
          ),
        }),
      );

      const sealer = new Sealer(keys, payload.length);
      const chunk = 1024 * 1024;
      for (let offset = 0; offset < payload.length; offset += chunk) {
        const slice = payload.subarray(
          offset,
          Math.min(offset + chunk, payload.length),
        );
        socket.send(sealer.sealChunk(new Uint8Array(slice)));
      }
    };

    socket.onmessage = (event) => {
      try {
        const message = JSON.parse(event.data);

        if (message.type === "key_exchange") {
          peerMessage = message.message;
          begin();
          return;
        }

        if (message.type === "ack") {
          acknowledged = message.bytes_received;
          if (acknowledged >= sealedSize) {
            socket.send(JSON.stringify({ type: "complete" }));
          }
          return;
        }

        if (message.type === "status") {
          if (message.status === "receiver_connected") {
            receiverPresent = true;
            // Sent here rather than on open: the relay drops a key exchange
            // that arrives before the peer is connected.
            socket.send(
              JSON.stringify({
                type: "key_exchange",
                message: handshake.message,
              }),
            );
            begin();
          }
          if (message.status === "transfer_complete") resolve();
          return;
        }

        if (message.type === "error") reject(new Error(message.message));
      } catch (error) {
        reject(error);
      }
    };
  });

  await finished;

  const exitCode = await new Promise((resolve) => receiver.on("exit", resolve));
  assert.equal(exitCode, 0, `the CLI receiver failed: ${receiverErr}`);

  const landed = await readFile(join(destination, "from-browser.bin"));
  assert.ok(landed.equals(payload), "the CLI wrote different bytes");
});
