use crate::agent::{AgentConfig, parse_connect_addr};
use crate::error::{Result, TunnelError};
use crate::server::{ServerConfig, parse_listen_addr};
use std::collections::HashMap;

pub fn parse_server_args<I>(args: I) -> Result<ServerConfig>
where
    I: IntoIterator<Item = String>,
{
    let values = parse_flag_values(args)?;
    reject_unknown(
        &values,
        &["--control", "--public", "--route", "--psk"],
        "tunnel-server",
    )?;

    Ok(ServerConfig {
        control_addr: parse_listen_addr(required(&values, "--control")?)?,
        public_addr: parse_listen_addr(required(&values, "--public")?)?,
        route: required(&values, "--route")?.to_owned(),
        psk: required(&values, "--psk")?.as_bytes().to_vec(),
    })
}

pub fn parse_agent_args<I>(args: I) -> Result<AgentConfig>
where
    I: IntoIterator<Item = String>,
{
    let values = parse_flag_values(args)?;
    reject_unknown(
        &values,
        &["--server", "--target", "--route", "--psk"],
        "tunnel-agent",
    )?;

    Ok(AgentConfig {
        server_addr: parse_connect_addr(required(&values, "--server")?)?,
        target_addr: parse_connect_addr(required(&values, "--target")?)?,
        route: required(&values, "--route")?.to_owned(),
        psk: required(&values, "--psk")?.as_bytes().to_vec(),
    })
}

pub fn server_usage() -> &'static str {
    "usage: tunnel-server --control 127.0.0.1:7000 --public 127.0.0.1:7001 --route dev --psk secret"
}

pub fn agent_usage() -> &'static str {
    "usage: tunnel-agent --server 127.0.0.1:7000 --target 127.0.0.1:9000 --route dev --psk secret"
}

fn parse_flag_values<I>(args: I) -> Result<HashMap<String, String>>
where
    I: IntoIterator<Item = String>,
{
    let mut values = HashMap::new();
    let mut iter = args.into_iter();
    while let Some(flag) = iter.next() {
        if !flag.starts_with("--") {
            return Err(TunnelError::Cli(format!("expected flag, got {flag}")));
        }
        let Some(value) = iter.next() else {
            return Err(TunnelError::Cli(format!("missing value for {flag}")));
        };
        if value.starts_with("--") {
            return Err(TunnelError::Cli(format!("missing value for {flag}")));
        }
        if values.insert(flag.clone(), value).is_some() {
            return Err(TunnelError::Cli(format!("duplicate flag {flag}")));
        }
    }
    Ok(values)
}

fn reject_unknown(values: &HashMap<String, String>, allowed: &[&str], binary: &str) -> Result<()> {
    for key in values.keys() {
        if !allowed.iter().any(|allowed| allowed == key) {
            return Err(TunnelError::Cli(format!("unknown flag {key} for {binary}")));
        }
    }
    Ok(())
}

fn required<'a>(values: &'a HashMap<String, String>, flag: &str) -> Result<&'a str> {
    values
        .get(flag)
        .map(|value| value.as_str())
        .ok_or_else(|| TunnelError::Cli(format!("missing required flag {flag}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_args() {
        let cfg = parse_server_args(
            [
                "--control",
                "127.0.0.1:7000",
                "--public",
                "127.0.0.1:7001",
                "--route",
                "dev",
                "--psk",
                "secret",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(cfg.control_addr.to_string(), "127.0.0.1:7000");
        assert_eq!(cfg.public_addr.to_string(), "127.0.0.1:7001");
        assert_eq!(cfg.route, "dev");
        assert_eq!(cfg.psk, b"secret");
    }

    #[test]
    fn parses_agent_args() {
        let cfg = parse_agent_args(
            [
                "--server",
                "127.0.0.1:7000",
                "--target",
                "127.0.0.1:9000",
                "--route",
                "dev",
                "--psk",
                "secret",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .unwrap();
        assert_eq!(cfg.server_addr.to_string(), "127.0.0.1:7000");
        assert_eq!(cfg.target_addr.to_string(), "127.0.0.1:9000");
        assert_eq!(cfg.route, "dev");
        assert_eq!(cfg.psk, b"secret");
    }

    #[test]
    fn rejects_missing_value() {
        let err = parse_server_args(["--control"].into_iter().map(str::to_owned)).unwrap_err();
        assert!(matches!(err, TunnelError::Cli(_)));
    }
}
