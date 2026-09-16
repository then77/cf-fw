use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::config::MAX_IPC_FRAME;
use crate::error::{FwError, Result};
use crate::ipc::protocol::Envelope;

const LENGTH_PREFIX_BYTES: usize = 4;

/// Encode a complete length-prefixed compact-JSON frame.
pub fn encode_frame<T: Serialize>(envelope: &Envelope<T>) -> Result<Vec<u8>> {
    envelope.validate_version()?;
    let payload = serde_json::to_vec(envelope)?;
    validate_payload_len(payload.len())?;

    let mut frame = Vec::with_capacity(LENGTH_PREFIX_BYTES + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decode one complete frame, rejecting trailing bytes and incompatible versions.
#[cfg(test)]
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<Envelope<T>> {
    if frame.len() < LENGTH_PREFIX_BYTES {
        return Err(FwError::Protocol("incomplete IPC frame header".into()));
    }

    let payload_len = u32::from_be_bytes(frame[..LENGTH_PREFIX_BYTES].try_into().unwrap()) as usize;
    validate_payload_len(payload_len)?;
    let expected_len = LENGTH_PREFIX_BYTES
        .checked_add(payload_len)
        .ok_or_else(|| FwError::Protocol("IPC frame length overflow".into()))?;
    if frame.len() != expected_len {
        return Err(FwError::Protocol(format!(
            "IPC frame length mismatch: header declares {payload_len} bytes, received {}",
            frame.len().saturating_sub(LENGTH_PREFIX_BYTES)
        )));
    }

    decode_payload(&frame[LENGTH_PREFIX_BYTES..])
}

/// Write one frame without allowing concurrent writers to interleave bytes.
pub async fn write_frame<W, T>(writer: &mut W, envelope: &Envelope<T>) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let frame = encode_frame(envelope)?;
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one frame from a stream. EOF in either header or payload is an I/O error.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<Envelope<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut header = [0_u8; LENGTH_PREFIX_BYTES];
    reader.read_exact(&mut header).await?;
    let payload_len = u32::from_be_bytes(header) as usize;
    validate_payload_len(payload_len)?;

    let mut payload = vec![0_u8; payload_len];
    reader.read_exact(&mut payload).await?;
    decode_payload(&payload)
}

fn decode_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<Envelope<T>> {
    let envelope: Envelope<T> = serde_json::from_slice(payload)?;
    envelope.validate_version()?;
    Ok(envelope)
}

fn validate_payload_len(payload_len: usize) -> Result<()> {
    if payload_len == 0 {
        return Err(FwError::Protocol("zero-length IPC frame".into()));
    }
    if payload_len > MAX_IPC_FRAME {
        return Err(FwError::Protocol(format!(
            "IPC frame is {payload_len} bytes; maximum is {MAX_IPC_FRAME}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncWriteExt, duplex};

    use super::*;
    use crate::config::PROTOCOL_VERSION;
    use crate::ipc::protocol::{ClientMessage, StopSelector};

    #[test]
    fn uses_four_byte_big_endian_length_prefix() {
        let envelope = Envelope::new(None, ClientMessage::List);
        let frame = encode_frame(&envelope).unwrap();
        let declared = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(declared, frame.len() - 4);
        assert_eq!(frame[4], b'{');
    }

    #[test]
    fn sync_round_trip_preserves_message() {
        let expected = Envelope::new(
            Some(9),
            ClientMessage::Stop {
                selector: StopSelector::Port(8080),
            },
        );
        let decoded: Envelope<ClientMessage> =
            decode_frame(&encode_frame(&expected).unwrap()).unwrap();
        assert_eq!(decoded, expected);
    }

    #[tokio::test]
    async fn async_round_trip_preserves_message() {
        let (mut client, mut server) = duplex(1024);
        let expected = Envelope::new(
            Some(3),
            ClientMessage::Register {
                port: 4321,
                slug: None,
                pid: 123,
            },
        );

        write_frame(&mut client, &expected).await.unwrap();
        let decoded: Envelope<ClientMessage> = read_frame(&mut server).await.unwrap();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn rejects_zero_length_frames() {
        let error = decode_frame::<ClientMessage>(&[0, 0, 0, 0]).unwrap_err();
        assert!(matches!(error, FwError::Protocol(_)));
    }

    #[test]
    fn rejects_oversized_frames_before_allocation() {
        let length = (MAX_IPC_FRAME as u32 + 1).to_be_bytes();
        let error = decode_frame::<ClientMessage>(&length).unwrap_err();
        assert!(matches!(error, FwError::Protocol(_)));
    }

    #[test]
    fn rejects_oversized_encoded_payloads() {
        let envelope = Envelope::new(
            None,
            ClientMessage::Register {
                port: 1,
                slug: Some("a".repeat(MAX_IPC_FRAME)),
                pid: 1,
            },
        );
        assert!(matches!(encode_frame(&envelope), Err(FwError::Protocol(_))));
    }

    #[test]
    fn rejects_declared_length_mismatch_and_trailing_data() {
        let mut frame = encode_frame(&Envelope::new(None, ClientMessage::Kill)).unwrap();
        frame.push(0);
        assert!(matches!(
            decode_frame::<ClientMessage>(&frame),
            Err(FwError::Protocol(_))
        ));
    }

    #[test]
    fn rejects_wrong_protocol_version() {
        let envelope = Envelope {
            version: PROTOCOL_VERSION + 1,
            request_id: None,
            message: ClientMessage::List,
        };
        assert!(matches!(encode_frame(&envelope), Err(FwError::Protocol(_))));
    }

    #[tokio::test]
    async fn stream_reader_rejects_oversized_header_immediately() {
        let (mut client, mut server) = duplex(16);
        client
            .write_all(&(MAX_IPC_FRAME as u32 + 1).to_be_bytes())
            .await
            .unwrap();
        let error = read_frame::<_, ClientMessage>(&mut server)
            .await
            .unwrap_err();
        assert!(matches!(error, FwError::Protocol(_)));
    }
}
