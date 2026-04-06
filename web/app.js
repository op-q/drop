const codeInput = document.getElementById("codeInput");
const uploadBtn = document.getElementById("uploadBtn");
const fileInput = document.getElementById("fileInput");
const downloadBtn = document.getElementById("downloadBtn");
const statusEl = document.getElementById("status");

const API_BASE = window.location.origin;
const WS_BASE = window.location.origin.replace(/^http/, "ws");

let uploadSocket = null;
let uploadFile = null;
let uploadCode = null;
let uploadCancelled = false;
let uploadStarted = false;
let uploadActive = false;

function setStatus(text) {
  statusEl.textContent = text;
}

function setUploading(active) {
  uploadActive = active;
  fileInput.disabled = active;

  if (active) {
    uploadBtn.textContent = "Cancel";
    uploadBtn.classList.add("is-cancel");
  } else {
    uploadBtn.textContent = "Drop a file";
    uploadBtn.classList.remove("is-cancel");
  }
}

async function createSession(file) {
  const res = await fetch(`${API_BASE}/api/session/create`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      filename: file.name,
      file_size: file.size,
      mime_type: file.type || "application/octet-stream",
    }),
  });

  if (!res.ok) {
    const text = await res.text();
    throw new Error(text || "failed to create session");
  }

  return res.json();
}

function openUploadSocket(code) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(`${WS_BASE}/ws/upload/${encodeURIComponent(code)}`);
    ws.binaryType = "arraybuffer";

    ws.onopen = () => resolve(ws);
    ws.onerror = () => reject(new Error("upload websocket failed"));
  });
}

function openDownloadSocket(code) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(`${WS_BASE}/ws/download/${encodeURIComponent(code)}`);
    ws.binaryType = "arraybuffer";

    ws.onopen = () => resolve(ws);
    ws.onerror = () => reject(new Error("download websocket failed"));
  });
}

async function sendFileChunks(ws, file, code) {
  const chunkSize = 64 * 1024;
  let offset = 0;

  setStatus(`Code: ${code}\nConnected\nSending...`);

  while (offset < file.size) {
    if (uploadCancelled || !ws || ws.readyState !== WebSocket.OPEN) {
      return;
    }

    const end = Math.min(offset + chunkSize, file.size);
    const chunk = await file.slice(offset, end).arrayBuffer();
    ws.send(chunk);
    offset = end;

    setStatus(`Code: ${code}\nConnected\nSending ${offset}/${file.size} bytes`);
    await new Promise((resolve) => setTimeout(resolve, 0));
  }

  if (!uploadCancelled && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({ type: "complete" }));
  }
}

function resetUploadState() {
  uploadSocket = null;
  uploadFile = null;
  uploadCode = null;
  uploadCancelled = false;
  uploadStarted = false;
  fileInput.disabled = false;
  fileInput.value = "";
  setUploading(false);
}

async function startUpload(file) {
  if (!file) {
    setStatus("Choose a file first.");
    return;
  }

  if (uploadActive) {
    return;
  }

  if (file.size === 0) {
    setStatus("Cannot upload an empty file.");
    return;
  }

  uploadCancelled = false;
  uploadStarted = false;
  uploadFile = file;

  setUploading(true);
  setStatus("Creating session...");

  try {
    const session = await createSession(file);
    uploadCode = session.code;

    setStatus(`Code: ${uploadCode}\nConnecting...`);
    uploadSocket = await openUploadSocket(uploadCode);

    uploadSocket.onmessage = async (event) => {
      if (typeof event.data !== "string") return;

      const msg = JSON.parse(event.data);

      if (msg.type === "status") {
        if (msg.status === "waiting_for_receiver") {
          setStatus(`Code: ${uploadCode}\nConnected\nWaiting for receiver...`);
          return;
        }

        if (msg.status === "receiver_connected") {
          setStatus(`Code: ${uploadCode}\nConnected\nReceiver connected`);

          if (!uploadStarted) {
            uploadStarted = true;

            uploadSocket.send(
              JSON.stringify({
                type: "meta",
                filename: uploadFile.name,
                file_size: uploadFile.size,
                mime_type: uploadFile.type || "application/octet-stream",
              })
            );

            await sendFileChunks(uploadSocket, uploadFile, uploadCode);
          }

          return;
        }

        if (msg.status === "sending") {
          setStatus(`Code: ${uploadCode}\nConnected\nSending...`);
          return;
        }

        if (msg.status === "transfer_complete") {
          setStatus(`Code: ${uploadCode}\nTransfer complete`);
          return;
        }

        if (msg.status === "cancelled") {
          setStatus(`Code: ${uploadCode}\nCancelled`);
          return;
        }
      }

      if (msg.type === "error") {
        setStatus(`Code: ${uploadCode}\nError: ${msg.message}`);
      }
    };

    uploadSocket.onclose = () => {
      if (uploadCancelled) {
        setStatus(uploadCode ? `Code: ${uploadCode}\nCancelled` : "Cancelled");
      }
      resetUploadState();
    };

    setStatus(`Code: ${uploadCode}\nConnected`);
  } catch (err) {
    setStatus(`Upload error: ${err.message}`);
    resetUploadState();
  }
}

function cancelUpload() {
  if (!uploadActive) return;

  uploadCancelled = true;
  setStatus(uploadCode ? `Code: ${uploadCode}\nCancelling...` : "Cancelling...");

  if (uploadSocket && uploadSocket.readyState === WebSocket.OPEN) {
    uploadSocket.send(JSON.stringify({ type: "cancel" }));
    uploadSocket.close();
  } else {
    resetUploadState();
    setStatus("Cancelled");
  }
}

async function startDownload() {
  const code = codeInput.value.trim();
  if (!code) {
    setStatus("Enter a code first.");
    return;
  }

  setStatus("Connecting download socket...");

  try {
    const ws = await openDownloadSocket(code);

    let filename = "download.bin";
    let mimeType = "application/octet-stream";
    let total = 0;
    const chunks = [];

    ws.onmessage = (event) => {
      if (typeof event.data === "string") {
        const msg = JSON.parse(event.data);

        if (msg.type === "status") {
          if (msg.status === "waiting_for_sender") {
            setStatus("Connected\nWaiting for sender...");
            return;
          }
        }

        if (msg.type === "meta") {
          filename = msg.filename || filename;
          mimeType = msg.mime_type || mimeType;
          setStatus(`Receiving ${filename}...`);
          return;
        }

        if (msg.type === "complete") {
          const blob = new Blob(chunks, { type: mimeType });
          const url = URL.createObjectURL(blob);

          const a = document.createElement("a");
          a.href = url;
          a.download = filename;
          document.body.appendChild(a);
          a.click();
          a.remove();

          URL.revokeObjectURL(url);
          setStatus(`Download complete. ${total} bytes received.`);
          ws.close();
          return;
        }

        if (msg.type === "error") {
          setStatus(`Download error: ${msg.message}`);
          ws.close();
        }

        return;
      }

      chunks.push(event.data);
      total += event.data.byteLength || 0;
      setStatus(`Receiving ${total} bytes...`);
    };

    ws.onclose = () => {};
  } catch (err) {
    setStatus(`Download error: ${err.message}`);
  }
}

function handleDroppedFiles(files) {
  if (!files || files.length === 0) return;

  const file = files[0];
  if (file && !uploadActive) {
    startUpload(file);
  }
}

uploadBtn.addEventListener("click", (event) => {
  event.preventDefault();
  event.stopPropagation();

  if (uploadActive) {
    cancelUpload();
  } else {
    fileInput.click();
  }
});

fileInput.addEventListener("change", () => {
  const file = fileInput.files[0];
  if (file) {
    startUpload(file);
  }
});

downloadBtn.addEventListener("click", startDownload);

window.addEventListener("dragenter", (event) => {
  event.preventDefault();
  document.body.classList.add("dragging");
});

window.addEventListener("dragover", (event) => {
  event.preventDefault();
});

window.addEventListener("dragleave", (event) => {
  event.preventDefault();

  if (event.target === document.body || event.target === document.documentElement) {
    document.body.classList.remove("dragging");
  }
});

window.addEventListener("drop", (event) => {
  event.preventDefault();
  document.body.classList.remove("dragging");
  handleDroppedFiles(event.dataTransfer.files);
});

window.addEventListener("beforeunload", () => {
  if (uploadSocket && uploadSocket.readyState === WebSocket.OPEN) {
    uploadSocket.close();
  }
});