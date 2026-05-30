use intranet_tunnel_rust::cli::{parse_server_args, server_usage};
use intranet_tunnel_rust::server::run_server;
use std::process::ExitCode;

fn main() -> ExitCode {
    match parse_server_args(std::env::args().skip(1)).and_then(run_server) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            eprintln!("{}", server_usage());
            ExitCode::from(2)
        }
    }
}
