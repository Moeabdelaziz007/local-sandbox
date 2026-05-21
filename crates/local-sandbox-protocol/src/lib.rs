//! Guest-agent wire protocol for local sandboxes.
//!
//! Messages are serialized as JSON frames prefixed by a four-byte big-endian
//! length. The same protocol is used by the host-side vfkit provider and the
//! guest-side sandbox agent.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum accepted frame payload size.
pub const MAX_FRAME_SIZE_BYTES: u32 = 16 * 1024 * 1024;

mod base64_serde {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let value = String::deserialize(deserializer)?;
        STANDARD.decode(value).map_err(serde::de::Error::custom)
    }
}

/// Request sent from the host provider to the guest agent.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRequest {
    /// Start a command in the guest.
    RunCommand {
        /// Executable name or path.
        cmd: String,
        /// Command arguments.
        args: Vec<String>,
        /// Optional working directory.
        #[serde(skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Optional environment overrides.
        #[serde(skip_serializing_if = "Option::is_none")]
        env: Option<BTreeMap<String, String>>,
        /// Whether the command should continue running after the response.
        /// Omitted means `false`.
        #[serde(skip_serializing_if = "Option::is_none")]
        detached: Option<bool>,
    },
    /// Fetch command status and output.
    GetCommand {
        /// Agent command identifier.
        cmd_id: String,
        /// Whether to wait for command completion before responding. Omitted
        /// means `false`.
        #[serde(skip_serializing_if = "Option::is_none")]
        wait: Option<bool>,
    },
    /// Kill a running command.
    KillCommand {
        /// Agent command identifier.
        cmd_id: String,
        /// Optional Unix signal number.
        #[serde(skip_serializing_if = "Option::is_none")]
        signal: Option<i32>,
    },
    /// Write files into the guest filesystem.
    WriteFiles {
        /// Files to write.
        files: Vec<AgentFileEntry>,
    },
    /// After `ConnectOk`, the connection becomes a raw TCP proxy.
    Connect {
        /// Guest loopback port to connect to.
        port: u16,
    },
    /// Probe guest-agent readiness.
    Ping,
    /// Flush guest-side filesystem dirty pages before host snapshotting.
    Sync,
}

/// File write entry transported over the guest-agent protocol.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentFileEntry {
    /// Absolute or sandbox-relative path understood by the guest agent.
    pub path: String,
    /// File contents. Encoded as base64 in JSON.
    #[serde(with = "base64_serde")]
    pub content: Vec<u8>,
    /// Optional Unix file mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

/// Response sent from the guest agent to the host provider.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentResponse {
    /// Detached command has started.
    CommandStarted {
        /// Agent command identifier.
        cmd_id: String,
    },
    /// Command status and captured output.
    CommandOutput {
        /// Agent command identifier.
        cmd_id: String,
        /// Captured stdout text.
        stdout: String,
        /// Captured stderr text.
        stderr: String,
        /// Process exit code when complete.
        exit_code: Option<i32>,
        /// Whether the command has finished.
        done: bool,
    },
    /// Files were written successfully.
    WriteFilesOk,
    /// Command kill request succeeded.
    KillOk,
    /// The connection can transition to raw TCP proxy mode.
    ConnectOk,
    /// Readiness probe succeeded.
    Pong,
    /// Guest-side filesystem flush completed.
    SyncOk,
    /// Guest-agent operation failed.
    Error {
        /// Stable error code for host-side classification.
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<AgentErrorCode>,
        /// Human-readable error message.
        message: String,
    },
}

/// Stable guest-agent error code.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentErrorCode {
    /// The requested command id is unknown to the guest agent.
    CommandNotFound,
}

/// Error returned while reading or writing protocol frames.
#[derive(Debug, Error)]
pub enum FrameError {
    /// Underlying IO error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization or deserialization error.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Incoming frame exceeded the accepted size.
    #[error("frame too large: {0} bytes")]
    TooLarge(u32),
}

/// Write one length-prefixed JSON frame.
pub async fn write_frame<W, T>(writer: &mut W, message: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(message)?;
    let len = u32::try_from(body.len()).map_err(|_| FrameError::TooLarge(u32::MAX))?;
    if len > MAX_FRAME_SIZE_BYTES {
        return Err(FrameError::TooLarge(len));
    }
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&body).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one length-prefixed JSON frame.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let mut len_buf = [0_u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_SIZE_BYTES {
        return Err(FrameError::TooLarge(len));
    }

    let mut body = vec![0_u8; len as usize];
    reader.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn run_command_frame_round_trips() {
        let request = AgentRequest::RunCommand {
            cmd: "echo".to_string(),
            args: vec!["hello".to_string()],
            cwd: Some("/tmp".to_string()),
            env: None,
            detached: None,
        };

        let (client, server) = tokio::io::duplex(4096);
        let (_client_reader, mut client_writer) = tokio::io::split(client);
        let (mut server_reader, _server_writer) = tokio::io::split(server);
        write_frame(&mut client_writer, &request).await.unwrap();
        drop(client_writer);

        let received: AgentRequest = read_frame(&mut server_reader).await.unwrap();
        assert_eq!(received, request);
    }

    #[tokio::test]
    async fn command_output_frame_round_trips() {
        let response = AgentResponse::CommandOutput {
            cmd_id: "cmd-1".to_string(),
            stdout: "out".to_string(),
            stderr: "err".to_string(),
            exit_code: Some(0),
            done: true,
        };

        let (client, server) = tokio::io::duplex(4096);
        let (_client_reader, mut client_writer) = tokio::io::split(client);
        let (mut server_reader, _server_writer) = tokio::io::split(server);
        write_frame(&mut client_writer, &response).await.unwrap();
        drop(client_writer);

        let received: AgentResponse = read_frame(&mut server_reader).await.unwrap();
        assert_eq!(received, response);
    }

    #[test]
    fn write_files_payload_uses_base64_json() {
        let request = AgentRequest::WriteFiles {
            files: vec![AgentFileEntry {
                path: "/tmp/hello.txt".to_string(),
                content: b"hello world".to_vec(),
                mode: Some(0o644),
            }],
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "type": "write_files",
                "files": [{
                    "path": "/tmp/hello.txt",
                    "content": "aGVsbG8gd29ybGQ=",
                    "mode": 420,
                }]
            })
        );
    }

    #[tokio::test]
    async fn ping_connect_kill_and_sync_messages_round_trip() {
        let messages = [
            AgentRequest::Ping,
            AgentRequest::Connect { port: 3000 },
            AgentRequest::KillCommand {
                cmd_id: "cmd-1".to_string(),
                signal: Some(15),
            },
            AgentRequest::Sync,
        ];

        for message in messages {
            let (client, server) = tokio::io::duplex(4096);
            let (_client_reader, mut client_writer) = tokio::io::split(client);
            let (mut server_reader, _server_writer) = tokio::io::split(server);
            write_frame(&mut client_writer, &message).await.unwrap();
            drop(client_writer);

            let received: AgentRequest = read_frame(&mut server_reader).await.unwrap();
            assert_eq!(received, message);
        }
    }

    #[tokio::test]
    async fn frame_too_large_is_rejected() {
        let (client, server) = tokio::io::duplex(4096);
        let (_client_reader, mut client_writer) = tokio::io::split(client);
        let (mut server_reader, _server_writer) = tokio::io::split(server);

        client_writer
            .write_all(&(MAX_FRAME_SIZE_BYTES + 1).to_be_bytes())
            .await
            .unwrap();
        drop(client_writer);

        let result: Result<AgentRequest, FrameError> = read_frame(&mut server_reader).await;
        assert!(
            matches!(result, Err(FrameError::TooLarge(size)) if size == MAX_FRAME_SIZE_BYTES + 1)
        );
    }

    #[tokio::test]
    async fn write_frame_rejects_payload_over_max_size() {
        let payload = "x".repeat(MAX_FRAME_SIZE_BYTES as usize);
        let mut writer = tokio::io::sink();

        let result = write_frame(&mut writer, &payload).await;

        assert!(matches!(result, Err(FrameError::TooLarge(size)) if size > MAX_FRAME_SIZE_BYTES));
    }

    #[test]
    fn optional_request_fields_omit_empty_defaults() {
        let run = AgentRequest::RunCommand {
            cmd: "true".to_string(),
            args: Vec::new(),
            cwd: None,
            env: None,
            detached: None,
        };
        let get = AgentRequest::GetCommand {
            cmd_id: "cmd-1".to_string(),
            wait: None,
        };

        assert_eq!(
            serde_json::to_value(run).unwrap(),
            json!({
                "type": "run_command",
                "cmd": "true",
                "args": [],
            })
        );
        assert_eq!(
            serde_json::to_value(get).unwrap(),
            json!({
                "type": "get_command",
                "cmd_id": "cmd-1",
            })
        );
    }
}
