<script lang="ts">
  import { onMount } from "svelte";
  import { createSession, openDownloadSocket, openUploadSocket } from "./api";
  import {
    Handshake,
    Metadata,
    Opener,
    Sealer,
    SessionKeys,
    TransferCode,
    ciphertextLen,
    envelopeVersion,
    loadEnvelope,
    openMetadata,
    sealMetadata,
  } from "./envelope";
  import type {
    DownloadSocketMessage,
    StatusState,
    UploadSocketMessage,
  } from "./types";

  // Matches the relay's RECOMMENDED_CHUNK_BYTES. Larger chunks mean far fewer
  // frames, wakeups, and control messages for the same number of file bytes.
  const CHUNK_SIZE = 1024 * 1024;
  // The window of unacknowledged bytes is what sets throughput on a
  // high-latency link: the ceiling is roughly this divided by the round trip
  // time, no matter how much bandwidth is available.
  const MAX_IN_FLIGHT_BYTES = 16 * 1024 * 1024;
  const WS_BUFFER_HIGH_WATER_BYTES = MAX_IN_FLIGHT_BYTES;
  // Acknowledge in batches rather than per chunk. This must stay well below
  // MAX_IN_FLIGHT_BYTES, or the sender would wait on an acknowledgement that
  // only a later chunk could trigger.
  const ACK_INTERVAL_BYTES = 4 * 1024 * 1024;
  const MAX_MEMORY_DOWNLOAD_BYTES = 256 * 1024 * 1024;

  interface FilePickerWindow extends Window {
    showSaveFilePicker?: (options?: {
      suggestedName?: string;
    }) => Promise<FileSystemFileHandle>;
  }

  interface FileSystemFileHandle {
    createWritable(): Promise<FileSystemWritableFileStream>;
  }

  interface FileSystemWritableFileStream {
    write(data: BlobPart): Promise<void>;
    close(): Promise<void>;
    abort(reason?: unknown): Promise<void>;
  }

  interface DownloadTarget {
    kind: "disk" | "memory";
    /** Opened plaintext, not the sealed bytes that arrived. */
    write(chunk: Uint8Array): Promise<void>;
    complete(filename: string, mimeType: string): Promise<void>;
    abort(): Promise<void>;
  }

  interface UploadFlowControl {
    // Sealed bytes, not plaintext. The relay meters ciphertext and the
    // receiver acknowledges ciphertext, so the window has to be counted in the
    // same units or the sender waits for an acknowledgement that never comes.
    acknowledgedBytes: number;
    error: Error | null;
    wake: (() => void) | null;
  }

  // Instantiating the WebAssembly envelope is asynchronous. Start it as soon
  // as the page is up so the first transfer does not pay for it.
  onMount(() => {
    void loadEnvelope().catch(() => undefined);
  });

  let codeInput = "";
  let fileInput: HTMLInputElement | null = null;
  let uploadActive = false;
  let uploadCancelled = false;
  let uploadStarted = false;
  let uploadCode: string | null = null;
  let uploadFile: File | null = null;
  let uploadSocket: WebSocket | null = null;
  let isDragActive = false;
  let dragDepth = 0;
  let status: StatusState = {
    code: null,
    lines: ["Idle"],
  };

  function setStatus(lines: string[], code: string | null = status.code): void {
    status = { code, lines };
  }

  function formatBytes(bytes: number): string {
    if (bytes < 1024) {
      return `${bytes} B`;
    }

    const units = ["KB", "MB", "GB", "TB"];
    let value = bytes;
    let unitIndex = -1;

    while (value >= 1024 && unitIndex < units.length - 1) {
      value /= 1024;
      unitIndex += 1;
    }

    return `${value.toFixed(value >= 10 ? 1 : 2)} ${units[unitIndex]}`;
  }

  function formatProgress(
    label: string,
    bytesTransferred: number,
    totalBytes: number
  ): string {
    const percent =
      totalBytes > 0
        ? Math.min(100, Math.round((bytesTransferred / totalBytes) * 100))
        : 0;

    return `${label} ${formatBytes(bytesTransferred)} of ${formatBytes(
      totalBytes
    )} (${percent}%)`;
  }

  function friendlyError(prefix: string, error: unknown): string {
    return `${prefix}: ${
      error instanceof Error ? error.message : "something went wrong"
    }`;
  }

  function normalizeCodeInput(): void {
    // Only whitespace is stripped here. Case is left alone because a code is
    // now a nameplate plus three lowercase words, and upper-casing the whole
    // string would corrupt the half that is the password. `TransferCode.parse`
    // normalizes each half the way that half needs.
    codeInput = codeInput.trim();
  }

  function resetUploadState(): void {
    uploadActive = false;
    uploadCancelled = false;
    uploadStarted = false;
    uploadCode = null;
    uploadFile = null;
    uploadSocket = null;
    isDragActive = false;
    dragDepth = 0;

    if (fileInput) {
      fileInput.value = "";
    }
  }

  function syncFileInput(file: File | null): void {
    if (!fileInput) {
      return;
    }

    if (!file) {
      fileInput.value = "";
      return;
    }

    const dataTransfer = new DataTransfer();
    dataTransfer.items.add(file);
    fileInput.files = dataTransfer.files;
  }

  async function createDownloadTarget(code: string): Promise<DownloadTarget> {
    const pickerWindow = window as FilePickerWindow;

    if (!pickerWindow.showSaveFilePicker) {
      const chunks: BlobPart[] = [];

      return {
        kind: "memory",
        async write(chunk: Uint8Array): Promise<void> {
          chunks.push(chunk);
        },
        async complete(filename: string, mimeType: string): Promise<void> {
          const blob = new Blob(chunks, { type: mimeType });
          const url = URL.createObjectURL(blob);
          const link = document.createElement("a");

          link.href = url;
          link.download = filename;
          document.body.appendChild(link);
          link.click();
          link.remove();

          chunks.length = 0;
          window.setTimeout(() => URL.revokeObjectURL(url), 60_000);
        },
        async abort(): Promise<void> {
          chunks.length = 0;
        },
      };
    }

    const handle = await pickerWindow.showSaveFilePicker({
      suggestedName: `drop-${code}`,
    });
    const writable = await handle.createWritable();

    return {
      kind: "disk",
      async write(chunk: Uint8Array): Promise<void> {
        await writable.write(chunk);
      },
      async complete(): Promise<void> {
        await writable.close();
      },
      async abort(): Promise<void> {
        await writable.abort().catch(() => undefined);
      },
    };
  }

  function signalUploadFlow(flow: UploadFlowControl): void {
    const wake = flow.wake;
    flow.wake = null;
    wake?.();
  }

  async function waitForUploadSignal(flow: UploadFlowControl): Promise<void> {
    await new Promise<void>((resolve) => {
      let settled = false;

      const finish = (): void => {
        if (settled) {
          return;
        }

        settled = true;
        window.clearTimeout(timer);
        if (flow.wake === finish) {
          flow.wake = null;
        }
        resolve();
      };

      const timer = window.setTimeout(finish, 25);
      flow.wake = finish;
    });
  }

  function assertUploadCanContinue(
    socket: WebSocket,
    flow: UploadFlowControl
  ): void {
    if (flow.error) {
      throw flow.error;
    }

    if (
      uploadCancelled ||
      uploadSocket !== socket ||
      socket.readyState !== WebSocket.OPEN
    ) {
      throw new Error("upload connection closed before the transfer completed");
    }
  }

  async function waitForUploadCapacity(
    socket: WebSocket,
    flow: UploadFlowControl,
    bytesAfterNextChunk: number
  ): Promise<void> {
    while (
      bytesAfterNextChunk - flow.acknowledgedBytes > MAX_IN_FLIGHT_BYTES ||
      socket.bufferedAmount > WS_BUFFER_HIGH_WATER_BYTES
    ) {
      assertUploadCanContinue(socket, flow);
      await waitForUploadSignal(flow);
    }

    assertUploadCanContinue(socket, flow);
  }

  async function sendFileChunks(
    socket: WebSocket,
    file: File,
    sealer: Sealer,
    sealedSize: number,
    code: string,
    flow: UploadFlowControl
  ): Promise<void> {
    let offset = 0;
    let sealedSent = 0;
    setStatus(["Upload started.", "Your file is on the way."], code);

    while (offset < file.size) {
      const end = Math.min(offset + CHUNK_SIZE, file.size);
      // Sealed before the capacity check rather than after, because the
      // window is measured in sealed bytes and the sealed length is only
      // known once the chunk exists. One chunk is held in memory either way.
      const plaintext = new Uint8Array(
        await file.slice(offset, end).arrayBuffer()
      );
      const sealed = sealer.sealChunk(plaintext);

      await waitForUploadCapacity(socket, flow, sealedSent + sealed.byteLength);
      assertUploadCanContinue(socket, flow);
      socket.send(sealed);
      sealedSent += sealed.byteLength;
      offset = end;

      setStatus(
        [
          "Uploading your file...",
          formatProgress("Uploaded", offset, file.size),
        ],
        code
      );
    }

    // Sealed bytes again: this is the receiver's cumulative count.
    while (flow.acknowledgedBytes < sealedSize) {
      assertUploadCanContinue(socket, flow);
      await waitForUploadSignal(flow);
    }

    assertUploadCanContinue(socket, flow);
    socket.send(JSON.stringify({ type: "complete" }));
  }

  async function startUpload(file: File | null): Promise<void> {
    if (!file) {
      setStatus(["Choose a file first."], null);
      return;
    }

    if (uploadActive) {
      return;
    }

    if (file.size === 0) {
      setStatus(["Cannot upload an empty file."], null);
      return;
    }

    uploadActive = true;
    uploadCancelled = false;
    uploadStarted = false;
    uploadFile = file;
    setStatus(["Preparing your transfer...", "Creating a one-time code."], null);

    try {
      await loadEnvelope();

      // The relay is told the sealed size and nothing else. The name, the MIME
      // type, and the real length travel inside the sealed blob.
      const sealedSize = ciphertextLen(file.size);
      const session = await createSession(sealedSize);

      // The relay allocated the nameplate. The three secret words are drawn
      // here, in the browser, and are never sent anywhere.
      const code = TransferCode.generateFor(session.code);
      uploadCode = code.toString();
      setStatus(
        ["Transfer code created.", "Connecting to the transfer service..."],
        uploadCode
      );

      const socket = await openUploadSocket(session.code);
      uploadSocket = socket;
      const flow: UploadFlowControl = {
        acknowledgedBytes: 0,
        error: null,
        wake: null,
      };
      let transferComplete = false;

      const failUpload = (error: Error): void => {
        if (uploadCancelled || transferComplete) {
          return;
        }

        flow.error ??= error;
        signalUploadFlow(flow);
        setStatus(["Upload stopped.", flow.error.message], uploadCode);

        if (socket.readyState === WebSocket.OPEN) {
          socket.send(JSON.stringify({ type: "cancel" }));
          socket.close();
        }
      };

      // Key agreement runs before anything is sealed. Either peer may connect
      // first, so the transfer starts only once both preconditions hold: the
      // receiver is present, and its half of the exchange has arrived.
      // Not sent yet. The relay forwards a key exchange to a peer that is
      // already connected and drops one that arrives before the peer exists,
      // so a sender that sends on connect strands a receiver waiting for a
      // half it will never get. `receiver_connected` is the signal that the
      // other end is there to receive it.
      const handshake = Handshake.start(code);
      let peerMessage: string | null = null;
      let receiverPresent = false;
      let sentKeyExchange = false;

      const beginTransfer = (): void => {
        if (uploadStarted || !receiverPresent || !peerMessage || !uploadFile) {
          return;
        }

        uploadStarted = true;
        const payload = uploadFile;

        try {
          // Consumes the handshake. It is single-use, and the guard above is
          // what stops this running twice.
          const keys: SessionKeys = handshake.finish(peerMessage);

          socket.send(
            JSON.stringify({
              type: "meta",
              version: envelopeVersion(),
              ciphertext_size: sealedSize,
              metadata: sealMetadata(
                keys,
                sealedSize,
                new Metadata(
                  payload.name,
                  payload.type || "application/octet-stream",
                  payload.size
                )
              ),
            })
          );

          void sendFileChunks(
            socket,
            payload,
            new Sealer(keys, payload.size),
            sealedSize,
            uploadCode ?? session.code,
            flow
          ).catch((error: unknown) => {
            failUpload(
              error instanceof Error
                ? error
                : new Error("the upload could not continue")
            );
          });
        } catch (error: unknown) {
          failUpload(
            error instanceof Error
              ? error
              : new Error("the key exchange could not be completed")
          );
        }
      };

      socket.onmessage = (event: MessageEvent<string>) => {
        if (typeof event.data !== "string") {
          return;
        }

        let message: UploadSocketMessage;
        try {
          message = JSON.parse(event.data) as UploadSocketMessage;
        } catch {
          failUpload(new Error("the server sent an invalid response"));
          return;
        }

        if (message.type === "error") {
          flow.error = new Error(message.message);
          signalUploadFlow(flow);
          setStatus(["Upload stopped.", message.message], uploadCode);
          return;
        }

        if (message.type === "key_exchange") {
          peerMessage = message.message;
          beginTransfer();
          return;
        }

        if (message.type === "ack") {
          if (
            message.bytes_received < flow.acknowledgedBytes ||
            message.bytes_received > sealedSize
          ) {
            failUpload(new Error("the receiver sent an invalid acknowledgement"));
            return;
          }

          flow.acknowledgedBytes = message.bytes_received;
          signalUploadFlow(flow);
          return;
        }

        if (message.type === "progress") {
          // The relay counts sealed bytes. The status line reports plaintext,
          // which `sendFileChunks` already keeps current, so these numbers are
          // deliberately dropped rather than shown in the wrong unit.
          return;
        }

        if (message.status === "waiting_for_receiver") {
          setStatus(
            [
              "Ready to send.",
              "Share the code with the receiver and keep this page open.",
            ],
            uploadCode
          );
          return;
        }

        if (message.status === "receiver_connected") {
          setStatus(
            ["Receiver connected.", "Starting the upload now..."],
            uploadCode
          );

          receiverPresent = true;

          if (!sentKeyExchange) {
            sentKeyExchange = true;
            socket.send(
              JSON.stringify({
                type: "key_exchange",
                message: handshake.message,
              })
            );
          }

          beginTransfer();
          return;
        }

        if (message.status === "sending") {
          setStatus(
            ["Upload in progress.", "Your file is being transferred."],
            uploadCode
          );
          return;
        }

        if (message.status === "awaiting_receiver") {
          setStatus(
            [
              "Upload delivered.",
              "Waiting for the receiver to finish saving the file...",
            ],
            uploadCode
          );
          return;
        }

        if (message.status === "transfer_complete") {
          transferComplete = true;
          setStatus(
            ["Upload complete.", "The transfer finished successfully."],
            uploadCode
          );
          return;
        }

        if (message.status === "cancelled") {
          setStatus(["Transfer cancelled.", "No file was sent."], uploadCode);
        }
      };

      socket.onclose = () => {
        if (!transferComplete && !uploadCancelled && !flow.error) {
          flow.error = new Error(
            "upload connection closed before the receiver confirmed the file"
          );
          setStatus(["Upload stopped.", flow.error.message], uploadCode);
        }
        signalUploadFlow(flow);

        if (uploadCancelled) {
          setStatus(
            ["Transfer cancelled.", "You can start a new upload anytime."],
            uploadCode
          );
        }
        resetUploadState();
      };
    } catch (error) {
      setStatus([friendlyError("Upload error", error)], uploadCode);
      resetUploadState();
    }
  }

  function cancelUpload(): void {
    if (!uploadActive) {
      return;
    }

    uploadCancelled = true;
    setStatus(["Cancelling transfer...", "Please wait a moment."], uploadCode);

    if (uploadSocket && uploadSocket.readyState === WebSocket.OPEN) {
      uploadSocket.send(JSON.stringify({ type: "cancel" }));
      uploadSocket.close();
      return;
    }

    resetUploadState();
    setStatus(["Transfer cancelled.", "You can start a new upload anytime."], null);
  }

  async function startDownload(): Promise<void> {
    normalizeCodeInput();
    const typed = codeInput.trim();

    if (!typed) {
      setStatus(["Enter a transfer code first."], null);
      return;
    }

    setStatus(["Connecting to the transfer...", "Waiting for the sender."], null);

    let downloadTarget: DownloadTarget | null = null;

    try {
      await loadEnvelope();

      // Parsed before the relay is contacted, so a mistyped code is reported
      // as a typing mistake rather than as a failed connection. Only the
      // nameplate is ever sent; the three words stay here as the password for
      // the key exchange.
      let code: TransferCode;
      try {
        code = TransferCode.parse(typed);
      } catch (error: unknown) {
        setStatus(
          [
            "That code does not look right.",
            error instanceof Error
              ? error.message
              : "check the code and try again",
          ],
          null
        );
        return;
      }

      const nameplate = code.nameplate;
      downloadTarget = await createDownloadTarget(nameplate);
      const target = downloadTarget;
      const socket = await openDownloadSocket(nameplate);

      const handshake = Handshake.start(code);
      let keys: SessionKeys | null = null;
      let opener: Opener | null = null;

      let filename = "download.bin";
      let mimeType = "application/octet-stream";
      // Two totals, deliberately. `sealedTotal` is what the relay meters and
      // what an acknowledgement carries. `total` is plaintext, and is what
      // reaches the disk and the progress line. Crossing them stalls the
      // transfer one authentication tag short of finishing.
      let sealedTotal = 0;
      let total = 0;
      let expectedTotal = 0;
      let expectedSealedTotal = 0;
      let unacknowledgedBytes = 0;
      let finalized = false;
      let failed = false;
      let messageQueue = Promise.resolve();

      const abortDownload = async (
        error: Error,
        notifyServer: boolean
      ): Promise<void> => {
        if (failed || finalized) {
          return;
        }

        failed = true;

        if (notifyServer && socket.readyState === WebSocket.OPEN) {
          socket.send(JSON.stringify({ type: "error" }));
        }

        await target.abort().catch(() => undefined);
        setStatus(["Download stopped.", error.message], null);

        if (socket.readyState === WebSocket.OPEN) {
          socket.close();
        }
      };

      const handleDownloadMessage = async (
        event: MessageEvent<ArrayBuffer | string>
      ): Promise<void> => {
        if (failed || finalized) {
          return;
        }

        if (typeof event.data === "string") {
          const message = JSON.parse(event.data) as DownloadSocketMessage;

          if (message.type === "key_exchange") {
            // Replied to here rather than sent on open: the relay drops a key
            // exchange that arrives before the peer is connected, so a receiver
            // that connects first would have its half discarded and the sender
            // would wait for it forever.
            socket.send(
              JSON.stringify({
                type: "key_exchange",
                message: handshake.message,
              })
            );

            // Completing this does not prove the sender knew the code. A wrong
            // code yields a well-formed message and a different key, caught
            // when the sealed details below fail to open.
            keys = handshake.finish(message.message);
            return;
          }

          if (message.type === "progress") {
            expectedSealedTotal = message.total_bytes;
            setStatus(
              [
                filename === "download.bin"
                  ? "Receiving your file..."
                  : `Receiving ${filename}...`,
                formatProgress("Saved", total, message.total_bytes),
              ],
              null
            );
            return;
          }

          if (message.type === "status" && message.status === "waiting_for_sender") {
            setStatus(
              ["Connected.", "Waiting for the sender to begin uploading..."],
              null
            );
            return;
          }

          if (message.type === "meta") {
            if (!keys) {
              throw new Error(
                "the transfer details arrived before the key exchange"
              );
            }

            // A version mismatch is fatal on purpose. A version that can be
            // negotiated downward is one a hostile relay steers to plaintext.
            if (message.version !== envelopeVersion()) {
              throw new Error(
                `the sender is using envelope version ${message.version}, which this page cannot read — update drop at both ends`
              );
            }

            // The first sealed thing opened, and so where a mistyped code is
            // actually caught: before a byte is written and before any of the
            // payload is touched.
            const details = openMetadata(
              keys,
              message.ciphertext_size,
              message.metadata
            );

            filename = details.filename || filename;
            mimeType = details.mimeType || mimeType;
            expectedTotal = details.plaintextSize;
            expectedSealedTotal = message.ciphertext_size;
            opener = new Opener(keys, expectedTotal);

            if (
              target.kind === "memory" &&
              expectedTotal > MAX_MEMORY_DOWNLOAD_BYTES
            ) {
              throw new Error(
                `this browser cannot safely buffer files over ${formatBytes(
                  MAX_MEMORY_DOWNLOAD_BYTES
                )}; use a browser with direct-to-disk download support`
              );
            }

            setStatus(
              [
                `Receiving ${filename}...`,
                `File size: ${formatBytes(expectedTotal)}`,
              ],
              null
            );
            return;
          }

          if (message.type === "complete") {
            if (!opener || expectedTotal === 0 || total !== expectedTotal) {
              throw new Error(
                `received ${formatBytes(total)} but expected ${formatBytes(
                  expectedTotal
                )}`
              );
            }

            // Each chunk authenticating on its own does not mean the stream
            // was not cut short. This is what catches a truncated transfer.
            opener.finish();

            await target.complete(filename, mimeType);
            finalized = true;

            if (socket.readyState !== WebSocket.OPEN) {
              setStatus(
                [
                  "Download saved.",
                  "The file was saved, but confirmation to the sender failed.",
                ],
                null
              );
              return;
            }

            socket.send(
              JSON.stringify({
                type: "complete",
                // Sealed bytes: the relay confirms against what crossed it.
                bytes_received: sealedTotal,
              })
            );
            setStatus(
              [
                "Download complete.",
                `${filename} received successfully (${formatBytes(total)}).`,
              ],
              null
            );
            return;
          }

          if (message.type === "error") {
            throw new Error(message.message);
          }

          return;
        }

        if (!opener) {
          throw new Error("received file bytes before the transfer details");
        }

        const sealed = new Uint8Array(event.data);

        if (sealedTotal + sealed.byteLength > expectedSealedTotal) {
          throw new Error("received more bytes than the declared transfer size");
        }

        // Fails if the chunk was altered, reordered, duplicated, or sealed
        // under a different key. Nothing reaches the target unopened.
        const plaintext = opener.openChunk(sealed);

        await target.write(plaintext);
        sealedTotal += sealed.byteLength;
        total += plaintext.byteLength;
        unacknowledgedBytes += sealed.byteLength;

        if (socket.readyState !== WebSocket.OPEN) {
          throw new Error("download connection closed while saving the file");
        }

        // Acknowledging every chunk cost a control message per chunk. Batch
        // them, but always flush once the whole file has arrived: the sender
        // waits for the final acknowledgement, and a tail smaller than the
        // batch would otherwise never trigger one.
        if (
          unacknowledgedBytes >= ACK_INTERVAL_BYTES ||
          sealedTotal === expectedSealedTotal
        ) {
          unacknowledgedBytes = 0;
          socket.send(
            JSON.stringify({
              type: "chunk_ack",
              // Cumulative sealed bytes. The sender's window is released
              // against this number, so it must be in the relay's units.
              bytes_received: sealedTotal,
            })
          );
        }

        setStatus(
          [
            filename === "download.bin"
              ? "Receiving your file..."
              : `Receiving ${filename}...`,
            formatProgress("Saved", total, expectedTotal),
          ],
          null
        );
      };

      socket.onmessage = (event: MessageEvent<ArrayBuffer | string>) => {
        messageQueue = messageQueue
          .then(() => handleDownloadMessage(event))
          .catch((error: unknown) =>
            abortDownload(
              error instanceof Error
                ? error
                : new Error("the download could not continue"),
              true
            )
          );
      };

      socket.onclose = () => {
        messageQueue = messageQueue.then(async () => {
          if (!finalized && !failed) {
            await abortDownload(
              new Error("download connection closed before completion"),
              false
            );
          }
          downloadTarget = null;
        });
      };
    } catch (error) {
      if (downloadTarget) {
        await downloadTarget.abort();
      }

      setStatus([friendlyError("Download error", error)], null);
    }
  }

  function handleDroppedFiles(files: FileList | null): void {
    if (!files || files.length === 0 || uploadActive) {
      return;
    }

    const file = files[0] ?? null;
    syncFileInput(file);
    void startUpload(file);
  }

  function eventHasFiles(event: DragEvent): boolean {
    return Array.from(event.dataTransfer?.types ?? []).includes("Files");
  }

  function handlePageDragEnter(event: DragEvent): void {
    if (uploadActive || !eventHasFiles(event)) {
      return;
    }

    dragDepth += 1;
    isDragActive = true;
  }

  function handlePageDragLeave(event: DragEvent): void {
    if (uploadActive || !eventHasFiles(event)) {
      return;
    }

    dragDepth = Math.max(0, dragDepth - 1);

    if (dragDepth === 0) {
      isDragActive = false;
    }
  }

  function handlePageDrop(event: DragEvent): void {
    if (uploadActive || !eventHasFiles(event)) {
      return;
    }

    dragDepth = 0;
    isDragActive = false;
    handleDroppedFiles(event.dataTransfer?.files ?? null);
  }
</script>

<svelte:window
  on:dragenter|preventDefault={handlePageDragEnter}
  on:dragover|preventDefault
  on:dragleave|preventDefault={handlePageDragLeave}
  on:drop|preventDefault={handlePageDrop}
/>

<div class="header-wrapper">
  <h1 class="title">drop</h1>
  <p class="subtitle">Share files instantly with a one-time code, <br /> nothing is stored</p>
</div>

<div class="download-wrapper">
  <input
    bind:value={codeInput}
    class="download-input"
    type="text"
    autocomplete="off"
    autocapitalize="characters"
    placeholder="Enter code"
    on:input={normalizeCodeInput}
    on:keydown={(event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        void startDownload();
      }
    }}
  />
  <button class="download-btn" type="button" on:click={() => void startDownload()}>
    Download
  </button>
</div>

<div
  class="upload-wrapper"
  class:is-dragover={isDragActive}
  role="region"
  aria-label="Upload area"
  on:dragenter|preventDefault={() => {
    if (!uploadActive) {
      isDragActive = true;
    }
  }}
  on:dragover|preventDefault={() => {
    if (!uploadActive) {
      isDragActive = true;
    }
  }}
  on:dragleave={(event) => {
    if (event.currentTarget === event.target) {
      isDragActive = false;
    }
  }}
  on:drop|preventDefault={(event) => {
    isDragActive = false;
    handleDroppedFiles(event.dataTransfer?.files ?? null);
  }}
>
  <input
    bind:this={fileInput}
    class="upload-input"
    type="file"
    hidden
    disabled={uploadActive}
    on:change={() => void startUpload(fileInput?.files?.[0] ?? null)}
  />

  <button
    class="upload-btn"
    class:is-cancel={uploadActive}
    type="button"
    on:click={() => {
      if (uploadActive) {
        cancelUpload();
      } else {
        fileInput?.click();
      }
    }}
  >
    {uploadActive ? "Cancel" : "Drop a file"}
  </button>

  <pre class="status" aria-live="polite"
    >{#if status.code}<span class="status-code">Code: {status.code}</span>{/if}{#each status.lines as line}
{line}{/each}</pre
  >
</div>

<a
  class="github"
  target="_blank"
  rel="noreferrer"
  href="https://github.com/op-q/drop"
  >github.com/op-q/drop</a
>
