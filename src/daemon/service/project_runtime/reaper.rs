use std::sync::mpsc;

type Cleanup = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone)]
pub(super) struct RuntimeReaper {
    sender: mpsc::Sender<Cleanup>,
}

impl RuntimeReaper {
    pub(super) fn new(name: &str) -> Self {
        let (sender, receiver) = mpsc::channel::<Cleanup>();
        let _worker = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                while let Ok(cleanup) = receiver.recv() {
                    cleanup();
                }
            });
        Self { sender }
    }

    pub(super) fn submit(&self, cleanup: impl FnOnce() + Send + 'static) {
        if let Err(disconnected) = self.sender.send(Box::new(cleanup)) {
            (disconnected.0)();
        }
    }
}
