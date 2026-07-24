<script lang="ts">
  import { createSession, openDownloadSocket, openUploadSocket } from "./api";
  import type {
    DownloadSocketMessage,
    StatusState,
    UploadSocketMessage,
  } from "./types";

  const CHUNK_SIZE = 64 * 1024;

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
    write(chunk: ArrayBuffer): Promise<void>;
    complete(filename: string, mimeType: string): Promise<void>;
    abort(): Promise<void>;
  }

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
    codeInput = codeInput.toUpperCase().replace(/\s+/g, "");
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
        async write(chunk: ArrayBuffer): Promise<void> {
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

          URL.revokeObjectURL(url);
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
    let writeQueue = Promise.resolve();

    return {
      kind: "disk",
      async write(chunk: ArrayBuffer): Promise<void> {
        writeQueue = writeQueue.then(() => writable.write(new Uint8Array(chunk)));
        await writeQueue;
      },
      async complete(): Promise<void> {
        await writeQueue;
        await writable.close();
      },
      async abort(): Promise<void> {
        try {
          await writable.abort();
        } catch {
          await writable.close().catch(() => undefined);
        }
      },
    };
  }

  async function sendFileChunks(
    socket: WebSocket,
    file: File,
    code: string
  ): Promise<void> {
    let offset = 0;
    setStatus(["Upload started.", "Your file is on the way."], code);

    while (offset < file.size) {
      if (
        uploadCancelled ||
        uploadSocket !== socket ||
        socket.readyState !== WebSocket.OPEN
      ) {
        return;
      }

      const end = Math.min(offset + CHUNK_SIZE, file.size);
      const chunk = await file.slice(offset, end).arrayBuffer();
      socket.send(chunk);
      offset = end;
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    }

    if (!uploadCancelled && socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify({ type: "complete" }));
    }
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
      const session = await createSession(file);
      uploadCode = session.code;
      setStatus(
        ["Transfer code created.", "Connecting to the transfer service..."],
        session.code
      );

      const socket = await openUploadSocket(session.code);
      uploadSocket = socket;

      socket.onmessage = async (event: MessageEvent<string>) => {
        if (typeof event.data !== "string") {
          return;
        }

        const message = JSON.parse(event.data) as UploadSocketMessage;

        if (message.type === "error") {
          setStatus(["Upload stopped.", message.message], uploadCode);
          return;
        }

        if (message.type === "progress") {
          setStatus(
            [
              "Uploading your file...",
              formatProgress(
                "Uploaded",
                message.bytes_transferred,
                message.total_bytes
              ),
            ],
            uploadCode
          );
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

          if (!uploadStarted && uploadFile) {
            uploadStarted = true;
            socket.send(
              JSON.stringify({
                type: "meta",
                filename: uploadFile.name,
                file_size: uploadFile.size,
                mime_type: uploadFile.type || "application/octet-stream",
              })
            );

            await sendFileChunks(socket, uploadFile, session.code);
          }

          return;
        }

        if (message.status === "sending") {
          setStatus(
            ["Upload in progress.", "Your file is being transferred."],
            uploadCode
          );
          return;
        }

        if (message.status === "transfer_complete") {
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
    const code = codeInput.trim();

    if (!code) {
      setStatus(["Enter a transfer code first."], null);
      return;
    }

    setStatus(["Connecting to the transfer...", "Waiting for the sender."], null);

    let downloadTarget: DownloadTarget | null = null;

    try {
      downloadTarget = await createDownloadTarget(code);
      const socket = await openDownloadSocket(code);
      let filename = "download.bin";
      let mimeType = "application/octet-stream";
      let total = 0;
      let expectedTotal = 0;

      socket.onmessage = async (event: MessageEvent<ArrayBuffer | string>) => {
        if (typeof event.data === "string") {
          const message = JSON.parse(event.data) as DownloadSocketMessage;

          if (message.type === "progress") {
            expectedTotal = message.total_bytes;
            setStatus(
              [
                filename === "download.bin"
                  ? "Receiving your file..."
                  : `Receiving ${filename}...`,
                formatProgress(
                  "Downloaded",
                  message.bytes_transferred,
                  message.total_bytes
                ),
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
            filename = message.filename || filename;
            mimeType = message.mime_type || mimeType;
            expectedTotal = message.file_size;
            setStatus(
              [
                `Receiving ${filename}...`,
                `File size: ${formatBytes(message.file_size)}`,
              ],
              null
            );
            return;
          }

          if (message.type === "complete") {
            await downloadTarget.complete(filename, mimeType);
            setStatus(
              [
                "Download complete.",
                `${filename} received successfully (${formatBytes(total)}).`,
              ],
              null
            );
            socket.close();
            return;
          }

          if (message.type === "error") {
            await downloadTarget.abort();
            setStatus(["Download stopped.", message.message], null);
            socket.close();
          }

          return;
        }

        await downloadTarget.write(event.data);
        total += event.data.byteLength;

        if (expectedTotal === 0) {
          setStatus(
            [
              filename === "download.bin"
                ? "Receiving your file..."
                : `Receiving ${filename}...`,
              `Downloaded ${formatBytes(total)} so far.`,
            ],
            null
          );
        }
      };

      socket.onclose = () => {
        downloadTarget = null;
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
