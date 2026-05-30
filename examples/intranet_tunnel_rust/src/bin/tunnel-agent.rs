use intranet_tunnel_rust::agent::run_agent;
use intranet_tunnel_rust::cli::{agent_usage, parse_agent_args};
use std::process::ExitCode;

fn main() -> ExitCode {
    match parse_agent_args(std::env::args().skip(1)).and_then(run_agent) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            eprintln!("{}", agent_usage());
            ExitCode::from(2)
        }
    }
}
