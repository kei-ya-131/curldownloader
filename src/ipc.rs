use crate::model::{ProxyProtocol, ProxySettings};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcRequest {
    Ping {
        request_id: String,
    },
    GetDefaults {
        request_id: String,
    },
    PickFolder {
        request_id: String,
    },
    Enqueue {
        request_id: String,
        url: String,
        filename: String,
        target_dir: String,
        proxy: WireProxy,
    },
}

impl IpcRequest {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Ping { request_id }
            | Self::GetDefaults { request_id }
            | Self::PickFolder { request_id }
            | Self::Enqueue { request_id, .. } => request_id,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IpcResponse {
    Pong {
        request_id: String,
        ok: bool,
    },
    Defaults {
        request_id: String,
        target_dir: String,
    },
    Folder {
        request_id: String,
        ok: bool,
        target_dir: Option<String>,
        error: Option<WireError>,
    },
    EnqueueResult {
        request_id: String,
        ok: bool,
        task_id: Option<u64>,
        error: Option<WireError>,
    },
}

impl IpcResponse {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Pong { request_id, .. }
            | Self::Defaults { request_id, .. }
            | Self::Folder { request_id, .. }
            | Self::EnqueueResult { request_id, .. } => request_id,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WireError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WireProxy {
    pub enabled: bool,
    pub protocol: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default)]
    pub password: String,
}

impl WireProxy {
    pub fn direct() -> Self {
        Self {
            enabled: false,
            protocol: "http".into(),
            host: String::new(),
            port: 8080,
            username: String::new(),
            password: String::new(),
        }
    }

    pub fn into_proxy_settings(self) -> Result<ProxySettings, String> {
        let protocol = match self.protocol.as_str() {
            "http" => ProxyProtocol::Http,
            "https" => ProxyProtocol::Https,
            "socks5" => ProxyProtocol::Socks5,
            "socks5h" => ProxyProtocol::Socks5h,
            _ => return Err("Proxy 類型無效".into()),
        };
        let mut proxy = ProxySettings {
            enabled: self.enabled,
            protocol,
            host: self.host,
            port: self.port,
            username: self.username,
            ..ProxySettings::default()
        };
        proxy.set_password(self.password)?;
        proxy.validate()?;
        Ok(proxy)
    }
}

pub fn read_frame<R: Read>(reader: &mut R, max_len: usize) -> io::Result<Vec<u8>> {
    let mut prefix = [0_u8; 4];
    reader.read_exact(&mut prefix)?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length == 0 || length > max_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC frame size is invalid",
        ));
    }
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    Ok(body)
}

pub fn write_frame<W: Write>(writer: &mut W, body: &[u8]) -> io::Result<()> {
    let length = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "IPC frame is too large"))?;
    if length == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "IPC frame cannot be empty",
        ));
    }
    writer.write_all(&length.to_le_bytes())?;
    writer.write_all(body)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_handles_partial_reader() {
        let body = br#"{"type":"ping","request_id":"1"}"#;
        let mut encoded = Vec::new();
        write_frame(&mut encoded, body).unwrap();
        let mut reader = ChunkedReader::new(encoded, 2);
        assert_eq!(read_frame(&mut reader, MAX_FRAME_BYTES).unwrap(), body);
    }

    #[test]
    fn frame_reader_rejects_oversized_body_before_allocating_it() {
        let encoded = (u32::try_from(MAX_FRAME_BYTES + 1).unwrap()).to_le_bytes();
        let mut reader = std::io::Cursor::new(encoded);
        assert_eq!(
            read_frame(&mut reader, MAX_FRAME_BYTES).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn wire_proxy_conversion_uses_zeroizing_password_storage() {
        let wire = WireProxy {
            enabled: true,
            protocol: "socks5h".into(),
            host: "127.0.0.1".into(),
            port: 1080,
            username: "alice".into(),
            password: "secret".into(),
        };
        let proxy = wire.into_proxy_settings().unwrap();
        assert!(!format!("{proxy:?}").contains("secret"));
        assert_eq!(
            proxy.password.as_deref().map(String::as_str),
            Some("secret")
        );
    }

    struct ChunkedReader {
        bytes: Vec<u8>,
        offset: usize,
        chunk_size: usize,
    }

    impl ChunkedReader {
        fn new(bytes: Vec<u8>, chunk_size: usize) -> Self {
            Self {
                bytes,
                offset: 0,
                chunk_size,
            }
        }
    }

    impl Read for ChunkedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.offset == self.bytes.len() {
                return Ok(0);
            }
            let size = self
                .chunk_size
                .min(buffer.len())
                .min(self.bytes.len() - self.offset);
            buffer[..size].copy_from_slice(&self.bytes[self.offset..self.offset + size]);
            self.offset += size;
            Ok(size)
        }
    }
}
