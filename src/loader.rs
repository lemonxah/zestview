use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use winit::event_loop::EventLoopProxy;

use crate::decode::{self, DecodeError, Decoded};

struct Request {
    id: u64,
    path: PathBuf,
}

pub struct Response {
    pub id: u64,
    pub result: Result<Decoded, DecodeError>,
    pub file_size: u64,
}

pub struct Loader {
    requests: Sender<Request>,
    responses: Receiver<Response>,
}

impl Loader {
    pub fn new(proxy: EventLoopProxy, max_dimension: u32) -> Self {
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        let (response_tx, response_rx) = mpsc::channel();
        thread::Builder::new()
            .name("decoder".into())
            .spawn(move || {
                while let Ok(mut request) = request_rx.recv() {
                    while let Ok(newer) = request_rx.try_recv() {
                        request = newer;
                    }
                    let result = decode::decode(&request.path, max_dimension);
                    let file_size = std::fs::metadata(&request.path)
                        .map(|m| m.len())
                        .unwrap_or(0);
                    let response = Response {
                        id: request.id,
                        result,
                        file_size,
                    };
                    if response_tx.send(response).is_err() {
                        break;
                    }
                    proxy.wake_up();
                }
            })
            .expect("failed to spawn decoder thread");
        Self {
            requests: request_tx,
            responses: response_rx,
        }
    }

    pub fn request(&self, id: u64, path: PathBuf) {
        let _ = self.requests.send(Request { id, path });
    }

    pub fn try_recv(&self) -> Option<Response> {
        self.responses.try_recv().ok()
    }
}
