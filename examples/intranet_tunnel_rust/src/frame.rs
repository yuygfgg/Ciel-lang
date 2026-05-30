use crate::error::{ProtocolError, Result, TunnelError};
use std::convert::TryFrom;
use std::io::{self, Read, Write};
use std::thread;
use std::time::Duration;

pub const MAGIC: u32 = 0x4349_544e;
pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 16;
pub const MAX_PAYLOAD_LEN: usize = 65_536;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum FrameKind {
    Hello = 1,
    HelloOk = 2,
    OpenStream = 3,
    OpenResult = 4,
    Data = 5,
    CloseWrite = 6,
    CloseStream = 7,
    Ping = 8,
    Pong = 9,
    Error = 10,
}

impl FrameKind {
    pub fn wire(self) -> u16 {
        self as u16
    }

    pub fn requires_zero_stream(self) -> bool {
        matches!(
            self,
            FrameKind::Hello
                | FrameKind::HelloOk
                | FrameKind::Ping
                | FrameKind::Pong
                | FrameKind::Error
        )
    }
}

impl TryFrom<u16> for FrameKind {
    type Error = ProtocolError;

    fn try_from(value: u16) -> std::result::Result<Self, ProtocolError> {
        match value {
            1 => Ok(FrameKind::Hello),
            2 => Ok(FrameKind::HelloOk),
            3 => Ok(FrameKind::OpenStream),
            4 => Ok(FrameKind::OpenResult),
            5 => Ok(FrameKind::Data),
            6 => Ok(FrameKind::CloseWrite),
            7 => Ok(FrameKind::CloseStream),
            8 => Ok(FrameKind::Ping),
            9 => Ok(FrameKind::Pong),
            10 => Ok(FrameKind::Error),
            other => Err(ProtocolError::UnknownFrameKind(other)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameHeader {
    pub kind: FrameKind,
    pub stream_id: u32,
    pub length: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: FrameKind,
    pub stream_id: u32,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(kind: FrameKind, stream_id: u32, payload: Vec<u8>) -> Result<Self> {
        validate_stream_id(kind, stream_id)?;
        if payload.len() > MAX_PAYLOAD_LEN {
            return Err(ProtocolError::OversizedPayload(payload.len()).into());
        }
        Ok(Self {
            kind,
            stream_id,
            payload,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenResultPayload {
    pub ok: bool,
    pub code: u16,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseReason {
    pub code: u16,
    pub message: String,
}

pub fn encode_header(header: &FrameHeader) -> Result<[u8; HEADER_LEN]> {
    validate_stream_id(header.kind, header.stream_id)?;
    if header.length as usize > MAX_PAYLOAD_LEN {
        return Err(ProtocolError::OversizedPayload(header.length as usize).into());
    }

    let mut out = [0_u8; HEADER_LEN];
    out[0..4].copy_from_slice(&MAGIC.to_be_bytes());
    out[4..6].copy_from_slice(&VERSION.to_be_bytes());
    out[6..8].copy_from_slice(&header.kind.wire().to_be_bytes());
    out[8..12].copy_from_slice(&header.stream_id.to_be_bytes());
    out[12..16].copy_from_slice(&header.length.to_be_bytes());
    Ok(out)
}

pub fn decode_header(input: &[u8]) -> Result<FrameHeader> {
    if input.len() < HEADER_LEN {
        return Err(ProtocolError::UnexpectedEof.into());
    }

    let magic = u32::from_be_bytes(input[0..4].try_into().expect("fixed header magic"));
    if magic != MAGIC {
        return Err(ProtocolError::BadMagic(magic).into());
    }

    let version = u16::from_be_bytes(input[4..6].try_into().expect("fixed header version"));
    if version != VERSION {
        return Err(ProtocolError::UnsupportedVersion(version).into());
    }

    let kind_raw = u16::from_be_bytes(input[6..8].try_into().expect("fixed header kind"));
    let kind = FrameKind::try_from(kind_raw)?;
    let stream_id = u32::from_be_bytes(input[8..12].try_into().expect("fixed header stream id"));
    let length = u32::from_be_bytes(input[12..16].try_into().expect("fixed header length"));
    if length as usize > MAX_PAYLOAD_LEN {
        return Err(ProtocolError::OversizedPayload(length as usize).into());
    }
    validate_stream_id(kind, stream_id)?;

    Ok(FrameHeader {
        kind,
        stream_id,
        length,
    })
}

pub fn read_frame<R: Read>(reader: &mut R) -> Result<Frame> {
    let mut header_bytes = [0_u8; HEADER_LEN];
    read_exact_protocol(reader, &mut header_bytes)?;
    let header = decode_header(&header_bytes)?;

    let mut payload = vec![0_u8; header.length as usize];
    read_exact_protocol(reader, &mut payload)?;

    Frame::new(header.kind, header.stream_id, payload)
}

pub fn write_frame<W: Write>(writer: &mut W, frame: &Frame) -> Result<()> {
    let header = FrameHeader {
        kind: frame.kind,
        stream_id: frame.stream_id,
        length: frame.payload.len() as u32,
    };
    let encoded = encode_header(&header)?;
    writer.write_all(&encoded)?;
    writer.write_all(&frame.payload)?;
    writer.flush()?;
    Ok(())
}

pub fn validate_stream_id(kind: FrameKind, stream_id: u32) -> Result<()> {
    let valid = if kind.requires_zero_stream() {
        stream_id == 0
    } else {
        stream_id != 0
    };

    if valid {
        Ok(())
    } else {
        Err(ProtocolError::InvalidStreamId {
            kind: kind.wire(),
            stream_id,
        }
        .into())
    }
}

pub fn encode_open_result_success() -> Vec<u8> {
    vec![0]
}

pub fn encode_open_result_error(code: u16, message: &str) -> Result<Vec<u8>> {
    let msg = message.as_bytes();
    if msg.len() > u16::MAX as usize {
        return Err(ProtocolError::MalformedPayload("open result message too long").into());
    }

    let mut out = Vec::with_capacity(5 + msg.len());
    out.push(1);
    out.extend_from_slice(&code.to_be_bytes());
    out.extend_from_slice(&(msg.len() as u16).to_be_bytes());
    out.extend_from_slice(msg);
    Ok(out)
}

pub fn decode_open_result(input: &[u8]) -> Result<OpenResultPayload> {
    if input.is_empty() {
        return Err(ProtocolError::MalformedPayload("empty open result").into());
    }

    match input[0] {
        0 if input.len() == 1 => Ok(OpenResultPayload {
            ok: true,
            code: 0,
            message: String::new(),
        }),
        0 => Err(ProtocolError::MalformedPayload("success open result has trailing bytes").into()),
        1 => {
            if input.len() < 5 {
                return Err(ProtocolError::MalformedPayload("short error open result").into());
            }
            let code = u16::from_be_bytes(input[1..3].try_into().expect("open result code"));
            let len =
                u16::from_be_bytes(input[3..5].try_into().expect("open result length")) as usize;
            if input.len() != 5 + len {
                return Err(ProtocolError::MalformedPayload("open result length mismatch").into());
            }
            let message =
                std::str::from_utf8(&input[5..]).map_err(|_| ProtocolError::Utf8Payload)?;
            Ok(OpenResultPayload {
                ok: false,
                code,
                message: message.to_owned(),
            })
        }
        _ => Err(ProtocolError::MalformedPayload("unknown open result status").into()),
    }
}

pub fn encode_close_reason(code: u16, message: &str) -> Result<Vec<u8>> {
    let msg = message.as_bytes();
    if msg.len() > u16::MAX as usize {
        return Err(ProtocolError::MalformedPayload("close message too long").into());
    }

    let mut out = Vec::with_capacity(4 + msg.len());
    out.extend_from_slice(&code.to_be_bytes());
    out.extend_from_slice(&(msg.len() as u16).to_be_bytes());
    out.extend_from_slice(msg);
    Ok(out)
}

pub fn decode_close_reason(input: &[u8]) -> Result<CloseReason> {
    if input.is_empty() {
        return Ok(CloseReason {
            code: 0,
            message: String::new(),
        });
    }

    if input.len() < 4 {
        return Err(ProtocolError::MalformedPayload("short close reason").into());
    }
    let code = u16::from_be_bytes(input[0..2].try_into().expect("close code"));
    let len = u16::from_be_bytes(input[2..4].try_into().expect("close length")) as usize;
    if input.len() != 4 + len {
        return Err(ProtocolError::MalformedPayload("close reason length mismatch").into());
    }
    let message = std::str::from_utf8(&input[4..]).map_err(|_| ProtocolError::Utf8Payload)?;
    Ok(CloseReason {
        code,
        message: message.to_owned(),
    })
}

pub fn encode_error_message(message: &str) -> Result<Vec<u8>> {
    if message.len() > MAX_PAYLOAD_LEN {
        return Err(ProtocolError::OversizedPayload(message.len()).into());
    }
    Ok(message.as_bytes().to_vec())
}

pub fn decode_error_message(input: &[u8]) -> Result<String> {
    let message = std::str::from_utf8(input).map_err(|_| ProtocolError::Utf8Payload)?;
    Ok(message.to_owned())
}

fn read_exact_protocol<R: Read>(reader: &mut R, out: &mut [u8]) -> Result<()> {
    let mut offset = 0;
    while offset < out.len() {
        match reader.read(&mut out[offset..]) {
            Ok(0) => return Err(ProtocolError::UnexpectedEof.into()),
            Ok(n) => offset += n,
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                thread::sleep(Duration::from_millis(1));
            }
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(ProtocolError::UnexpectedEof.into());
            }
            Err(err) => return Err(TunnelError::Io(err)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trips() {
        let header = FrameHeader {
            kind: FrameKind::Data,
            stream_id: 7,
            length: 1024,
        };
        let encoded = encode_header(&header).unwrap();
        assert_eq!(decode_header(&encoded).unwrap(), header);
    }

    #[test]
    fn rejects_short_header() {
        let err = decode_header(&[0; HEADER_LEN - 1]).unwrap_err();
        assert!(matches!(
            err,
            TunnelError::Protocol(ProtocolError::UnexpectedEof)
        ));
    }

    #[test]
    fn rejects_bad_magic() {
        let header = FrameHeader {
            kind: FrameKind::Ping,
            stream_id: 0,
            length: 0,
        };
        let mut encoded = encode_header(&header).unwrap();
        encoded[3] ^= 0x55;
        assert!(matches!(
            decode_header(&encoded).unwrap_err(),
            TunnelError::Protocol(ProtocolError::BadMagic(_))
        ));
    }

    #[test]
    fn rejects_oversized_payload_before_allocation() {
        let mut encoded = [0_u8; HEADER_LEN];
        encoded[0..4].copy_from_slice(&MAGIC.to_be_bytes());
        encoded[4..6].copy_from_slice(&VERSION.to_be_bytes());
        encoded[6..8].copy_from_slice(&(FrameKind::Data.wire()).to_be_bytes());
        encoded[8..12].copy_from_slice(&1_u32.to_be_bytes());
        encoded[12..16].copy_from_slice(&((MAX_PAYLOAD_LEN as u32) + 1).to_be_bytes());

        assert!(matches!(
            decode_header(&encoded).unwrap_err(),
            TunnelError::Protocol(ProtocolError::OversizedPayload(_))
        ));
    }

    #[test]
    fn validates_stream_id_rules() {
        assert!(Frame::new(FrameKind::Ping, 0, Vec::new()).is_ok());
        assert!(Frame::new(FrameKind::Data, 4, Vec::new()).is_ok());
        assert!(matches!(
            Frame::new(FrameKind::Ping, 4, Vec::new()).unwrap_err(),
            TunnelError::Protocol(ProtocolError::InvalidStreamId { .. })
        ));
        assert!(matches!(
            Frame::new(FrameKind::Data, 0, Vec::new()).unwrap_err(),
            TunnelError::Protocol(ProtocolError::InvalidStreamId { .. })
        ));
    }

    #[test]
    fn open_result_round_trips() {
        assert_eq!(
            decode_open_result(&encode_open_result_success()).unwrap(),
            OpenResultPayload {
                ok: true,
                code: 0,
                message: String::new()
            }
        );

        let encoded = encode_open_result_error(17, "target down").unwrap();
        assert_eq!(
            decode_open_result(&encoded).unwrap(),
            OpenResultPayload {
                ok: false,
                code: 17,
                message: "target down".to_owned()
            }
        );
    }
}
