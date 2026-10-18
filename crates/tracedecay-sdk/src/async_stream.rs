//! Asynchronous operation-event stream.
//!
//! The same [`StreamSession`] that drives the blocking iterator, over
//! reqwest's asynchronous client. Waiting for the network never blocks a
//! thread, and dropping the stream closes its connection, which interrupts any
//! read or connect in flight. A host that owns an event loop (the Node
//! binding) drives this type.

use reqwest::Response;

use crate::client::{ClientError, Endpoint, StreamEvent, StreamOptions, media_type};
use crate::stream::{self, Action, ConnectClass, StreamSession};

/// Asynchronous client for the operation-event stream. Clone it to share the
/// connection pool.
#[derive(Clone, Debug)]
pub struct AsyncStreamClient {
    http: reqwest::Client,
    endpoint: Endpoint,
}

impl AsyncStreamClient {
    pub(crate) fn new(http: reqwest::Client, endpoint: Endpoint) -> Self {
        Self { http, endpoint }
    }

    /// Validates the request and returns the stream. No I/O happens until the
    /// first [`AsyncOperationStream::next`], whose result carries a failed
    /// first connection; that connection is never retried.
    pub fn stream_operation(
        &self,
        operation_id: &str,
        options: StreamOptions,
    ) -> Result<AsyncOperationStream, ClientError> {
        crate::observe::finish(StreamSession::new(operation_id, &options)).map(|session| {
            AsyncOperationStream {
                client: self.clone(),
                session,
                body: None,
            }
        })
    }
}

/// Pull-based event stream. It reads the network only inside [`Self::next`],
/// so a slow consumer applies TCP backpressure instead of buffering, and at
/// most the events decoded from one body chunk are held.
///
/// Dropping the future returned by `next`, or the stream itself, closes the
/// connection and abandons the stream; it does not request cancellation of the
/// server operation.
#[derive(Debug)]
pub struct AsyncOperationStream {
    client: AsyncStreamClient,
    session: StreamSession,
    body: Option<Response>,
}

impl AsyncOperationStream {
    /// The next event, or `None` once the stream has ended. After an `Err`
    /// the following call returns `None`.
    pub async fn next(&mut self) -> Option<Result<StreamEvent, ClientError>> {
        loop {
            let action = self.session.next_action();
            if !self.session.is_open() {
                self.body = None;
            }
            match action {
                Action::Deliver(item) => return Some(item),
                Action::Done => return None,
                Action::Connect { delay } => {
                    if !delay.is_zero() {
                        tokio::time::sleep(delay).await;
                    }
                    match self.connect().await {
                        Ok(()) => self.session.connected(),
                        Err(error) => self.session.connect_failed(error),
                    }
                }
                Action::Read => {
                    let read = match self.body.as_mut() {
                        Some(body) => body.chunk().await,
                        None => Ok(None),
                    };
                    match read {
                        Ok(Some(chunk)) => self.session.chunk(&chunk),
                        Ok(None) => self.session.end_of_body(),
                        Err(error) => self
                            .session
                            .read_failed(ClientError::Transport(error.to_string())),
                    }
                }
            }
        }
    }

    async fn connect(&mut self) -> Result<(), ClientError> {
        let url = self.client.endpoint.events_url(&self.session)?;
        let response = self
            .client
            .http
            .get(url)
            .headers(self.client.endpoint.headers("text/event-stream"))
            .send()
            .await
            .map_err(ClientError::transport)?;
        let status = response.status().as_u16();
        match stream::classify_connect(status, media_type(response.headers())) {
            ConnectClass::Stream => {
                self.body = Some(response);
                Ok(())
            }
            ConnectClass::ProblemBody => {
                let body = response.bytes().await.map_err(ClientError::transport)?;
                Err(stream::problem_from_body(status, &body))
            }
            ConnectClass::Refused(error) => Err(error),
        }
    }
}
