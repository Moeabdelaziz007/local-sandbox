use crate::error::{Error, Result};
use bytes::{BufMut, Bytes, BytesMut};

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Bytes),
    Null,
    Array(Vec<Frame>),
}

#[derive(Debug)]
pub struct Command {
    pub name: String,
    pub args: Vec<Bytes>,
}

impl Frame {
    /// Try to parse one complete RESP2 frame from 'buf'
    ///
    /// Returns:
    ///     `Ok(Some((frame,n))`: complete frame found, `n` bytes consumed
    ///     `Ok(None): need more data
    ///     `Err(_)`: malformed input
    pub fn parse(buf: &[u8]) -> Result<Option<(Frame, usize)>> {
        if buf.is_empty() {
            return Ok(None);
        }

        match buf[0] {
            b'+' => parse_simple(buf),
            b'-' => parse_error(buf),
            b':' => parse_integer(buf),
            b'$' => parse_bulk(buf),
            b'*' => parse_array(buf),
            _ => parse_inline(buf),
        }
    }

    /// Serialize this fram back to RESP2 wire bytes.
    pub fn serialize(&self) -> Bytes {
        let mut buf = BytesMut::new();
        write_frame(&mut buf, self);
        buf.freeze()
    }
}

fn write_frame(buf: &mut BytesMut, frame: &Frame) {
    match frame {
        Frame::Simple(s) => {
            buf.put_u8(b'+');
            buf.put_slice(s.as_bytes());
            buf.put_slice(b"\r\n");
        }
        Frame::Error(s) => {
            buf.put_u8(b'-');
            buf.put_slice(s.as_bytes());
            buf.put_slice(b"\r\n");
        }
        Frame::Integer(n) => {
            buf.put_slice(format!(":{}\r\n", n).as_bytes());
        }
        Frame::Bulk(data) => {
            buf.put_slice(format!("${}\r\n", data.len()).as_bytes());
            buf.put_slice(data);
            buf.put_slice(b"\r\n");
        }
        Frame::Null => {
            buf.put_slice(b"$-1\r\n");
        }
        Frame::Array(frames) => {
            buf.put_slice(format!("*{}\r\n", frames.len()).as_bytes());
            frames.iter().for_each(|f| write_frame(buf, f));
        }
    }
}

// Helpers

/// Find `\r\n`, return index of `\r`
fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|b| b == b"\r\n")
}

fn parse_line_i64(buf: &[u8]) -> Result<Option<(i64, usize)>> {
    match find_crlf(&buf[1..]) {
        None => Ok(None),
        Some(pos) => {
            let s = std::str::from_utf8(&buf[1..1 + pos])
                .map_err(|_| Error::Protocol("non-utf8 length".into()))?;
            let n = s
                .parse::<i64>()
                .map_err(|_| Error::Protocol(format!("invalid integer: {s}")))?;
            Ok(Some((n, 1 + pos + 2))) // prefix + content + \r\n
        }
    }
}
/// Read line after the prefix byte, parse it as i64
fn parse_simple(buf: &[u8]) -> Result<Option<(Frame, usize)>> {
    match find_crlf(&buf[1..]) {
        None => Ok(None),

        Some(pos) => {
            let s = std::str::from_utf8(&buf[1..1 + pos])
                .map_err(|_| Error::Protocol("non-utf8 simple string".into()))?
                .to_string();

            Ok(Some((Frame::Simple(s), 1 + pos + 2)))
        }
    }
}
fn parse_inline(buf: &[u8]) -> Result<Option<(Frame, usize)>> {
    match find_crlf(buf) {
        None => Ok(None),
        Some(pos) => {
            let line = std::str::from_utf8(&buf[..pos])
                .map_err(|_| Error::Protocol("non-utf8 inline command".into()))?;
            let parts: Vec<Bytes> = line
                .split_ascii_whitespace()
                .map(|s| Bytes::copy_from_slice(s.as_bytes()))
                .collect();
            if parts.is_empty() {
                return Err(Error::Protocol("empty inline command".into()));
            }
            let frames = parts.into_iter().map(Frame::Bulk).collect();
            Ok(Some((Frame::Array(frames), pos + 2)))
        }
    }
}

fn parse_array(buf: &[u8]) -> Result<Option<(Frame, usize)>> {
    let (count, mut offset) = match parse_line_i64(buf)? {
        None => return Ok(None),
        Some(v) => v,
    };

    if count == -1 {
        return Ok(Some((Frame::Null, offset)));
    }
    if count < 0 {
        return Err(Error::Protocol(format!("invalid array length: {count}")));
    }

    let mut frames = Vec::with_capacity(count as usize);
    for _ in 0..count {
        match Frame::parse(&buf[offset..])? {
            None => return Ok(None),
            Some((frame, consumed)) => {
                frames.push(frame);
                offset += consumed;
            }
        }
    }

    Ok(Some((Frame::Array(frames), offset)))
}

fn parse_bulk(buf: &[u8]) -> Result<Option<(Frame, usize)>> {
    let (len, header_len) = match parse_line_i64(buf)? {
        None => return Ok(None),
        Some(v) => v,
    };

    if len == -1 {
        return Ok(Some((Frame::Null, header_len)));
    }
    if len < 0 {
        return Err(Error::Protocol(format!("invalid bulk length: {len}")));
    }

    let len = len as usize;
    let total_needed = header_len + len + 2; // +2 for trailing \r\n
    if buf.len() < total_needed {
        return Ok(None);
    }

    let data = Bytes::copy_from_slice(&buf[header_len..header_len + len]);
    Ok(Some((Frame::Bulk(data), total_needed)))
}

fn parse_integer(buf: &[u8]) -> Result<Option<(Frame, usize)>> {
    match parse_line_i64(buf)? {
        None => Ok(None),
        Some((n, consumed)) => Ok(Some((Frame::Integer(n), consumed))),
    }
}

fn parse_error(buf: &[u8]) -> Result<Option<(Frame, usize)>> {
    match find_crlf(&buf[1..]) {
        None => Ok(None),

        Some(pos) => {
            let s = std::str::from_utf8(&buf[1..1 + pos])
                .map_err(|_| Error::Protocol("non-utf8 error".into()))?
                .to_string();

            Ok(Some((Frame::Simple(s), 1 + pos + 2)))
        }
    }
}

impl Command {
    pub fn from_frame(frame: Frame) -> Result<Command> {
        let frames = match frame {
            Frame::Array(v) => v,
            other => {
                return Err(Error::Protocol(format!(
                    "expected array frame, got: {:?}",
                    other
                )));
            }
        };

        let mut iter = frames.into_iter();

        let name = match iter.next() {
            Some(Frame::Bulk(b)) => String::from_utf8(b.to_vec())
                .map_err(|_| Error::Protocol("command name is not utf8".into()))?
                .to_uppercase(),
            _ => {
                return Err(Error::Protocol(
                    "first element must be a bulk string".into(),
                ));
            }
        };

        let args = iter
            .map(|f| match f {
                Frame::Bulk(b) => Ok(b),
                _ => Err(Error::Protocol("command args must be bulk strings".into())),
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Command { name, args })
    }

    pub fn arg(&self, index: usize, cmd_name: &'static str) -> Result<&Bytes> {
        self.args.get(index).ok_or(Error::WrongArity(cmd_name))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn parse_simple_string() {
        let (frame, n) = Frame::parse(b"+OK\r\n").unwrap().unwrap();
        assert_eq!(frame, Frame::Simple("OK".into()));
        assert_eq!(n, 5);
    }

    #[test]
    fn parse_error_frame() {
        let (frame, n) = Frame::parse(b"-ERR oops\r\n").unwrap().unwrap();
        assert_eq!(frame, Frame::Simple("ERR oops".into()));
        assert_eq!(n, 11);
    }

    #[test]
    fn parse_integer() {
        let (frame, n) = Frame::parse(b":42\r\n").unwrap().unwrap();
        assert_eq!(frame, Frame::Integer(42));
        assert_eq!(n, 5);
    }

    #[test]
    fn parse_negative_integer() {
        let (frame, _) = Frame::parse(b":-1\r\n").unwrap().unwrap();
        assert_eq!(frame, Frame::Integer(-1));
    }

    #[test]
    fn parse_bulk_string() {
        let (frame, n) = Frame::parse(b"$5\r\nhello\r\n").unwrap().unwrap();
        assert_eq!(frame, Frame::Bulk(Bytes::from_static(b"hello")));
        assert_eq!(n, 11);
    }

    #[test]
    fn parse_null_bulk() {
        let (frame, n) = Frame::parse(b"$-1\r\n").unwrap().unwrap();
        assert_eq!(frame, Frame::Null);
        assert_eq!(n, 5);
    }

    #[test]
    fn parse_empty_bulk() {
        let (frame, _) = Frame::parse(b"$0\r\n\r\n").unwrap().unwrap();
        assert_eq!(frame, Frame::Bulk(Bytes::new()));
    }

    #[test]
    fn parse_array() {
        let input = b"*2\r\n$3\r\nGET\r\n$3\r\nfoo\r\n";
        let (frame, n) = Frame::parse(input).unwrap().unwrap();
        assert_eq!(
            frame,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"GET")),
                Frame::Bulk(Bytes::from_static(b"foo")),
            ])
        );
        assert_eq!(n, input.len());
    }

    #[test]
    fn parse_null_array() {
        let (frame, _) = Frame::parse(b"*-1\r\n").unwrap().unwrap();
        assert_eq!(frame, Frame::Null);
    }

    #[test]
    fn parse_returns_none_when_incomplete() {
        assert!(Frame::parse(b"+OK").unwrap().is_none());
        assert!(Frame::parse(b"$5\r\nhel").unwrap().is_none());
        assert!(Frame::parse(b"*2\r\n$3\r\nGET\r\n").unwrap().is_none());
    }

    #[test]
    fn parse_inline_command() {
        let (frame, _) = Frame::parse(b"PING\r\n").unwrap().unwrap();
        assert_eq!(
            frame,
            Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"PING"))])
        );
    }

    #[test]
    fn parse_inline_with_args() {
        let (frame, _) = Frame::parse(b"SET foo bar\r\n").unwrap().unwrap();
        assert_eq!(
            frame,
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"SET")),
                Frame::Bulk(Bytes::from_static(b"foo")),
                Frame::Bulk(Bytes::from_static(b"bar")),
            ])
        );
    }

    #[test]
    fn serialize_simple() {
        assert_eq!(
            Frame::Simple("OK".into()).serialize(),
            Bytes::from_static(b"+OK\r\n")
        );
    }

    #[test]
    fn serialize_error() {
        assert_eq!(
            Frame::Error("ERR bad".into()).serialize(),
            Bytes::from_static(b"-ERR bad\r\n")
        );
    }

    #[test]
    fn serialize_integer() {
        assert_eq!(Frame::Integer(7).serialize(), Bytes::from_static(b":7\r\n"));
    }

    #[test]
    fn serialize_bulk() {
        assert_eq!(
            Frame::Bulk(Bytes::from_static(b"hi")).serialize(),
            Bytes::from_static(b"$2\r\nhi\r\n")
        );
    }

    #[test]
    fn serialize_null() {
        assert_eq!(Frame::Null.serialize(), Bytes::from_static(b"$-1\r\n"));
    }

    #[test]
    fn serialize_then_parse_roundtrip() {
        let original = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"SET")),
            Frame::Bulk(Bytes::from_static(b"key")),
            Frame::Bulk(Bytes::from_static(b"val")),
        ]);
        let bytes = original.serialize();
        let (parsed, n) = Frame::parse(&bytes).unwrap().unwrap();
        assert_eq!(parsed, original);
        assert_eq!(n, bytes.len());
    }

    #[test]
    fn command_from_frame_basic() {
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"get")),
            Frame::Bulk(Bytes::from_static(b"mykey")),
        ]);
        let cmd = Command::from_frame(frame).unwrap();
        assert_eq!(cmd.name, "GET");
        assert_eq!(cmd.args, vec![Bytes::from_static(b"mykey")]);
    }

    #[test]
    fn command_from_frame_normalises_name_to_uppercase() {
        let frame = Frame::Array(vec![Frame::Bulk(Bytes::from_static(b"ping"))]);
        let cmd = Command::from_frame(frame).unwrap();
        assert_eq!(cmd.name, "PING");
    }

    #[test]
    fn command_from_non_array_errors() {
        assert!(Command::from_frame(Frame::Simple("PING".into())).is_err());
    }
}
