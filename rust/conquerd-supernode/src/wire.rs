// ConquerD Supernode — wire.rs
// Datagram and stream wire formats for QUIC relay forwarding.

/// Max datagram payload.
#[allow(dead_code)]
pub const MAX_DATAGRAM_SIZE: usize = 1200;

/// Broadcast target index (all room members).
#[allow(dead_code)]
pub const BROADCAST_INDEX: u8 = 0xFF;

/// Stream IDs.
#[allow(dead_code)]
pub const SIGNALING_STREAM_ID: u64 = 0;
#[allow(dead_code)]
pub const RELAY_CMD_STREAM_ID: u64 = 1;

/// Encode a relay command as length-prefixed JSON for stream 1.
pub fn encode_relay_cmd(cmd: &serde_json::Value) -> Vec<u8> {
    let json = serde_json::to_vec(cmd).unwrap();
    let mut buf = Vec::with_capacity(4 + json.len());
    buf.extend_from_slice(&(json.len() as u32).to_be_bytes());
    buf.extend_from_slice(&json);
    buf
}

/// Decode one length-prefixed JSON message from a buffer.
/// Returns (parsed_value, bytes_consumed) or None if not enough data.
#[allow(dead_code)]
pub fn decode_relay_cmd(buf: &[u8]) -> Option<(serde_json::Value, usize)> {
    if buf.len() < 4 {
        return None;
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < 4 + len {
        return None;
    }
    let val = serde_json::from_slice(&buf[4..4 + len]).ok()?;
    Some((val, 4 + len))
}

/// Parse incoming datagram: returns (target_index, payload_slice).
pub fn parse_datagram(data: &[u8]) -> Option<(u8, &[u8])> {
    if data.is_empty() {
        return None;
    }
    Some((data[0], &data[1..]))
}

/// Build an outgoing datagram with a sender index prefix.
pub fn build_forwarded_datagram(sender_index: u8, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + payload.len());
    buf.push(sender_index);
    buf.extend_from_slice(payload);
    buf
}

/// Encode a signaling-stream message (length-prefixed JSON on stream 0).
#[allow(dead_code)]
pub fn encode_signaling(json_bytes: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + json_bytes.len());
    buf.extend_from_slice(&(json_bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(json_bytes);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_relay_cmd_roundtrip() {
        let cmd = serde_json::json!({"relay_cmd": "welcome", "index": 5});
        let encoded = encode_relay_cmd(&cmd);
        let (decoded, consumed) = decode_relay_cmd(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded["relay_cmd"], "welcome");
        assert_eq!(decoded["index"], 5);
    }

    #[test]
    fn test_datagram_parse() {
        let data = [0xFF, 1, 2, 3];
        let (idx, payload) = parse_datagram(&data).unwrap();
        assert_eq!(idx, BROADCAST_INDEX);
        assert_eq!(payload, &[1, 2, 3]);
    }

    #[test]
    fn test_forwarded_datagram() {
        let fwd = build_forwarded_datagram(5, &[10, 20, 30]);
        assert_eq!(fwd, &[5, 10, 20, 30]);
    }

    #[test]
    fn relay_is_transparent_to_first_party_channel_tags() {
        // The relay forwards the inner payload opaquely: the 1-byte channel
        // tag (e.g. CHAT_TAG / FILE_TAG from the shared `channel_frame`
        // contract) and the body must survive an index-prefix round-trip
        // unchanged. This is what lets chat/file ride a relayed session on
        // their own channel without the supernode parsing them.
        use conquerd_features::channel_frame::{decode_frame, encode_frame, CHAT_TAG, FILE_TAG};
        for (tag, body) in [
            (CHAT_TAG, &b"{\"type\":\"chat_message\"}"[..]),
            (FILE_TAG, &b"chunk"[..]),
        ] {
            let inner = encode_frame(tag, body);
            let fwd = build_forwarded_datagram(7, &inner);
            let (idx, payload) = parse_datagram(&fwd).unwrap();
            assert_eq!(idx, 7);
            assert_eq!(payload, inner.as_slice());
            // Receiver recovers the original tag + body intact.
            assert_eq!(decode_frame(payload), Some((tag, body)));
        }
    }
}
