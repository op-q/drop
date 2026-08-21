import type { SessionResponse } from "./types";

interface ApiErrorResponse {
  message?: string;
}

function normalizeOrigin(origin: string): string {
  return origin.trim().replace(/\/+$/, "");
}

function toWebSocketOrigin(origin: string): string {
  if (origin.startsWith("https://")) {
    return `wss://${origin.slice("https://".length)}`;
  }

  if (origin.startsWith("http://")) {
    return `ws://${origin.slice("http://".length)}`;
  }

  return origin.replace(/^http/, "ws");
}

const configuredBackendOrigin = import.meta.env.VITE_BACKEND_ORIGIN?.trim();
const API_BASE = normalizeOrigin(configuredBackendOrigin || window.location.origin);
const WS_BASE = toWebSocketOrigin(API_BASE);

async function readErrorMessage(response: Response): Promise<string> {
  const contentType = response.headers.get("content-type") ?? "";

  if (contentType.includes("application/json")) {
    const payload = (await response.json()) as ApiErrorResponse;
    if (payload.message) {
      return payload.message;
    }
  }

  const text = await response.text();
  return text || `request failed with status ${response.status}`;
}

// Takes the sealed size rather than the file, because the relay is told
// neither the name nor the real length. The sealed length is a deterministic
// function of the plaintext length, so an exact total can still be declared
// before a byte moves. See `docs/protocol.md`.
export async function createSession(
  ciphertextSize: number
): Promise<SessionResponse> {
  const response = await fetch(`${API_BASE}/api/session/create`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      ciphertext_size: ciphertextSize,
    }),
  });

  if (!response.ok) {
    throw new Error(await readErrorMessage(response));
  }

  return (await response.json()) as SessionResponse;
}

function openSocket(path: string, errorMessage: string): Promise<WebSocket> {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(`${WS_BASE}${path}`);
    socket.binaryType = "arraybuffer";
    socket.onopen = () => resolve(socket);
    socket.onerror = () => reject(new Error(errorMessage));
  });
}

export function openUploadSocket(code: string): Promise<WebSocket> {
  return openSocket(
    `/ws/upload/${encodeURIComponent(code)}`,
    "upload websocket failed"
  );
}

export function openDownloadSocket(code: string): Promise<WebSocket> {
  return openSocket(
    `/ws/download/${encodeURIComponent(code)}`,
    "download websocket failed"
  );
}
