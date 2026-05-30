use crate::auth;
use crate::error::{AuthError, ProtocolError, Result, TunnelError};
use crate::frame::{self, Frame, FrameKind};
use crate::runtime::{ControlWriter, StreamEvent, StreamRegistry, spawn_local_reader};
use std::io::Write;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub server_addr: SocketAddr,
    pub target_addr: SocketAddr,
    pub route: String,
    pub psk: Vec<u8>,
}

pub struct AgentHandle {
    shutdown: Arc<AtomicBool>,
    control: Arc<AgentControlState>,
}

struct AgentControlState {
    writer: ControlWriter,
    streams: StreamRegistry,
    target_addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
}

impl AgentControlState {
    fn send(&self, frame: Frame) -> Result<()> {
        self.writer.send(frame)
    }
}

pub fn start_agent(config: AgentConfig) -> Result<AgentHandle> {
    let mut stream = TcpStream::connect(config.server_addr)?;
    stream.set_nodelay(true)?;
    handle_agent_handshake(&mut stream, &config.route, &config.psk)?;
    eprintln!("agent connected to server {}", config.server_addr);

    let reader = stream.try_clone()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let control = Arc::new(AgentControlState {
        writer: ControlWriter::new(stream),
        streams: StreamRegistry::new(),
        target_addr: config.target_addr,
        shutdown: shutdown.clone(),
    });
    spawn_agent_control_reader(reader, control.clone());

    Ok(AgentHandle { shutdown, control })
}

pub fn run_agent(config: AgentConfig) -> Result<()> {
    let mut backoff_ms = 100_u64;
    loop {
        match start_agent(config.clone()) {
            Ok(handle) => {
                backoff_ms = 100;
                while !handle.shutdown.load(Ordering::SeqCst) {
                    thread::park_timeout(Duration::from_millis(200));
                }
                eprintln!("agent control connection ended; reconnecting");
            }
            Err(TunnelError::Auth(err)) => return Err(TunnelError::Auth(err)),
            Err(err) => {
                eprintln!("agent connection failed: {err}; retrying in {backoff_ms} ms");
                thread::sleep(Duration::from_millis(backoff_ms));
                backoff_ms = (backoff_ms * 2).min(2_000);
            }
        }
    }
}

impl AgentHandle {
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.control.shutdown.store(true, Ordering::SeqCst);
        self.control.writer.shutdown();
    }
}

fn handle_agent_handshake(stream: &mut TcpStream, route: &str, psk: &[u8]) -> Result<()> {
    let hello = auth::encode_hello(route, psk)?;
    let hello_frame = Frame::new(FrameKind::Hello, 0, hello)?;
    frame::write_frame(stream, &hello_frame)?;

    let response = frame::read_frame(stream)?;
    match response.kind {
        FrameKind::HelloOk => {
            auth::decode_hello_ok(&response.payload)?;
            Ok(())
        }
        FrameKind::Error => {
            let message = frame::decode_error_message(&response.payload)?;
            Err(AuthError::ServerRejected(message).into())
        }
        _ => Err(ProtocolError::MalformedPayload("expected HelloOk or Error").into()),
    }
}

fn spawn_agent_control_reader(mut reader: TcpStream, control: Arc<AgentControlState>) {
    thread::spawn(move || {
        let _ = reader.set_read_timeout(Some(Duration::from_millis(200)));
        loop {
            if control.shutdown.load(Ordering::SeqCst) {
                break;
            }

            match frame::read_frame(&mut reader) {
                Ok(frame) => match frame.kind {
                    FrameKind::OpenStream => {
                        if frame.stream_id == 0 {
                            eprintln!("protocol error: zero stream id in OpenStream");
                            break;
                        }
                        if !frame.payload.is_empty() {
                            eprintln!("protocol error: OpenStream payload is not supported in MVP");
                            break;
                        }
                        let stream_id = frame.stream_id;
                        let (tx, rx) = mpsc::channel();
                        control.streams.insert(stream_id, tx.clone());
                        spawn_agent_stream_worker(stream_id, control.clone(), rx, tx);
                    }
                    FrameKind::Data | FrameKind::CloseWrite | FrameKind::CloseStream => {
                        let stream_id = frame.stream_id;
                        let target = {
                            control
                                .streams
                                .inner
                                .lock()
                                .expect("stream registry poisoned")
                                .get(&stream_id)
                                .cloned()
                        };
                        let Some(tx) = target else {
                            if matches!(frame.kind, FrameKind::Data) {
                                let payload = frame::encode_close_reason(20, "unknown stream")
                                    .and_then(|payload| {
                                        Frame::new(FrameKind::CloseStream, stream_id, payload)
                                    });
                                if let Ok(close) = payload {
                                    let _ = control.send(close);
                                }
                            }
                            continue;
                        };
                        if tx.send(StreamEvent::RemoteFrame(frame)).is_err() {
                            control.streams.remove(stream_id);
                        }
                    }
                    FrameKind::Ping => {
                        let pong = Frame::new(FrameKind::Pong, 0, Vec::new());
                        if let Ok(pong) = pong {
                            let _ = control.send(pong);
                        }
                    }
                    FrameKind::Pong => {}
                    FrameKind::Hello
                    | FrameKind::HelloOk
                    | FrameKind::OpenResult
                    | FrameKind::Error => {
                        eprintln!("protocol error: illegal frame kind on agent control connection");
                        break;
                    }
                },
                Err(TunnelError::Protocol(ProtocolError::UnexpectedEof)) => {
                    eprintln!("agent control connection closed");
                    break;
                }
                Err(TunnelError::Io(err))
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(err) => {
                    eprintln!("agent control read error: {err}");
                    break;
                }
            }
        }

        control.shutdown.store(true, Ordering::SeqCst);
        control.writer.shutdown();
    });
}

fn spawn_agent_stream_worker(
    stream_id: u32,
    control: Arc<AgentControlState>,
    rx: mpsc::Receiver<StreamEvent>,
    tx: mpsc::Sender<StreamEvent>,
) {
    thread::spawn(move || {
        let mut target =
            match TcpStream::connect_timeout(&control.target_addr, Duration::from_secs(2)) {
                Ok(target) => target,
                Err(err) => {
                    let message = format!("target dial failed: {err}");
                    let open = frame::encode_open_result_error(100, &message)
                        .and_then(|payload| Frame::new(FrameKind::OpenResult, stream_id, payload));
                    if let Ok(open) = open {
                        let _ = control.send(open);
                    }
                    let close = frame::encode_close_reason(100, &message)
                        .and_then(|payload| Frame::new(FrameKind::CloseStream, stream_id, payload));
                    if let Ok(close) = close {
                        let _ = control.send(close);
                    }
                    control.streams.remove(stream_id);
                    return;
                }
            };

        let _ = target.set_nodelay(true);
        let success = match Frame::new(
            FrameKind::OpenResult,
            stream_id,
            frame::encode_open_result_success(),
        ) {
            Ok(frame) => frame,
            Err(err) => {
                eprintln!("failed to build OpenResult for stream {stream_id}: {err}");
                control.streams.remove(stream_id);
                return;
            }
        };
        if let Err(err) = control.send(success) {
            eprintln!("failed to send OpenResult for stream {stream_id}: {err}");
            control.streams.remove(stream_id);
            return;
        }

        let reader = match target.try_clone() {
            Ok(reader) => reader,
            Err(err) => {
                eprintln!("failed to clone target stream {stream_id}: {err}");
                control.streams.remove(stream_id);
                return;
            }
        };
        let reader_join = spawn_local_reader(
            stream_id,
            reader,
            control.writer.clone(),
            tx,
            control.shutdown.clone(),
            "target",
        );

        let mut local_write_closed = false;
        let mut remote_write_closed = false;
        loop {
            if control.shutdown.load(Ordering::SeqCst) {
                break;
            }

            let event = match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(event) => event,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };

            match event {
                StreamEvent::RemoteFrame(frame) => match frame.kind {
                    FrameKind::Data => {
                        if let Err(err) = target.write_all(&frame.payload) {
                            let message = format!("target write failed: {err}");
                            let close =
                                frame::encode_close_reason(101, &message).and_then(|payload| {
                                    Frame::new(FrameKind::CloseStream, stream_id, payload)
                                });
                            if let Ok(close) = close {
                                let _ = control.send(close);
                            }
                            break;
                        }
                    }
                    FrameKind::CloseWrite => {
                        remote_write_closed = true;
                        let _ = target.shutdown(Shutdown::Write);
                        if local_write_closed {
                            break;
                        }
                    }
                    FrameKind::CloseStream => break,
                    other => {
                        eprintln!(
                            "protocol error: unexpected frame {other:?} for agent stream {stream_id}"
                        );
                        break;
                    }
                },
                StreamEvent::LocalReadClosed => {
                    local_write_closed = true;
                    let close = Frame::new(FrameKind::CloseWrite, stream_id, Vec::new());
                    if let Ok(close) = close {
                        let _ = control.send(close);
                    }
                    if remote_write_closed {
                        break;
                    }
                }
                StreamEvent::LocalReadFailed(reason) => {
                    let close = frame::encode_close_reason(102, &reason)
                        .and_then(|payload| Frame::new(FrameKind::CloseStream, stream_id, payload));
                    if let Ok(close) = close {
                        let _ = control.send(close);
                    }
                    break;
                }
            }
        }

        let _ = target.shutdown(Shutdown::Both);
        control.streams.remove(stream_id);
        let _ = reader_join.join();
        eprintln!("agent stream {stream_id} closed");
    });
}

pub fn parse_connect_addr(text: &str) -> Result<SocketAddr> {
    text.parse::<SocketAddr>()
        .map_err(|err| TunnelError::Cli(format!("invalid socket address {text}: {err}")))
}
