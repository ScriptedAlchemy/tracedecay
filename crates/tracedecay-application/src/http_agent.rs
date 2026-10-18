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

use std::time::Instant;

use ureq::Agent;
use ureq::config::Config;
use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::time::Duration;
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

fn resume_interrupted<T>(
    timeout: NextTimeout,
    mut wait: impl FnMut(NextTimeout) -> Result<T, ureq::Error>,
) -> Result<T, ureq::Error> {
    let started = Instant::now();
    loop {
        let mut remaining = timeout;
        if let Duration::Exact(after) = timeout.after {
            let Some(after) = after
                .checked_sub(started.elapsed())
                .filter(|after| !after.is_zero())
            else {
                return Err(ureq::Error::Timeout(timeout.reason));
            };
            remaining.after = Duration::Exact(after);
        }
        match wait(remaining) {
            Err(error) if is_interrupted(&error) => {}
            outcome => return outcome,
        }
    }
}

impl<T: Transport> Transport for InterruptResumingTransport<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.0.buffers()
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        resume_interrupted(timeout, |remaining| {
            self.0.transmit_output(amount, remaining)
        })
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        resume_interrupted(timeout, |remaining| self.0.await_input(remaining))
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
    use ureq::unversioned::transport::time::Duration;
    use ureq::unversioned::transport::{
        Buffers, DefaultConnector, LazyBuffers, NextTimeout, Transport,
    };

    use super::{InterruptFirstReadConnector, InterruptResumingTransport, http_agent_over};

    #[derive(Debug)]
    struct InterruptedWait {
        buffers: LazyBuffers,
        pause: std::time::Duration,
        waits: Vec<NextTimeout>,
    }

    impl InterruptedWait {
        fn new(pause: std::time::Duration) -> Self {
            Self {
                buffers: LazyBuffers::new(16, 16),
                pause,
                waits: Vec::new(),
            }
        }

        fn wait(&mut self, timeout: NextTimeout) -> Result<(), ureq::Error> {
            self.waits.push(timeout);
            if self.waits.len() == 1 {
                std::thread::sleep(self.pause);
                Err(std::io::Error::from(std::io::ErrorKind::Interrupted).into())
            } else {
                Ok(())
            }
        }
    }

    impl Transport for InterruptedWait {
        fn buffers(&mut self) -> &mut dyn Buffers {
            &mut self.buffers
        }

        fn transmit_output(&mut self, _: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
            self.wait(timeout)
        }

        fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
            self.wait(timeout).map(|()| true)
        }

        fn is_open(&mut self) -> bool {
            true
        }
    }

    fn global_timeout(after: Duration) -> NextTimeout {
        NextTimeout {
            after,
            reason: ureq::Timeout::Global,
        }
    }

    #[test]
    fn an_expired_read_wait_returns_its_original_timeout() {
        let mut transport =
            InterruptResumingTransport(InterruptedWait::new(std::time::Duration::from_millis(15)));
        let result = transport.await_input(global_timeout(Duration::from_millis(5)));
        assert!(matches!(
            result,
            Err(ureq::Error::Timeout(ureq::Timeout::Global))
        ));
        assert_eq!(
            transport.0.waits.len(),
            1,
            "a spent wait must not resume IO"
        );
    }

    #[test]
    fn an_expired_write_wait_returns_its_original_timeout() {
        let mut transport =
            InterruptResumingTransport(InterruptedWait::new(std::time::Duration::from_millis(15)));
        let result = transport.transmit_output(1, global_timeout(Duration::from_millis(5)));
        assert!(matches!(
            result,
            Err(ureq::Error::Timeout(ureq::Timeout::Global))
        ));
        assert_eq!(
            transport.0.waits.len(),
            1,
            "a spent wait must not resume IO"
        );
    }

    #[test]
    fn a_resumed_read_wait_uses_only_the_remaining_budget() {
        let mut transport =
            InterruptResumingTransport(InterruptedWait::new(std::time::Duration::from_millis(20)));
        assert!(
            transport
                .await_input(global_timeout(Duration::from_secs(2)))
                .unwrap()
        );
        assert_eq!(transport.0.waits.len(), 2);
        assert!(transport.0.waits[1].after < transport.0.waits[0].after);
        assert_eq!(transport.0.waits[1].reason, ureq::Timeout::Global);
    }

    #[test]
    fn an_unlimited_wait_stays_unlimited_when_resumed() {
        let mut transport =
            InterruptResumingTransport(InterruptedWait::new(std::time::Duration::ZERO));
        assert!(
            transport
                .await_input(global_timeout(Duration::NotHappening))
                .unwrap()
        );
        assert_eq!(
            transport.0.waits,
            vec![global_timeout(Duration::NotHappening); 2]
        );
    }

    #[test]
    fn a_zero_budget_returns_timeout_without_starting_io() {
        let mut transport =
            InterruptResumingTransport(InterruptedWait::new(std::time::Duration::ZERO));
        let result = transport.await_input(global_timeout(Duration::from_millis(0)));
        assert!(matches!(
            result,
            Err(ureq::Error::Timeout(ureq::Timeout::Global))
        ));
        assert!(transport.0.waits.is_empty());
    }

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
