//! The one way TraceDecay builds an outbound HTTP agent.
//!
//! ureq sets `SO_RCVTIMEO` on every socket to enforce its deadlines, and Linux
//! never restarts a socket read that has a receive timeout: any caught signal
//! fails it with `EINTR`, even under `SA_RESTART` (signal(7)). The CLI and
//! daemon catch `SIGCHLD` whenever tokio supervises a child process (bounded
//! git and version probes), so a release lookup or GitHub read running beside
//! one would surface `Interrupted` as a transport failure, and callers would
//! report the network as unreachable. ureq's TCP transport returns that error
//! from its raw `read`. The transport built here resumes the interrupted wait,
//! the POSIX meaning of `EINTR` and what `std::io::Read::read_exact` does. It
//! is not a retry of a failed request: nothing was read or consumed, and the
//! next wait uses the same deadline ureq handed in.

use ureq::Agent;
use ureq::config::Config;
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, DefaultConnector, NextTimeout, Transport,
};

/// An agent for `config` over ureq's default connector chain (proxy, TCP,
/// TLS) with interrupted socket waits resumed.
pub fn http_agent(config: Config) -> Agent {
    http_agent_over(config, DefaultConnector::new())
}

/// [`http_agent`] over an explicit connector chain.
pub fn http_agent_over(config: Config, connector: impl Connector) -> Agent {
    Agent::with_parts(
        config,
        ResumeInterrupted(connector),
        DefaultResolver::default(),
    )
}

#[derive(Debug)]
struct ResumeInterrupted<C>(C);

impl<C: Connector> Connector for ResumeInterrupted<C> {
    type Out = InterruptResumingTransport<C::Out>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(self
            .0
            .connect(details, chained)?
            .map(InterruptResumingTransport))
    }
}

#[derive(Debug)]
struct InterruptResumingTransport<T>(T);

fn is_interrupted(error: &ureq::Error) -> bool {
    matches!(error, ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::Interrupted)
}

impl<T: Transport> Transport for InterruptResumingTransport<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.0.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        loop {
            match self.0.transmit_output(amount, timeout) {
                Err(error) if is_interrupted(&error) => {}
                outcome => return outcome,
            }
        }
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        loop {
            match self.0.await_input(timeout) {
                Err(error) if is_interrupted(&error) => {}
                outcome => return outcome,
            }
        }
    }

    fn is_open(&mut self) -> bool {
        self.0.is_open()
    }

    fn is_tls(&self) -> bool {
        self.0.is_tls()
    }
}

/// A connector whose transports fail their first input wait with `EINTR`,
/// exactly as a socket read interrupted by a caught signal does: nothing is
/// read, and the connection is intact.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug)]
pub struct InterruptFirstReadConnector<C>(pub C);

#[cfg(any(test, feature = "test-helpers"))]
impl<C: Connector> Connector for InterruptFirstReadConnector<C> {
    type Out = InterruptFirstReadTransport<C::Out>;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(self
            .0
            .connect(details, chained)?
            .map(|inner| InterruptFirstReadTransport {
                inner,
                interrupted: false,
            }))
    }
}

#[cfg(any(test, feature = "test-helpers"))]
#[derive(Debug)]
pub struct InterruptFirstReadTransport<T> {
    inner: T,
    interrupted: bool,
}

#[cfg(any(test, feature = "test-helpers"))]
impl<T: Transport> Transport for InterruptFirstReadTransport<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        self.inner.transmit_output(amount, timeout)
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        if !self.interrupted {
            self.interrupted = true;
            return Err(std::io::Error::from(std::io::ErrorKind::Interrupted).into());
        }
        self.inner.await_input(timeout)
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }

    fn is_tls(&self) -> bool {
        self.inner.is_tls()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use ureq::Agent;
    use ureq::unversioned::resolver::DefaultResolver;
    use ureq::unversioned::transport::DefaultConnector;

    use super::{InterruptFirstReadConnector, http_agent_over};

    /// Answers one request per accepted connection with `body`.
    fn serve(body: &'static str, connections: usize) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().take(connections) {
                let mut stream = stream.unwrap();
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                while !request.ends_with(b"\r\n\r\n") && stream.read_exact(&mut byte).is_ok() {
                    request.push(byte[0]);
                }
                // The non-resuming client has already dropped its connection.
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        url
    }

    #[test]
    fn an_interrupted_socket_wait_is_resumed_not_reported() {
        let url = serve("release metadata", 2);

        let plain = Agent::with_parts(
            Agent::config_builder().build(),
            InterruptFirstReadConnector(DefaultConnector::new()),
            DefaultResolver::default(),
        );
        let error = plain.get(&url).call().unwrap_err();
        assert!(
            matches!(&error, ureq::Error::Io(io) if io.kind() == std::io::ErrorKind::Interrupted),
            "the injected EINTR reaches an agent that does not resume it: {error}"
        );

        let resuming = http_agent_over(
            Agent::config_builder().build(),
            InterruptFirstReadConnector(DefaultConnector::new()),
        );
        let body = resuming
            .get(&url)
            .call()
            .unwrap()
            .body_mut()
            .read_to_string()
            .unwrap();
        assert_eq!(body, "release metadata");
    }
}
