use std::{
    fmt,
    io::{Cursor, Read},
};

pub(in crate::proxy) const PRIVATE_ENVELOPE_HEADER_BYTES: usize = 10;
pub(in crate::proxy) const PRIVATE_MESSAGE_MAX_BYTES: usize = 128 * 1024 * 1024;
pub(in crate::proxy) const PRIVATE_WEBSOCKET_SUBPROTOCOL: &str = "ai-cove-zstd.v1";

const PRIVATE_ENVELOPE_MAGIC: &[u8; 4] = b"AICZ";
const PRIVATE_ENVELOPE_VERSION: u8 = 0x01;
pub(in crate::proxy) const FLAG_ZSTD_COMPRESSED: u8 = 0b0000_0001;
const FLAG_ORIGINAL_BINARY: u8 = 0b0000_0010;
const ALLOWED_FLAGS: u8 = FLAG_ZSTD_COMPRESSED | FLAG_ORIGINAL_BINARY;
const ZSTD_LEVEL: i32 = 3;
const ZSTD_WINDOW_LOG_MAX: u32 = 27;

#[derive(Debug)]
pub(in crate::proxy) struct DecodedPrivateMessage {
    pub(in crate::proxy) payload: Vec<u8>,
    pub(in crate::proxy) original_binary: bool,
    #[cfg(test)]
    pub(in crate::proxy) compressed: bool,
}

#[derive(Debug)]
pub(in crate::proxy) struct PrivateProtocolError {
    pub(in crate::proxy) close_code: u16,
    message: &'static str,
}

impl PrivateProtocolError {
    pub(in crate::proxy) const fn protocol(message: &'static str) -> Self {
        Self {
            close_code: 1002,
            message,
        }
    }

    const fn invalid_data(message: &'static str) -> Self {
        Self {
            close_code: 1007,
            message,
        }
    }

    const fn too_large(message: &'static str) -> Self {
        Self {
            close_code: 1009,
            message,
        }
    }

    pub(in crate::proxy) const fn internal(message: &'static str) -> Self {
        Self {
            close_code: 1011,
            message,
        }
    }
}

impl fmt::Display for PrivateProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PrivateProtocolError {}

pub(in crate::proxy) fn encode_private_message(
    payload: &[u8],
    original_binary: bool,
) -> Result<Vec<u8>, PrivateProtocolError> {
    if payload.len() > PRIVATE_MESSAGE_MAX_BYTES {
        return Err(PrivateProtocolError::too_large(
            "original message exceeds 128 MiB",
        ));
    }
    if !original_binary && std::str::from_utf8(payload).is_err() {
        return Err(PrivateProtocolError::invalid_data(
            "text message is not UTF-8",
        ));
    }

    let compressed = if payload.len() >= super::super::MIN_COMPRESSION_INPUT_BYTES {
        Some(
            zstd::stream::encode_all(Cursor::new(payload), ZSTD_LEVEL)
                .map_err(|_| PrivateProtocolError::internal("zstd compression failed"))?,
        )
    } else {
        None
    };
    let use_compressed = compressed
        .as_ref()
        .is_some_and(|compressed| compressed.len() < payload.len());
    let wire_payload = if use_compressed {
        compressed.as_deref().unwrap_or(payload)
    } else {
        payload
    };
    let original_len = u32::try_from(payload.len())
        .map_err(|_| PrivateProtocolError::too_large("original message length is invalid"))?;
    let flags = u8::from(use_compressed) | (u8::from(original_binary) << 1);

    let mut envelope = Vec::with_capacity(PRIVATE_ENVELOPE_HEADER_BYTES + wire_payload.len());
    envelope.extend_from_slice(PRIVATE_ENVELOPE_MAGIC);
    envelope.push(PRIVATE_ENVELOPE_VERSION);
    envelope.push(flags);
    envelope.extend_from_slice(&original_len.to_be_bytes());
    envelope.extend_from_slice(wire_payload);
    Ok(envelope)
}

pub(in crate::proxy) fn decode_private_message(
    envelope: &[u8],
) -> Result<DecodedPrivateMessage, PrivateProtocolError> {
    let header = envelope
        .get(..PRIVATE_ENVELOPE_HEADER_BYTES)
        .ok_or_else(|| PrivateProtocolError::protocol("private envelope header is truncated"))?;
    let payload = envelope
        .get(PRIVATE_ENVELOPE_HEADER_BYTES..)
        .ok_or_else(|| PrivateProtocolError::protocol("private envelope payload is missing"))?;
    if payload.len() > PRIVATE_MESSAGE_MAX_BYTES {
        return Err(PrivateProtocolError::too_large(
            "wire payload exceeds 128 MiB",
        ));
    }
    if header.get(..4) != Some(PRIVATE_ENVELOPE_MAGIC.as_slice()) {
        return Err(PrivateProtocolError::protocol(
            "private envelope magic is invalid",
        ));
    }
    if header.get(4).copied() != Some(PRIVATE_ENVELOPE_VERSION) {
        return Err(PrivateProtocolError::protocol(
            "private envelope version is unsupported",
        ));
    }
    let flags = header
        .get(5)
        .copied()
        .ok_or_else(|| PrivateProtocolError::protocol("private envelope flags are missing"))?;
    if flags & !ALLOWED_FLAGS != 0 {
        return Err(PrivateProtocolError::protocol(
            "private envelope flags are invalid",
        ));
    }
    let length_bytes: [u8; 4] = header
        .get(6..10)
        .ok_or_else(|| PrivateProtocolError::protocol("private envelope length is missing"))?
        .try_into()
        .map_err(|_| PrivateProtocolError::protocol("private envelope length is invalid"))?;
    let original_len = usize::try_from(u32::from_be_bytes(length_bytes))
        .map_err(|_| PrivateProtocolError::too_large("original message length is invalid"))?;
    if original_len > PRIVATE_MESSAGE_MAX_BYTES {
        return Err(PrivateProtocolError::too_large(
            "original message exceeds 128 MiB",
        ));
    }

    let compressed = flags & FLAG_ZSTD_COMPRESSED != 0;
    let decoded = if compressed {
        decode_zstd(payload, original_len)?
    } else {
        if payload.len() != original_len {
            return Err(PrivateProtocolError::protocol(
                "raw payload length does not match the declared length",
            ));
        }
        payload.to_vec()
    };

    let original_binary = flags & FLAG_ORIGINAL_BINARY != 0;
    if !original_binary && std::str::from_utf8(&decoded).is_err() {
        return Err(PrivateProtocolError::invalid_data(
            "text message is not UTF-8",
        ));
    }
    Ok(DecodedPrivateMessage {
        payload: decoded,
        original_binary,
        #[cfg(test)]
        compressed,
    })
}

fn decode_zstd(payload: &[u8], original_len: usize) -> Result<Vec<u8>, PrivateProtocolError> {
    if payload.len() >= original_len {
        return Err(PrivateProtocolError::protocol(
            "compressed payload is not smaller than the original",
        ));
    }
    let mut decoder = zstd::stream::read::Decoder::new(Cursor::new(payload))
        .map_err(|_| PrivateProtocolError::invalid_data("zstd payload is damaged"))?;
    decoder
        .window_log_max(ZSTD_WINDOW_LOG_MAX)
        .map_err(|_| PrivateProtocolError::internal("zstd window limit failed"))?;
    let mut output = Vec::with_capacity(original_len.min(64 * 1024));
    let decode_limit = u64::try_from(original_len)
        .map_err(|_| PrivateProtocolError::too_large("original message length is invalid"))?
        .saturating_add(1);
    decoder
        .take(decode_limit)
        .read_to_end(&mut output)
        .map_err(|_| PrivateProtocolError::invalid_data("zstd payload is damaged"))?;
    if output.len() != original_len {
        return Err(PrivateProtocolError::invalid_data(
            "decoded length does not match the declared length",
        ));
    }
    Ok(output)
}
