use crate::auth;
use crate::error::{ProtocolError, Result, TunnelError};
use crate::frame::{self, Frame, FrameKind};
use crate::runtime::{ControlWriter, StreamEvent, StreamRegistry, spawn_local_reader};
use std::io::Write;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub control_addr: SocketAddr,
    pub public_addr: SocketAddr,
    pub route: String,
    pub psk: Vec<u8>,
}

pub struct ServerHandle {
    pub control_addr: SocketAddr,
    pub public_addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    active_control: Arc<Mutex<Option<Arc<ServerControlState>>>>,
}

#[derive(Clone)]
struct ServerControlState {
    writer: ControlWriter,
    streams: StreamRegistry,
    shutdown: Arc<AtomicBool>,
}

impl ServerControlState {
    fn send(&self, frame: Frame) -> Result<()> {
        self.writer.send(frame)
    }
}

pub fn start_server(config: ServerConfig) -> Result<ServerHandle> {
    let control_listener = TcpListener::bind(config.control_addr)?;
    let public_listener = TcpListener::bind(config.public_addr)?;
    control_listener.set_nonblocking(true)?;
    public_listener.set_nonblocking(true)?;

    let control_addr = control_listener.local_addr()?;
    let public_addr = public_listener.local_addr()?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let active_control: Arc<Mutex<Option<Arc<ServerControlState>>>> = Arc::new(Mutex::new(None));
    let next_stream_id = Arc::new(AtomicU32::new(1));

    spawn_control_loop(
        control_listener,
        config.route.clone(),
        config.psk.clone(),
        shutdown.clone(),
        active_control.clone(),
    );
    spawn_public_loop(
        public_listener,
        shutdown.clone(),
        active_control.clone(),
        next_stream_id,
    );

    Ok(ServerHandle {
        control_addr,
        public_addr,
        shutdown,
        active_control,
    })
}

pub fn run_server(config: ServerConfig) -> Result<()> {
    let handle = start_server(config)?;
    eprintln!(
        "server started: control={}, public={}",
        handle.control_addr, handle.public_addr
    );
    while !handle.shutdown.load(Ordering::SeqCst) {
        thread::park_timeout(Duration::from_millis(200));
    }
    Ok(())
}

impl ServerHandle {
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(control) = self
            .active_control
            .lock()
            .expect("active control poisoned")
            .as_ref()
        {
            control.shutdown.store(true, Ordering::SeqCst);
            control.writer.shutdown();
        }
    }
}

fn spawn_control_loop(
    listener: TcpListener,
    route: String,
    psk: Vec<u8>,
    shutdown: Arc<AtomicBool>,
    active_control: Arc<Mutex<Option<Arc<ServerControlState>>>>,
) {
    thread::spawn(move || {
        loop {
            if shutdown.load(Ordering::SeqCst) {
                break;
            }

            match listener.accept() {
                Ok((mut stream, peer)) => {
                    let _ = stream.set_nodelay(true);
                    eprintln!("control connection accepted from {peer}");

                    let current = active_control
                        .lock()
                        .expect("active control poisoned")
                        .as_ref()
                        .cloned();
                    if current.is_some() {
                        eprintln!("rejecting additional control connection while agent is active");
                        let _ = stream.shutdown(Shutdown::Both);
                        continue;
                    }

                    match handle_server_handshake(&mut stream, &route, &psk) {
                        Ok(()) => {
                            let reader = match stream.try_clone() {
                                Ok(reader) => reader,
                                Err(err) => {
                                    eprintln!("failed to clone control stream: {err}");
                                    let _ = stream.shutdown(Shutdown::Both);
                                    continue;
                                }
                            };
                            let state = Arc::new(ServerControlState {
                                writer: ControlWriter::new(stream),
                                streams: StreamRegistry::new(),
                                shutdown: Arc::new(AtomicBool::new(false)),
                            });
                            {
                                let mut slot =
                                    active_control.lock().expect("active control poisoned");
                                *slot = Some(state.clone());
                            }
                            spawn_server_control_reader(
                                reader,
                                state.clone(),
                                active_control.clone(),
                                route.clone(),
                            );
                            eprintln!("authentication accepted for route {route}");
                        }
                        Err(err) => {
                            eprintln!("authentication rejected: {err}");
                            if let Ok(error_frame) = build_auth_failure_frame(&err.to_string()) {
                                let _ = frame::write_frame(&mut stream, &error_frame);
                            }
                            let _ = stream.shutdown(Shutdown::Both);
                        }
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(err) => {
                    eprintln!("control listener error: {err}");
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
    });
}

fn spawn_public_loop(
    listener: TcpListener,
    shutdown: Arc<AtomicBool>,
    active_control: Arc<Mutex<Option<Arc<ServerControlState>>>>,
    next_stream_id: Arc<AtomicU32>,
) {
    thread::spawn(move || {
        loop {
            if shutdown.load(Ordering::SeqCst) {
                break;
            }

            match listener.accept() {
                Ok((client, peer)) => {
                    let _ = client.set_nodelay(true);
                    eprintln!("public client accepted from {peer}");
                    let control = active_control
                        .lock()
                        .expect("active control poisoned")
                        .as_ref()
                        .cloned();
                    let Some(control) = control else {
                        eprintln!("no authenticated agent available; closing public client");
                        let _ = client.shutdown(Shutdown::Both);
                        continue;
                    };

                    let stream_id = next_stream_id.fetch_add(1, Ordering::SeqCst);
                    if stream_id == 0 {
                        continue;
                    }
                    let (tx, rx) = mpsc::channel();
                    control.streams.insert(stream_id, tx.clone());
                    let open = match Frame::new(FrameKind::OpenStream, stream_id, Vec::new()) {
                        Ok(frame) => frame,
                        Err(err) => {
                            eprintln!("failed to create open frame: {err}");
                            let _ = client.shutdown(Shutdown::Both);
                            control.streams.remove(stream_id);
                            continue;
                        }
                    };
                    if let Err(err) = control.send(open) {
                        eprintln!("failed to send open stream: {err}");
                        let _ = client.shutdown(Shutdown::Both);
                        control.streams.remove(stream_id);
                        continue;
                    }
                    spawn_server_stream_worker(
                        stream_id,
                        client,
                        control.clone(),
                        rx,
                        tx,
                        shutdown.clone(),
                    );
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(err) => {
                    eprintln!("public listener error: {err}");
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
    });
}

fn spawn_server_control_reader(
    mut reader: TcpStream,
    control: Arc<ServerControlState>,
    active_control: Arc<Mutex<Option<Arc<ServerControlState>>>>,
    route: String,
) {
    thread::spawn(move || {
        let _ = reader.set_read_timeout(Some(Duration::from_millis(200)));
        loop {
            if control.shutdown.load(Ordering::SeqCst) {
                break;
            }
            match frame::read_frame(&mut reader) {
                Ok(frame) => match frame.kind {
                    FrameKind::OpenResult
                    | FrameKind::Data
                    | FrameKind::CloseWrite
                    | FrameKind::CloseStream => {
                        if frame.stream_id == 0 {
                            eprintln!("protocol error: zero stream id on stream frame");
                            break;
                        }
                        let stream_id = frame.stream_id;
                        let kind = frame.kind;
                        let target = {
                            let streams = control.streams.clone();
                            streams
                                .inner
                                .lock()
                                .expect("stream registry poisoned")
                                .get(&stream_id)
                                .cloned()
                        };
                        let Some(tx) = target else {
                            if matches!(kind, FrameKind::Data | FrameKind::OpenResult) {
                                eprintln!("protocol error: unknown stream {stream_id}");
                                break;
                            }
                            continue;
                        };
                        if tx.send(StreamEvent::RemoteFrame(frame)).is_err() {
                            control.streams.remove(stream_id);
                        }
                    }
                    FrameKind::Ping => {
                        let pong = match Frame::new(FrameKind::Pong, 0, Vec::new()) {
                            Ok(frame) => frame,
                            Err(err) => {
                                eprintln!("failed to build pong: {err}");
                                break;
                            }
                        };
                        if let Err(err) = control.send(pong) {
                            eprintln!("failed to send pong: {err}");
                            break;
                        }
                    }
                    FrameKind::Pong => {}
                    FrameKind::Hello | FrameKind::HelloOk | FrameKind::Error => {
                        eprintln!("protocol error: illegal control frame kind from agent");
                        break;
                    }
                    FrameKind::OpenStream => {
                        eprintln!("protocol error: agent sent open stream frame");
                        break;
                    }
                },
                Err(TunnelError::Protocol(ProtocolError::UnexpectedEof)) => {
                    eprintln!("control connection closed");
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
                    eprintln!("control read error: {err}");
                    break;
                }
            }
        }

        control.shutdown.store(true, Ordering::SeqCst);
        control.writer.shutdown();
        {
            let mut slot = active_control.lock().expect("active control poisoned");
            if slot
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &control))
            {
                *slot = None;
            }
        }
        eprintln!("control connection closed for route {route}");
    });
}

fn spawn_server_stream_worker(
    stream_id: u32,
    mut client: TcpStream,
    control: Arc<ServerControlState>,
    rx: mpsc::Receiver<StreamEvent>,
    tx: mpsc::Sender<StreamEvent>,
    shutdown: Arc<AtomicBool>,
) {
    thread::spawn(move || {
        let _ = client.set_read_timeout(Some(Duration::from_millis(200)));
        let _ = client.set_write_timeout(Some(Duration::from_millis(200)));
        let mut remote_write_closed = false;
        let mut local_write_closed = false;
        let mut reader_started = false;
        let mut reader_join = None;

        loop {
            if shutdown.load(Ordering::SeqCst) {
                break;
            }

            let event = match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(event) => event,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };

            match event {
                StreamEvent::RemoteFrame(frame) => match frame.kind {
                    FrameKind::OpenResult => {
                        if reader_started {
                            eprintln!(
                                "protocol error: duplicate open result for stream {stream_id}"
                            );
                            break;
                        }
                        let result = match frame::decode_open_result(&frame.payload) {
                            Ok(result) => result,
                            Err(err) => {
                                eprintln!("malformed open result for stream {stream_id}: {err}");
                                break;
                            }
                        };
                        if !result.ok {
                            eprintln!(
                                "target dial failed for stream {stream_id}: {} ({})",
                                result.message, result.code
                            );
                            let _ = client.shutdown(Shutdown::Both);
                            break;
                        }

                        let reader = match client.try_clone() {
                            Ok(reader) => reader,
                            Err(err) => {
                                eprintln!(
                                    "failed to clone client socket for stream {stream_id}: {err}"
                                );
                                break;
                            }
                        };
                        reader_started = true;
                        reader_join = Some(spawn_local_reader(
                            stream_id,
                            reader,
                            control.writer.clone(),
                            tx.clone(),
                            shutdown.clone(),
                            "client",
                        ));
                    }
                    FrameKind::Data => {
                        if frame.payload.is_empty() {
                            continue;
                        }
                        if let Err(err) = client.write_all(&frame.payload) {
                            eprintln!(
                                "failed to write to public client for stream {stream_id}: {err}"
                            );
                            let close = match frame::encode_close_reason(2, &err.to_string())
                                .and_then(|payload| {
                                    Frame::new(FrameKind::CloseStream, stream_id, payload)
                                }) {
                                Ok(frame) => frame,
                                Err(build_err) => {
                                    eprintln!("failed to build close frame: {build_err}");
                                    break;
                                }
                            };
                            let _ = control.send(close);
                            break;
                        }
                    }
                    FrameKind::CloseWrite => {
                        remote_write_closed = true;
                        let _ = client.shutdown(Shutdown::Write);
                        if local_write_closed {
                            break;
                        }
                    }
                    FrameKind::CloseStream => {
                        break;
                    }
                    FrameKind::Ping => {
                        if let Ok(pong) = Frame::new(FrameKind::Pong, 0, Vec::new()) {
                            let _ = control.send(pong);
                        }
                    }
                    other => {
                        eprintln!(
                            "protocol error: unsupported control frame {other:?} on stream {stream_id}"
                        );
                        break;
                    }
                },
                StreamEvent::LocalReadClosed => {
                    local_write_closed = true;
                    let close = match Frame::new(FrameKind::CloseWrite, stream_id, Vec::new()) {
                        Ok(frame) => frame,
                        Err(err) => {
                            eprintln!("failed to build close-write frame: {err}");
                            break;
                        }
                    };
                    if let Err(err) = control.send(close) {
                        eprintln!("failed to forward close-write for stream {stream_id}: {err}");
                        break;
                    }
                    if remote_write_closed {
                        break;
                    }
                }
                StreamEvent::LocalReadFailed(reason) => {
                    eprintln!("local client error on stream {stream_id}: {reason}");
                    let payload = match frame::encode_close_reason(3, &reason)
                        .and_then(|payload| Frame::new(FrameKind::CloseStream, stream_id, payload))
                    {
                        Ok(frame) => frame,
                        Err(err) => {
                            eprintln!("failed to build local failure close frame: {err}");
                            break;
                        }
                    };
                    let _ = control.send(payload);
                    break;
                }
            }
        }

        let _ = client.shutdown(Shutdown::Both);
        control.streams.remove(stream_id);
        if let Some(join) = reader_join {
            let _ = join.join();
        }
        eprintln!("stream {stream_id} closed");
    });
}

fn handle_server_handshake(stream: &mut TcpStream, expected_route: &str, psk: &[u8]) -> Result<()> {
    let hello_frame = frame::read_frame(stream)?;
    if hello_frame.kind != FrameKind::Hello || hello_frame.stream_id != 0 {
        return Err(ProtocolError::MalformedPayload("expected Hello frame").into());
    }
    let hello = auth::decode_hello(&hello_frame.payload)?;
    auth::verify_hello(&hello, expected_route, psk)?;

    let server_nonce = auth::random_nonce()?;
    let hello_ok = auth::encode_hello_ok_with_nonce(frame::VERSION, server_nonce)?;
    let response = Frame::new(FrameKind::HelloOk, 0, hello_ok)?;
    frame::write_frame(stream, &response)?;
    Ok(())
}

pub fn build_auth_failure_frame(reason: &str) -> Result<Frame> {
    let payload = frame::encode_error_message(reason)?;
    Frame::new(FrameKind::Error, 0, payload)
}

pub fn parse_listen_addr(text: &str) -> Result<SocketAddr> {
    text.parse::<SocketAddr>()
        .map_err(|err| TunnelError::Cli(format!("invalid socket address {text}: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentConfig, start_agent};
    use crate::error::AuthError;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::thread;
    use std::time::Duration;

    fn start_echo_listener() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        thread::spawn(move || {
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        thread::spawn(move || {
                            let mut buf = [0_u8; 4096];
                            loop {
                                match stream.read(&mut buf) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        if stream.write_all(&buf[..n]).is_err() {
                                            break;
                                        }
                                    }
                                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                                        thread::sleep(Duration::from_millis(10));
                                    }
                                    Err(_) => break,
                                }
                            }
                        });
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        addr
    }

    fn start_server_and_agent(
        echo_addr: SocketAddr,
    ) -> (ServerHandle, crate::agent::AgentHandle, SocketAddr) {
        let server = start_server(ServerConfig {
            control_addr: "127.0.0.1:0".parse().unwrap(),
            public_addr: "127.0.0.1:0".parse().unwrap(),
            route: "dev".to_owned(),
            psk: b"secret".to_vec(),
        })
        .unwrap();
        let agent = start_agent(AgentConfig {
            server_addr: server.control_addr,
            target_addr: echo_addr,
            route: "dev".to_owned(),
            psk: b"secret".to_vec(),
        })
        .unwrap();
        let public_addr = server.public_addr;
        (server, agent, public_addr)
    }

    fn round_trip(addr: SocketAddr, payload: &[u8]) -> Vec<u8> {
        let mut client = TcpStream::connect(addr).unwrap();
        client.write_all(payload).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut out = Vec::new();
        client.read_to_end(&mut out).unwrap();
        out
    }

    #[test]
    fn sequential_clients_echo_through_tunnel() {
        let echo_addr = start_echo_listener();
        let (server, agent, public_addr) = start_server_and_agent(echo_addr);
        thread::sleep(Duration::from_millis(250));
        assert_eq!(round_trip(public_addr, b"first"), b"first");
        assert_eq!(round_trip(public_addr, b"second"), b"second");
        server.shutdown();
        agent.shutdown();
    }

    #[test]
    fn concurrent_clients_echo_independently() {
        let echo_addr = start_echo_listener();
        let (server, agent, public_addr) = start_server_and_agent(echo_addr);
        thread::sleep(Duration::from_millis(250));
        let a = public_addr;
        let b = public_addr;
        let left = thread::spawn(move || round_trip(a, b"alpha"));
        let right = thread::spawn(move || round_trip(b, b"bravo"));
        assert_eq!(left.join().unwrap(), b"alpha");
        assert_eq!(right.join().unwrap(), b"bravo");
        server.shutdown();
        agent.shutdown();
    }

    #[test]
    fn large_payload_is_split_and_reassembled() {
        let echo_addr = start_echo_listener();
        let (server, agent, public_addr) = start_server_and_agent(echo_addr);
        thread::sleep(Duration::from_millis(250));
        let payload: Vec<u8> = (0..200_000).map(|idx| (idx % 251) as u8).collect();
        assert_eq!(round_trip(public_addr, &payload), payload);
        server.shutdown();
        agent.shutdown();
    }

    #[test]
    fn early_public_client_disconnect_does_not_break_control_connection() {
        let echo_addr = start_echo_listener();
        let (server, agent, public_addr) = start_server_and_agent(echo_addr);
        thread::sleep(Duration::from_millis(250));

        {
            let mut client = TcpStream::connect(public_addr).unwrap();
            client.write_all(b"drop-before-reading").unwrap();
        }

        thread::sleep(Duration::from_millis(300));
        assert_eq!(
            round_trip(public_addr, b"after-disconnect"),
            b"after-disconnect"
        );
        server.shutdown();
        agent.shutdown();
    }

    #[test]
    fn wrong_psk_rejects_control_connection() {
        let echo_addr = start_echo_listener();
        let server = start_server(ServerConfig {
            control_addr: "127.0.0.1:0".parse().unwrap(),
            public_addr: "127.0.0.1:0".parse().unwrap(),
            route: "dev".to_owned(),
            psk: b"secret".to_vec(),
        })
        .unwrap();
        let err = match start_agent(AgentConfig {
            server_addr: server.control_addr,
            target_addr: echo_addr,
            route: "dev".to_owned(),
            psk: b"wrong".to_vec(),
        }) {
            Ok(agent) => {
                agent.shutdown();
                panic!("agent with wrong PSK unexpectedly authenticated");
            }
            Err(err) => err,
        };
        assert!(matches!(
            err,
            TunnelError::Auth(AuthError::ServerRejected(_))
        ));
        server.shutdown();
    }

    #[test]
    fn target_unavailable_reports_close_and_recovers_for_next_stream() {
        let reserve = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_addr = reserve.local_addr().unwrap();
        drop(reserve);

        let server = start_server(ServerConfig {
            control_addr: "127.0.0.1:0".parse().unwrap(),
            public_addr: "127.0.0.1:0".parse().unwrap(),
            route: "dev".to_owned(),
            psk: b"secret".to_vec(),
        })
        .unwrap();
        let agent = start_agent(AgentConfig {
            server_addr: server.control_addr,
            target_addr,
            route: "dev".to_owned(),
            psk: b"secret".to_vec(),
        })
        .unwrap();
        thread::sleep(Duration::from_millis(250));

        let mut client = TcpStream::connect(server.public_addr).unwrap();
        client.write_all(b"probe").unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut out = Vec::new();
        match client.read_to_end(&mut out) {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::ConnectionReset => {}
            Err(err) => panic!("unexpected client read error: {err}"),
        }
        assert!(out.is_empty());

        let echo = TcpListener::bind(target_addr).unwrap();
        echo.set_nonblocking(true).unwrap();
        let echo_join = thread::spawn(move || {
            loop {
                match echo.accept() {
                    Ok((mut stream, _)) => {
                        thread::spawn(move || {
                            let mut buf = [0_u8; 1024];
                            loop {
                                match stream.read(&mut buf) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        if stream.write_all(&buf[..n]).is_err() {
                                            break;
                                        }
                                    }
                                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                                        thread::sleep(Duration::from_millis(10));
                                    }
                                    Err(_) => break,
                                }
                            }
                        });
                        break;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        thread::sleep(Duration::from_millis(200));
        assert_eq!(round_trip(server.public_addr, b"recover"), b"recover");
        let _ = echo_join.join();
        server.shutdown();
        agent.shutdown();
    }
}
