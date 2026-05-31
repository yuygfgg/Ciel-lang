use socket2::{Domain, Protocol, Socket, Type};
use std::env;
use std::future::Future;
use std::io::{self, ErrorKind};
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Builder;
use tokio::sync::Barrier;
use tokio::time;

const ECHO_BACKLOG: i32 = 16_384;
const ECHO_BUFFER_BYTES: usize = 128 * 1024;
const READ_BUFFER_BYTES: usize = 128 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let Some(command) = args.first().map(String::as_str) else {
        return Err(usage());
    };
    let worker_threads = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
        .max(1);
    let runtime = Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .map_err(|error| format!("failed to build tokio runtime: {error}"))?;

    runtime.block_on(async {
        match command {
            "echo-server" => run_echo_server(&args[1..]).await,
            "loadgen" => run_loadgen(&args[1..]).await,
            "-h" | "--help" | "help" => {
                println!("{}", usage());
                Ok(())
            }
            other => Err(format!("unknown command `{other}`\n{}", usage())),
        }
    })
}

fn usage() -> String {
    [
        "usage:",
        "  stress-tool echo-server --bind 127.0.0.1:0",
        "  stress-tool loadgen --addr 127.0.0.1:7001 --concurrency N --payload-bytes N --round-trips N --timeout-ms N",
    ]
    .join("\n")
}

async fn run_echo_server(args: &[String]) -> Result<(), String> {
    let bind = required(args, "--bind")?
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid --bind: {error}"))?;
    let listener =
        bind_listener(bind).map_err(|error| format!("failed to bind echo server: {error}"))?;
    eprintln!(
        "READY echo-server addr={}",
        listener
            .local_addr()
            .map_err(|error| format!("failed to read echo server address: {error}"))?
    );

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|error| format!("echo accept failed: {error}"))?;
        let _ = stream.set_nodelay(true);
        tokio::spawn(async move {
            if let Err(error) = echo_client(stream).await {
                eprintln!("echo client failed: {error}");
            }
        });
    }
}

fn bind_listener(addr: SocketAddr) -> io::Result<TcpListener> {
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    socket.set_reuse_address(true)?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(ECHO_BACKLOG)?;
    let listener: std::net::TcpListener = socket.into();
    TcpListener::from_std(listener)
}

async fn echo_client(mut stream: TcpStream) -> Result<(), String> {
    let mut buffer = vec![0_u8; ECHO_BUFFER_BYTES];
    loop {
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|error| format!("echo read failed: {error}"))?;
        if read == 0 {
            return Ok(());
        }
        stream
            .write_all(&buffer[..read])
            .await
            .map_err(|error| format!("echo write failed: {error}"))?;
    }
}

#[derive(Clone, Copy)]
struct LoadConfig {
    addr: SocketAddr,
    concurrency: usize,
    payload_bytes: usize,
    round_trips: usize,
    timeout: Duration,
}

#[derive(Default)]
struct WorkerStats {
    requests: u64,
    bytes: u64,
}

async fn run_loadgen(args: &[String]) -> Result<(), String> {
    let cfg = parse_load_config(args)?;
    let payload = Arc::<[u8]>::from(build_payload(cfg.payload_bytes));
    let barrier = Arc::new(Barrier::new(cfg.concurrency + 1));
    let mut joins = Vec::with_capacity(cfg.concurrency);

    for worker_id in 0..cfg.concurrency {
        let worker_cfg = cfg;
        let worker_payload = payload.clone();
        let worker_barrier = barrier.clone();
        joins.push(tokio::spawn(async move {
            run_worker(worker_id, worker_cfg, worker_payload, worker_barrier).await
        }));
    }

    barrier.wait().await;
    let started = Instant::now();

    let mut total = WorkerStats::default();
    let mut first_error = None::<String>;
    for join in joins {
        match join.await {
            Ok(Ok(stats)) => {
                total.requests = total.requests.saturating_add(stats.requests);
                total.bytes = total.bytes.saturating_add(stats.bytes);
            }
            Ok(Err(error)) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(format!("load worker panicked: {error}"));
                }
            }
        }
    }

    let elapsed = started.elapsed();
    let elapsed_secs = elapsed.as_secs_f64().max(0.000_001);
    let elapsed_ms = elapsed_secs * 1000.0;
    let mibps = (total.bytes as f64 / (1024.0 * 1024.0)) / elapsed_secs;
    let rps = total.requests as f64 / elapsed_secs;

    if let Some(error) = first_error {
        println!(
            "FAIL concurrency={} payload_bytes={} round_trips={} requests={} bytes={} elapsed_ms={:.3} reason={}",
            cfg.concurrency,
            cfg.payload_bytes,
            cfg.round_trips,
            total.requests,
            total.bytes,
            elapsed_ms,
            error
        );
        return Err(error);
    }

    println!(
        "OK concurrency={} payload_bytes={} round_trips={} requests={} bytes={} elapsed_ms={:.3} mibps={:.3} rps={:.3}",
        cfg.concurrency,
        cfg.payload_bytes,
        cfg.round_trips,
        total.requests,
        total.bytes,
        elapsed_ms,
        mibps,
        rps
    );
    Ok(())
}

fn parse_load_config(args: &[String]) -> Result<LoadConfig, String> {
    let addr = required(args, "--addr")?
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid --addr: {error}"))?;
    let concurrency = parse_usize(args, "--concurrency")?;
    let payload_bytes = parse_usize(args, "--payload-bytes")?;
    let round_trips = parse_usize(args, "--round-trips")?;
    let timeout_ms = parse_u64(args, "--timeout-ms")?;

    if concurrency == 0 {
        return Err("--concurrency must be greater than zero".to_string());
    }
    if payload_bytes == 0 {
        return Err("--payload-bytes must be greater than zero".to_string());
    }
    if round_trips == 0 {
        return Err("--round-trips must be greater than zero".to_string());
    }
    if timeout_ms == 0 {
        return Err("--timeout-ms must be greater than zero".to_string());
    }

    Ok(LoadConfig {
        addr,
        concurrency,
        payload_bytes,
        round_trips,
        timeout: Duration::from_millis(timeout_ms),
    })
}

fn build_payload(len: usize) -> Vec<u8> {
    let mut payload = vec![0_u8; len];
    for (idx, byte) in payload.iter_mut().enumerate() {
        *byte = ((idx.wrapping_mul(31) ^ idx.rotate_left(7)) & 0xff) as u8;
    }
    payload
}

async fn run_worker(
    worker_id: usize,
    cfg: LoadConfig,
    payload: Arc<[u8]>,
    barrier: Arc<Barrier>,
) -> Result<WorkerStats, String> {
    let mut read_buffer = vec![0_u8; cfg.payload_bytes.min(READ_BUFFER_BYTES)];

    barrier.wait().await;

    let mut stream = timeout_io(
        cfg.timeout,
        TcpStream::connect(cfg.addr),
        worker_id,
        "connect",
        0,
    )
    .await?;
    let _ = stream.set_nodelay(true);

    for request in 0..cfg.round_trips {
        timeout_io(
            cfg.timeout,
            stream.write_all(&payload),
            worker_id,
            "write",
            request,
        )
        .await?;
        read_echo(
            &mut stream,
            &payload,
            &mut read_buffer,
            cfg.timeout,
            worker_id,
            request,
        )
        .await?;
    }

    Ok(WorkerStats {
        requests: cfg.round_trips as u64,
        bytes: (cfg.payload_bytes as u64)
            .saturating_mul(2)
            .saturating_mul(cfg.round_trips as u64),
    })
}

async fn read_echo(
    stream: &mut TcpStream,
    payload: &[u8],
    buffer: &mut [u8],
    timeout: Duration,
    worker_id: usize,
    request: usize,
) -> Result<(), String> {
    let mut offset = 0;
    while offset < payload.len() {
        let want = buffer.len().min(payload.len() - offset);
        let read = timeout_io(
            timeout,
            stream.read(&mut buffer[..want]),
            worker_id,
            "read",
            request,
        )
        .await?;
        if read == 0 {
            return Err(format!("worker {worker_id} read {request} got EOF"));
        }
        if buffer[..read] != payload[offset..offset + read] {
            return Err(format!("worker {worker_id} read {request} echo mismatch"));
        }
        offset += read;
    }
    Ok(())
}

async fn timeout_io<T, F>(
    timeout: Duration,
    future: F,
    worker_id: usize,
    operation: &str,
    request: usize,
) -> Result<T, String>
where
    F: Future<Output = io::Result<T>>,
{
    match time::timeout(timeout, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(worker_io_error(worker_id, operation, request, error)),
        Err(_) => Err(format!(
            "worker {worker_id} {operation} {request} timed out waiting for echo"
        )),
    }
}

fn worker_io_error(worker_id: usize, operation: &str, request: usize, error: io::Error) -> String {
    match error.kind() {
        ErrorKind::TimedOut | ErrorKind::WouldBlock => {
            format!("worker {worker_id} {operation} {request} timed out waiting for echo: {error}")
        }
        _ => format!("worker {worker_id} {operation} {request} failed: {error}"),
    }
}

fn required<'a>(args: &'a [String], flag: &str) -> Result<&'a str, String> {
    let mut idx = 0;
    while idx < args.len() {
        if args[idx] == flag {
            return args
                .get(idx + 1)
                .map(String::as_str)
                .ok_or_else(|| format!("missing value for {flag}"));
        }
        idx += 1;
    }
    Err(format!("missing required flag {flag}"))
}

fn parse_usize(args: &[String], flag: &str) -> Result<usize, String> {
    required(args, flag)?
        .parse::<usize>()
        .map_err(|error| format!("invalid {flag}: {error}"))
}

fn parse_u64(args: &[String], flag: &str) -> Result<u64, String> {
    required(args, flag)?
        .parse::<u64>()
        .map_err(|error| format!("invalid {flag}: {error}"))
}
