export interface SessionResponse {
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

export interface ProgressMessage {
  type: "progress";
  bytes_transferred: number;
  total_bytes: number;
}

export interface ErrorMessage {
  type: "error";
  message: string;
}

export interface AcknowledgementMessage {
  type: "ack";
  bytes_received: number;
}

export interface MetaMessage {
  type: "meta";
  filename: string;
  file_size: number;
  mime_type: string;
}

export interface CompleteMessage {
  type: "complete";
}

export type UploadSocketMessage =
  | StatusMessage
  | ProgressMessage
  | AcknowledgementMessage
  | ErrorMessage;
export type DownloadSocketMessage =
  | StatusMessage
  | ProgressMessage
  | ErrorMessage
  | MetaMessage
  | CompleteMessage;
