use std::io;

use sift_extension_protocol::Message;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const DEFAULT_MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const HARD_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("RPC transport I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("RPC frame length {actual} exceeds limit {maximum}")]
    TooLarge { actual: usize, maximum: usize },
    #[error("RPC frame cannot be empty")]
    Empty,
    #[error("RPC frame is not valid UTF-8 JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

pub struct FrameReader<R> {
    inner: R,
    maximum: usize,
}

#[derive(Debug)]
pub struct ReceivedMessage {
    pub message: Message,
    pub encoded_bytes: usize,
}

impl<R> FrameReader<R>
where
    R: AsyncRead + Unpin,
{
    pub fn new(inner: R, maximum: usize) -> Result<Self, FrameError> {
        validate_maximum(maximum)?;
        Ok(Self { inner, maximum })
    }

    pub async fn read_message(&mut self) -> Result<Message, FrameError> {
        Ok(self.read_frame().await?.message)
    }

    pub async fn read_frame(&mut self) -> Result<ReceivedMessage, FrameError> {
        let length = self.inner.read_u32().await? as usize;
        if length == 0 {
            return Err(FrameError::Empty);
        }
        if length > self.maximum {
            return Err(FrameError::TooLarge {
                actual: length,
                maximum: self.maximum,
            });
        }
        let mut payload = vec![0_u8; length];
        self.inner.read_exact(&mut payload).await?;
        Ok(ReceivedMessage {
            message: serde_json::from_slice(&payload)?,
            encoded_bytes: length + 4,
        })
    }
}

pub struct FrameWriter<W> {
    inner: W,
    maximum: usize,
}

impl<W> FrameWriter<W>
where
    W: AsyncWrite + Unpin,
{
    pub fn new(inner: W, maximum: usize) -> Result<Self, FrameError> {
        validate_maximum(maximum)?;
        Ok(Self { inner, maximum })
    }

    pub async fn write_message(&mut self, message: &Message) -> Result<usize, FrameError> {
        let payload = serde_json::to_vec(message)?;
        if payload.is_empty() {
            return Err(FrameError::Empty);
        }
        if payload.len() > self.maximum {
            return Err(FrameError::TooLarge {
                actual: payload.len(),
                maximum: self.maximum,
            });
        }
        self.inner.write_u32(payload.len() as u32).await?;
        self.inner.write_all(&payload).await?;
        self.inner.flush().await?;
        Ok(payload.len() + 4)
    }

    pub async fn shutdown(&mut self) -> Result<(), FrameError> {
        self.inner.shutdown().await.map_err(Into::into)
    }
}

fn validate_maximum(maximum: usize) -> Result<(), FrameError> {
    if maximum == 0 {
        return Err(FrameError::Empty);
    }
    if maximum > HARD_MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            actual: maximum,
            maximum: HARD_MAX_FRAME_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use sift_extension_protocol::{Heartbeat, Message};
    use tokio::io::AsyncWriteExt;

    use super::*;

    #[tokio::test]
    async fn fragmented_frames_round_trip() {
        let (mut client, server) = tokio::io::duplex(128);
        let task = tokio::spawn(async move {
            let payload =
                serde_json::to_vec(&Message::Heartbeat(Heartbeat { sequence: 9 })).unwrap();
            let length = (payload.len() as u32).to_be_bytes();
            for byte in length.into_iter().chain(payload) {
                client.write_all(&[byte]).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        let mut reader = FrameReader::new(server, 128).unwrap();
        assert_eq!(
            reader.read_message().await.unwrap(),
            Message::Heartbeat(Heartbeat { sequence: 9 })
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn oversized_length_fails_before_allocating_payload() {
        let (mut client, server) = tokio::io::duplex(16);
        client.write_u32(1024).await.unwrap();
        let mut reader = FrameReader::new(server, 64).unwrap();
        assert!(matches!(
            reader.read_message().await,
            Err(FrameError::TooLarge {
                actual: 1024,
                maximum: 64
            })
        ));
    }

    #[tokio::test]
    async fn empty_and_malformed_frames_fail_closed() {
        let (mut empty_client, empty_server) = tokio::io::duplex(16);
        empty_client.write_u32(0).await.unwrap();
        let mut reader = FrameReader::new(empty_server, 64).unwrap();
        assert!(matches!(
            reader.read_message().await,
            Err(FrameError::Empty)
        ));

        let (mut invalid_client, invalid_server) = tokio::io::duplex(32);
        invalid_client.write_u32(8).await.unwrap();
        invalid_client.write_all(b"not-json").await.unwrap();
        let mut reader = FrameReader::new(invalid_server, 64).unwrap();
        assert!(matches!(
            reader.read_message().await,
            Err(FrameError::InvalidJson(_))
        ));
    }
}
