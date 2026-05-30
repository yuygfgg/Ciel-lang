use crate::error::Result;
use crate::frame::{self, Frame, FrameKind};
use std::collections::HashMap;
use std::io::{self, Read};
use std::net::{Shutdown, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub enum StreamEvent {
    RemoteFrame(Frame),
    LocalReadClosed,
    LocalReadFailed(String),
}

#[derive(Clone)]
pub struct ControlWriter {
    inner: Arc<Mutex<TcpStream>>,
}

impl ControlWriter {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            inner: Arc::new(Mutex::new(stream)),
        }
    }

    pub fn send(&self, frame: Frame) -> Result<()> {
        let mut writer = self.inner.lock().expect("control writer poisoned");
        frame::write_frame(&mut *writer, &frame)
    }

    pub fn shutdown(&self) {
        if let Ok(writer) = self.inner.lock() {
            let _ = writer.shutdown(Shutdown::Both);
        }
    }
}

#[derive(Clone)]
pub struct StreamRegistry {
    pub(crate) inner: Arc<Mutex<HashMap<u32, mpsc::Sender<StreamEvent>>>>,
}

impl StreamRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn insert(&self, stream_id: u32, tx: mpsc::Sender<StreamEvent>) {
        self.inner
            .lock()
            .expect("stream registry poisoned")
            .insert(stream_id, tx);
    }

    pub fn remove(&self, stream_id: u32) {
        self.inner
            .lock()
            .expect("stream registry poisoned")
            .remove(&stream_id);
    }
}

pub fn spawn_local_reader(
    stream_id: u32,
    mut reader: TcpStream,
    control: ControlWriter,
    tx: mpsc::Sender<StreamEvent>,
    shutdown: Arc<AtomicBool>,
    label: &'static str,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let _ = reader.set_read_timeout(Some(Duration::from_millis(200)));
        let mut buf = vec![0_u8; 16_384];
        loop {
            if shutdown.load(Ordering::SeqCst) {
                break;
            }
            match reader.read(&mut buf) {
                Ok(0) => {
                    let _ = tx.send(StreamEvent::LocalReadClosed);
                    break;
                }
                Ok(n) => {
                    let frame = Frame::new(FrameKind::Data, stream_id, buf[..n].to_vec());
                    match frame.and_then(|frame| control.send(frame)) {
                        Ok(()) => {}
                        Err(err) => {
                            let _ = tx.send(StreamEvent::LocalReadFailed(format!(
                                "{label} read path failed: {err}"
                            )));
                            break;
                        }
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
                Err(err) if err.kind() == io::ErrorKind::TimedOut => continue,
                Err(err) => {
                    let _ = tx.send(StreamEvent::LocalReadFailed(format!(
                        "{label} read error: {err}"
                    )));
                    break;
                }
            }
        }
    })
}
