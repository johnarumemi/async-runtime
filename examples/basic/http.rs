//! Code related to http client
//!
//! Makes only GET requests to the delayserver in `rust-async-utils`
#![allow(unused)]
use std::{
    future::Future,
    io::{ErrorKind, Read, Write},
    pin::Pin,
    task::{Context, Poll},
};

use mio::Interest;

use crate::runtime::{self, reactor, MyWaker};

static DELAYSERVER: &str = "127.0.0.1:8080";

// traits and types from reading from a IO source

/// The main http client responsible for I/O operations via kernel
///
/// While not required, we can add state to it at a later date + good for encapsulating
/// functionality related to making http requests to a server.
pub struct Http;

impl Http {
    /// Returns a future that yields the response of the HTTP request
    pub fn get(path: &str) -> impl Future<Output = String> {
        HttpGetFuture::new(path)
    }
}

/// A Leaf Future
///
/// This future is !Unpin, as there is nothing that makes it unsafe
/// to move it around. Only futures created via async/await are self-referential.
struct HttpGetFuture {
    /// Optional since we do not connect on instantiation of HttpGetFuture
    stream: Option<mio::net::TcpStream>,
    /// data read from TCP stream is placed here
    buffer: Vec<u8>,
    path: String,
    /// id retrieved from reactor for our source we want to track events on.
    id: usize,
}

impl HttpGetFuture {
    fn new(path: &str) -> Self {
        let id = reactor().next_id();

        Self {
            // do not connect yet, only on first poll
            stream: None,
            buffer: Vec::new(),
            path: path.to_string(),
            id,
        }
    }

    /// Makes a non-blocking write request to the delayserver
    /// and stores the created stream on the future.
    fn write_request(&mut self) {
        // Create a standard library stream first and wrap it in mio stream
        let stream = std::net::TcpStream::connect(DELAYSERVER).unwrap();
        stream.set_nonblocking(true).unwrap();
        let mut stream = mio::net::TcpStream::from_std(stream);

        let req = get_req(&self.path);

        // non-blocking IO operation
        stream.write_all(&req).unwrap();

        // store stream on future
        self.stream = Some(stream);
    }
}

impl Future for HttpGetFuture {
    type Output = String;
    /// Below can be viewed as a simple state machine with 3 possible states.
    ///
    /// 1. Not Started: indicated by self.stream being None.
    /// 2. Pending: indicatd by self.stream being Some and a read to `stream.read`
    ///    returning `ErrorKind::WouldBlock`.
    /// 3. Resolved, indicated by self.stream being Some and `stream.read`
    ///    returning 0 bytes.
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        // If stream is none, this is first time we are polling the future, so
        // "progressing" the future, means making a request to the delayserver.

        let id = self.id;

        if self.stream.is_none() {
            // Send GET request and store created stream on future.
            println!("FIRST POLL - STARTING OPERATION - Make GET REQUEST");
            self.write_request();

            // It should be a mio::net::TcpStream, hence
            // already implements the mio `Source` trait.
            let stream = self.stream.as_mut().unwrap();

            // register interest with event queue
            reactor().register(stream, Interest::READABLE, id);

            // rather than pass in `waker`, we now pass in the full Context `cx`
            reactor().set_waker(cx, id);

            // below was removed to enable us immediately poll the TcpStream.
            // This means we will not return control to the scheduler if we happen
            // to get the response immediately.
        }

        // Reach here if this is not first poll on the future.
        // "Progressing" the future means waiting / checking if response is ready.
        let mut buff = vec![0u8; 4096]; // 4Kb buffer

        // we keep trying to read from stream until we reach end
        // or if operation would block
        loop {
            match self.stream.as_mut().unwrap().read(&mut buff) {
                Ok(0) => {
                    // we have reached end of buffer
                    let response = String::from_utf8_lossy(&self.buffer).to_string();

                    // No longer interested in notifications for this event source
                    reactor().deregister(self.stream.as_mut().unwrap(), id);

                    return Poll::Ready(response);
                }
                Ok(n) => {
                    // we have read N bytes, extend buffer on future with temporary buffer.

                    self.buffer.extend_from_slice(&buff[..n]);
                    continue;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    // we would block, return NotReady
                    // also reach here if we are interrupted
                    // return PollState::NotReady;

                    // NOTE: we can reach here via been polled the first or subsequent times. We
                    // must ensure that we always register the latest waker with the Reactor if we
                    // are still waiting to be notified. This is because the future may have been
                    // polled on a different executor between polls. So the piror waker stored in
                    // reactor may be associated with the previous executor it was on.
                    reactor().set_waker(cx, id);
                    break Poll::Pending; // break and retun value from `loop`
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {
                    // try reading again
                    continue;
                }
                // We do no error handling, so all we do is panic in below situation.
                Err(e) => panic!("IO Error: {e:?}"),
            }
        }
    }
}

/// Helper function to write actual GET request as a stream of bytes
fn get_req(path: &str) -> Vec<u8> {
    let req = format!(
        "GET {path} HTTP/1.1\r\n\
             Host: localhost\r\n\
             Connection: close\r\n\
             \r\n"
    );

    req.into_bytes()
}
