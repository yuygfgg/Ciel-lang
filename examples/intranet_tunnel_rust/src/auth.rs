use crate::error::{AuthError, ProtocolError, Result, TunnelError};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub const NONCE_LEN: usize = 32;
pub const TAG_LEN: usize = 32;
pub const AUTH_ALGORITHM: &str = "HMAC(SHA-256)";
const AUTH_CONTEXT: &[u8] = b"ciel-intranet-tunnel-auth-v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub version: u16,
    pub route: String,
    pub nonce: [u8; NONCE_LEN],
    pub tag: [u8; TAG_LEN],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloOk {
    pub selected_version: u16,
    pub server_nonce: [u8; NONCE_LEN],
}

pub fn random_nonce() -> Result<[u8; NONCE_LEN]> {
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|err| TunnelError::Crypto(err.to_string()))?;
    Ok(nonce)
}

pub fn mac_once(algorithm: &str, key: &[u8], data: &[u8], out: &mut [u8]) -> Result<usize> {
    if algorithm != AUTH_ALGORITHM {
        return Err(TunnelError::Crypto(format!(
            "unsupported MAC algorithm {algorithm}"
        )));
    }
    if out.len() < TAG_LEN {
        return Err(TunnelError::Crypto(
            "MAC output buffer is too small".to_owned(),
        ));
    }

    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|err| TunnelError::Crypto(format!("invalid HMAC key: {err}")))?;
    mac.update(data);
    let tag = mac.finalize().into_bytes();
    out[..TAG_LEN].copy_from_slice(&tag);
    Ok(TAG_LEN)
}

pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();
    for idx in 0..max_len {
        let lhs = left.get(idx).copied().unwrap_or(0);
        let rhs = right.get(idx).copied().unwrap_or(0);
        diff |= (lhs ^ rhs) as usize;
    }
    diff == 0
}

pub fn encode_hello(route: &str, psk: &[u8]) -> Result<Vec<u8>> {
    let nonce = random_nonce()?;
    encode_hello_with_nonce(route, psk, nonce)
}

pub fn encode_hello_with_nonce(route: &str, psk: &[u8], nonce: [u8; NONCE_LEN]) -> Result<Vec<u8>> {
    if route.len() > u16::MAX as usize {
        return Err(ProtocolError::MalformedPayload("route name too long").into());
    }

    let tag = compute_auth_tag(psk, crate::frame::VERSION, route, &nonce)?;
    let mut out = Vec::with_capacity(2 + 2 + route.len() + NONCE_LEN + TAG_LEN);
    out.extend_from_slice(&crate::frame::VERSION.to_be_bytes());
    out.extend_from_slice(&(route.len() as u16).to_be_bytes());
    out.extend_from_slice(route.as_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&tag);
    Ok(out)
}

pub fn decode_hello(input: &[u8]) -> Result<Hello> {
    if input.len() < 2 + 2 + NONCE_LEN + TAG_LEN {
        return Err(AuthError::MalformedHello.into());
    }

    let version = u16::from_be_bytes(input[0..2].try_into().expect("hello version"));
    let route_len = u16::from_be_bytes(input[2..4].try_into().expect("hello route len")) as usize;
    let expected_len = 2 + 2 + route_len + NONCE_LEN + TAG_LEN;
    if input.len() != expected_len {
        return Err(AuthError::MalformedHello.into());
    }

    let route_start = 4;
    let route_end = route_start + route_len;
    let route = std::str::from_utf8(&input[route_start..route_end])
        .map_err(|_| ProtocolError::Utf8Payload)?
        .to_owned();

    let mut nonce = [0_u8; NONCE_LEN];
    nonce.copy_from_slice(&input[route_end..route_end + NONCE_LEN]);
    let mut tag = [0_u8; TAG_LEN];
    tag.copy_from_slice(&input[route_end + NONCE_LEN..]);

    Ok(Hello {
        version,
        route,
        nonce,
        tag,
    })
}

pub fn verify_hello(hello: &Hello, expected_route: &str, psk: &[u8]) -> Result<()> {
    if hello.version != crate::frame::VERSION {
        return Err(AuthError::UnsupportedVersion(hello.version).into());
    }
    if hello.route != expected_route {
        return Err(AuthError::RouteMismatch {
            expected: expected_route.to_owned(),
            actual: hello.route.clone(),
        }
        .into());
    }

    let expected = compute_auth_tag(psk, hello.version, &hello.route, &hello.nonce)?;
    if !constant_time_eq(&expected, &hello.tag) {
        return Err(AuthError::TagMismatch.into());
    }

    Ok(())
}

pub fn encode_hello_ok() -> Result<Vec<u8>> {
    let server_nonce = random_nonce()?;
    encode_hello_ok_with_nonce(crate::frame::VERSION, server_nonce)
}

pub fn encode_hello_ok_with_nonce(
    selected_version: u16,
    server_nonce: [u8; NONCE_LEN],
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(2 + NONCE_LEN);
    out.extend_from_slice(&selected_version.to_be_bytes());
    out.extend_from_slice(&server_nonce);
    Ok(out)
}

pub fn decode_hello_ok(input: &[u8]) -> Result<HelloOk> {
    if input.len() != 2 + NONCE_LEN {
        return Err(ProtocolError::MalformedPayload("bad HelloOk length").into());
    }
    let selected_version = u16::from_be_bytes(input[0..2].try_into().expect("hello ok version"));
    if selected_version != crate::frame::VERSION {
        return Err(AuthError::UnsupportedVersion(selected_version).into());
    }
    let mut server_nonce = [0_u8; NONCE_LEN];
    server_nonce.copy_from_slice(&input[2..]);
    Ok(HelloOk {
        selected_version,
        server_nonce,
    })
}

fn compute_auth_tag(
    psk: &[u8],
    version: u16,
    route: &str,
    nonce: &[u8; NONCE_LEN],
) -> Result<[u8; TAG_LEN]> {
    let mut transcript = Vec::with_capacity(AUTH_CONTEXT.len() + 2 + 2 + route.len() + NONCE_LEN);
    transcript.extend_from_slice(AUTH_CONTEXT);
    transcript.extend_from_slice(&version.to_be_bytes());
    transcript.extend_from_slice(&(route.len() as u16).to_be_bytes());
    transcript.extend_from_slice(route.as_bytes());
    transcript.extend_from_slice(nonce);

    let mut out = [0_u8; TAG_LEN];
    mac_once(AUTH_ALGORITHM, psk, &transcript, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_sha256_matches_known_answer() {
        let key = [0x0b_u8; 20];
        let data = b"Hi There";
        let mut out = [0_u8; TAG_LEN];
        mac_once(AUTH_ALGORITHM, &key, data, &mut out).unwrap();
        assert_eq!(
            out,
            [
                0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53, 0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b,
                0xf1, 0x2b, 0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7, 0x26, 0xe9, 0x37, 0x6c,
                0x2e, 0x32, 0xcf, 0xf7,
            ]
        );
    }

    #[test]
    fn hello_round_trips_and_verifies() {
        let nonce = [7_u8; NONCE_LEN];
        let payload = encode_hello_with_nonce("dev", b"secret", nonce).unwrap();
        let hello = decode_hello(&payload).unwrap();
        assert_eq!(hello.version, crate::frame::VERSION);
        assert_eq!(hello.route, "dev");
        assert_eq!(hello.nonce, nonce);
        verify_hello(&hello, "dev", b"secret").unwrap();
    }

    #[test]
    fn wrong_psk_rejects() {
        let payload = encode_hello_with_nonce("dev", b"secret", [1_u8; NONCE_LEN]).unwrap();
        let hello = decode_hello(&payload).unwrap();
        assert!(matches!(
            verify_hello(&hello, "dev", b"wrong").unwrap_err(),
            TunnelError::Auth(AuthError::TagMismatch)
        ));
    }

    #[test]
    fn constant_time_eq_handles_length_mismatch() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abc\0"));
    }
}
