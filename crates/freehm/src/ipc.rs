//! Transport-independent IPC handling between the background app and its clients
//! (settings window, `freehm --toggle`, ...). Platform modules provide the listener
//! (per-user named pipe / Unix socket) and call [`serve`] for each connection.

use crate::app::{AppEvent, EventSink};
use freehm_core::ipc::{MAX_FRAME, Request, Response};
use freehm_core::wire::{read_frame, write_frame};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::Duration;

/// Handle one client connection until it closes.
pub fn serve<S: Read + Write>(mut stream: S, sink: &EventSink) {
    loop {
        let req: Request = match read_frame(&mut stream, MAX_FRAME) {
            Ok(r) => r,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::UnexpectedEof && e.kind() != std::io::ErrorKind::BrokenPipe {
                    log::debug!("ipc client error: {e}");
                }
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        sink.send(AppEvent::Ipc(req, tx));
        let resp = rx.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| Response::Error("the application did not respond".into()));
        if write_frame(&mut stream, &resp, MAX_FRAME).is_err() {
            return;
        }
    }
}

/// Client side: a connected stream that sends requests and waits for responses.
pub struct Client {
    stream: Box<dyn ReadWrite + Send>,
}

pub trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

impl Client {
    pub fn connect() -> std::io::Result<Client> {
        Ok(Client { stream: crate::platform::ipc_connect()? })
    }

    pub fn call(&mut self, req: &Request) -> std::io::Result<Response> {
        write_frame(&mut self.stream, req, MAX_FRAME)?;
        read_frame(&mut self.stream, MAX_FRAME)
    }
}
