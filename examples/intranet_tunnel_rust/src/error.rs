use std::error::Error;
use std::fmt;
use std::io;

pub type Result<T> = std::result::Result<T, TunnelError>;

#[derive(Debug)]
pub enum TunnelError {
    Io(io::Error),
    Protocol(ProtocolError),
    Auth(AuthError),
    Crypto(String),
    Cli(String),
    ChannelClosed,
    AgentUnavailable,
    Shutdown,
}

impl fmt::Display for TunnelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TunnelError::Io(err) => write!(f, "io error: {err}"),
            TunnelError::Protocol(err) => write!(f, "protocol error: {err}"),
            TunnelError::Auth(err) => write!(f, "authentication error: {err}"),
            TunnelError::Crypto(err) => write!(f, "crypto error: {err}"),
            TunnelError::Cli(err) => write!(f, "cli error: {err}"),
            TunnelError::ChannelClosed => f.write_str("worker channel closed"),
            TunnelError::AgentUnavailable => f.write_str("no authenticated agent is available"),
            TunnelError::Shutdown => f.write_str("shutdown requested"),
        }
    }
}

impl Error for TunnelError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            TunnelError::Io(err) => Some(err),
            TunnelError::Protocol(err) => Some(err),
            TunnelError::Auth(err) => Some(err),
            TunnelError::Crypto(_)
            | TunnelError::Cli(_)
            | TunnelError::ChannelClosed
            | TunnelError::AgentUnavailable
            | TunnelError::Shutdown => None,
        }
    }
}

impl From<io::Error> for TunnelError {
    fn from(value: io::Error) -> Self {
        TunnelError::Io(value)
    }
}

impl From<ProtocolError> for TunnelError {
    fn from(value: ProtocolError) -> Self {
        TunnelError::Protocol(value)
    }
}

impl From<AuthError> for TunnelError {
    fn from(value: AuthError) -> Self {
        TunnelError::Auth(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    UnexpectedEof,
    BadMagic(u32),
    UnsupportedVersion(u16),
    UnknownFrameKind(u16),
    OversizedPayload(usize),
    InvalidStreamId { kind: u16, stream_id: u32 },
    InvalidTransition(&'static str),
    MalformedPayload(&'static str),
    Utf8Payload,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::UnexpectedEof => f.write_str("unexpected eof"),
            ProtocolError::BadMagic(magic) => write!(f, "bad magic 0x{magic:08x}"),
            ProtocolError::UnsupportedVersion(version) => {
                write!(f, "unsupported protocol version {version}")
            }
            ProtocolError::UnknownFrameKind(kind) => write!(f, "unknown frame kind {kind}"),
            ProtocolError::OversizedPayload(len) => {
                write!(f, "payload length {len} exceeds maximum")
            }
            ProtocolError::InvalidStreamId { kind, stream_id } => {
                write!(f, "invalid stream id {stream_id} for frame kind {kind}")
            }
            ProtocolError::InvalidTransition(reason) => {
                write!(f, "invalid stream transition: {reason}")
            }
            ProtocolError::MalformedPayload(reason) => write!(f, "malformed payload: {reason}"),
            ProtocolError::Utf8Payload => f.write_str("payload is not valid utf-8"),
        }
    }
}

impl Error for ProtocolError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    RouteMismatch { expected: String, actual: String },
    TagMismatch,
    UnsupportedVersion(u16),
    MalformedHello,
    ServerRejected(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::RouteMismatch { expected, actual } => {
                write!(f, "route mismatch: expected {expected}, got {actual}")
            }
            AuthError::TagMismatch => f.write_str("authentication tag mismatch"),
            AuthError::UnsupportedVersion(version) => {
                write!(f, "unsupported authentication version {version}")
            }
            AuthError::MalformedHello => f.write_str("malformed hello payload"),
            AuthError::ServerRejected(reason) => write!(f, "server rejected handshake: {reason}"),
        }
    }
}

impl Error for AuthError {}
