//! Length-prefixed JSON framing used by both local protocols (app ⇄ settings/CLI and
//! app ⇄ helper service). Every frame is `u32 little-endian length` + UTF-8 JSON, with a
//! hard size limit enforced *before* allocating, so a misbehaving peer cannot make the
//! reader allocate unbounded memory.

use serde::{Serialize, de::DeserializeOwned};
use std::io::{self, Read, Write};

pub fn write_frame<T: Serialize, W: Write>(w: &mut W, msg: &T, max: usize) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    if body.len() > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {} bytes exceeds limit {max}", body.len())));
    }
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buf.extend_from_slice(&body);
    w.write_all(&buf)?;
    w.flush()
}

pub fn read_frame<T: DeserializeOwned, R: Read>(r: &mut R, max: usize) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {len} bytes exceeds limit {max}")));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &vec![1u32, 2, 3], 1024).unwrap();
        let v: Vec<u32> = read_frame(&mut Cursor::new(buf), 1024).unwrap();
        assert_eq!(v, vec![1, 2, 3]);
    }

    #[test]
    fn oversized_frames_rejected_without_allocating() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(u32::MAX).to_le_bytes());
        let r: io::Result<Vec<u8>> = read_frame(&mut Cursor::new(buf), 1024);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::InvalidData);
        let mut out = Vec::new();
        assert!(write_frame(&mut out, &"x".repeat(2000), 1024).is_err());
    }

    #[test]
    fn garbage_rejected() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(b"{{{");
        let r: io::Result<Vec<u8>> = read_frame(&mut Cursor::new(buf), 1024);
        assert!(r.is_err());
    }

    #[test]
    fn truncated_frame_is_error() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&10u32.to_le_bytes());
        buf.extend_from_slice(b"[1,");
        let r: io::Result<Vec<u8>> = read_frame(&mut Cursor::new(buf), 1024);
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }
}
