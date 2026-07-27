//! The wire framing language servers and debug adapters share.
//!
//! LSP and DAP agree on their envelope and on nothing after it: a JSON body
//! behind MIME-style headers, of which only `Content-Length` means anything.
//! This codec reads and writes that envelope; what the body says is the
//! caller's business, which is what lets both protocols use the same twenty
//! lines instead of carrying one each.

use std::io::{self, BufRead, Write};

use serde_json::Value;

/// Writes one message. The length is bytes, not characters, which is why it is
/// measured after serializing rather than promised before.
pub fn write_frame(writer: &mut impl Write, body: &Value) -> io::Result<()> {
    let body = body.to_string();
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body.as_bytes())?;
    writer.flush()
}

/// Reads one message, or `None` when there will never be another: the stream
/// ended between frames, or what arrived was not a frame. There is no way to
/// resynchronize a byte stream after a malformed header, so a broken frame and
/// a closed pipe are the same answer.
pub fn read_frame(reader: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        // Servers may send Content-Type too; anything but the length is noise.
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let Some(length) = length else {
        return Ok(None);
    };
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn a_frame_comes_back_as_it_went_in() {
        let mut wire = Vec::new();
        let sent = json!({"jsonrpc": "2.0", "method": "initialized", "params": {}});
        write_frame(&mut wire, &sent).unwrap();
        let got = read_frame(&mut Cursor::new(wire)).unwrap();
        assert_eq!(got, Some(sent));
    }

    #[test]
    fn two_frames_on_one_stream_read_in_order() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &json!({"id": 1})).unwrap();
        write_frame(&mut wire, &json!({"id": 2})).unwrap();
        let mut cursor = Cursor::new(wire);
        assert_eq!(read_frame(&mut cursor).unwrap(), Some(json!({"id": 1})));
        assert_eq!(read_frame(&mut cursor).unwrap(), Some(json!({"id": 2})));
        assert_eq!(read_frame(&mut cursor).unwrap(), None);
    }

    #[test]
    fn the_length_counts_bytes_not_characters() {
        // "żółć" is four characters and eight UTF-8 bytes; a codec counting
        // characters would leave half the body on the wire.
        let mut wire = Vec::new();
        let sent = json!({"text": "żółć"});
        write_frame(&mut wire, &sent).unwrap();
        assert_eq!(read_frame(&mut Cursor::new(wire)).unwrap(), Some(sent));
    }

    #[test]
    fn an_extra_header_is_ignored() {
        let body = r#"{"ok":true}"#;
        let wire = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let got = read_frame(&mut Cursor::new(wire.into_bytes())).unwrap();
        assert_eq!(got, Some(json!({"ok": true})));
    }

    #[test]
    fn an_empty_stream_is_the_end_not_an_error() {
        assert_eq!(read_frame(&mut Cursor::new(Vec::new())).unwrap(), None);
    }

    #[test]
    fn a_stream_without_a_length_is_over() {
        let wire = b"X-Nonsense: yes\r\n\r\n{}".to_vec();
        assert_eq!(read_frame(&mut Cursor::new(wire)).unwrap(), None);
    }
}
