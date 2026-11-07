//! The app's USB session: one thread owns the HID handle and serves requests
//! in order. After a broken request, queued requests are refused and only a
//! fresh read reopens the session, so a change is never sent on a state the
//! app has not re-read, and none is replayed.
use super::{REQUEST_LIMIT, RequestError};
use rc2fx_installer::control::{Client, Failure, Reply, Request};
use tokio::sync::{mpsc, oneshot};

#[cfg(feature = "mock-mixer")]
mod mock;

struct Job {
    request: Request,
    answer: oneshot::Sender<Result<Reply, String>>,
}

/// Something that serves requests: the real client, or a test stand-in.
trait Session {
    fn request(&mut self, request: &Request) -> Result<Reply, Failure>;
    fn is_open(&self) -> bool;
    fn reopen(&mut self) -> Result<(), String>;
}

impl Session for Client {
    fn request(&mut self, request: &Request) -> Result<Reply, Failure> {
        Client::request(self, request)
    }
    fn is_open(&self) -> bool {
        Client::is_open(self)
    }
    fn reopen(&mut self) -> Result<(), String> {
        Client::reopen(self)
    }
}

#[derive(Clone)]
pub(super) struct Usb {
    jobs: mpsc::Sender<Job>,
}

impl Usb {
    /// Open the session; also returns the mixer's USB serial.
    pub(super) async fn connect() -> Result<(Self, String), String> {
        let (jobs, receiver) = mpsc::channel::<Job>(8);
        let (ready, opened) = oneshot::channel::<Result<String, String>>();
        std::thread::Builder::new()
            .name("rc2fx-usb".into())
            .spawn(move || {
                #[cfg(feature = "mock-mixer")]
                {
                    let _ = ready.send(Ok(mock::SERIAL.to_owned()));
                    serve(receiver, mock::Connection::open());
                }
                // HID handles stay on the thread that opened them.
                #[cfg(not(feature = "mock-mixer"))]
                match Client::open() {
                    Ok(client) => {
                        let _ = ready.send(Ok(client.serial().to_owned()));
                        serve(receiver, client);
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let mixer = opened
            .await
            .map_err(|_| "USB worker stopped".to_owned())??;
        Ok((Self { jobs }, mixer))
    }

    pub(super) async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<Reply, RequestError> {
        if body
            .as_ref()
            .is_some_and(|b| serde_json::to_vec(b).map_or(true, |b| b.len() > REQUEST_LIMIT))
        {
            return Err(RequestError::TooLarge);
        }
        let (answer, reply) = oneshot::channel();
        let request = Request {
            method: method.into(),
            path: path.into(),
            body,
        };
        self.jobs
            .send(Job { request, answer })
            .await
            .map_err(|_| RequestError::Usb("USB disconnected".into()))?;
        reply
            .await
            .map_err(|_| RequestError::Usb("USB worker stopped".into()))?
            .map_err(RequestError::Usb)
    }
}

fn serve(mut receiver: mpsc::Receiver<Job>, mut session: impl Session) {
    while let Some(job) = receiver.blocking_recv() {
        if job.answer.is_closed() {
            continue;
        }
        if !session.is_open() {
            if job.request.method != "GET" || job.request.body.is_some() {
                let _ = job.answer.send(Err(
                    "USB reconnected; wait for fresh state before sending changes".into(),
                ));
                continue;
            }
            if let Err(error) = session.reopen() {
                let _ = job.answer.send(Err(error));
                continue;
            }
        }
        match session.request(&job.request) {
            Ok(reply) => {
                let _ = job.answer.send(Ok(reply));
            }
            Err(Failure::NotSent(error)) => {
                let _ = job.answer.send(Err(error));
            }
            Err(Failure::Broken(error)) => {
                let _ = job.answer.send(Err(error));
                while let Ok(queued) = receiver.try_recv() {
                    let _ = queued
                        .answer
                        .send(Err("USB disconnected; queued request was not sent".into()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    /// Breaks on its first request, then serves reads after reopening.
    struct Flaky {
        open: bool,
        broken_once: bool,
        reopens: Arc<AtomicUsize>,
    }

    impl Session for Flaky {
        fn request(&mut self, _: &Request) -> Result<Reply, Failure> {
            if !self.broken_once {
                self.broken_once = true;
                self.open = false;
                return Err(Failure::Broken("lost".into()));
            }
            Ok(Reply {
                status: 200,
                body: serde_json::Value::Null,
            })
        }
        fn is_open(&self) -> bool {
            self.open
        }
        fn reopen(&mut self) -> Result<(), String> {
            self.reopens.fetch_add(1, Ordering::SeqCst);
            self.open = true;
            Ok(())
        }
    }

    fn job(
        method: &str,
        body: Option<serde_json::Value>,
    ) -> (Job, oneshot::Receiver<Result<Reply, String>>) {
        let (answer, reply) = oneshot::channel();
        let request = Request {
            method: method.into(),
            path: "api/v1/config".into(),
            body,
        };
        (Job { request, answer }, reply)
    }

    #[test]
    fn after_a_broken_request_queued_ones_are_refused_and_only_a_read_reopens() {
        let (jobs, receiver) = mpsc::channel(8);
        let reopens = Arc::new(AtomicUsize::new(0));
        let (first, first_reply) = job("PUT", Some(serde_json::json!({})));
        let (queued, queued_reply) = job("PUT", Some(serde_json::json!({})));
        jobs.blocking_send(first).unwrap();
        jobs.blocking_send(queued).unwrap();
        let (change, change_reply) = job("PUT", Some(serde_json::json!({})));
        let (read, read_reply) = job("GET", None);
        jobs.blocking_send(change).unwrap();
        jobs.blocking_send(read).unwrap();
        drop(jobs);
        let session = Flaky {
            open: true,
            broken_once: false,
            reopens: reopens.clone(),
        };
        serve(receiver, session);
        assert!(first_reply.blocking_recv().unwrap().is_err());
        assert!(
            queued_reply
                .blocking_recv()
                .unwrap()
                .unwrap_err()
                .contains("not sent")
        );
        // Queued with the failure, so also refused unsent.
        assert!(
            change_reply
                .blocking_recv()
                .unwrap()
                .unwrap_err()
                .contains("not sent")
        );
        assert!(
            read_reply
                .blocking_recv()
                .unwrap()
                .unwrap_err()
                .contains("not sent")
        );
        assert_eq!(reopens.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_change_waits_for_a_fresh_read_after_reconnecting() {
        let (jobs, receiver) = mpsc::channel(8);
        let reopens = Arc::new(AtomicUsize::new(0));
        let session = Flaky {
            open: false,
            broken_once: true,
            reopens: reopens.clone(),
        };
        let (change, change_reply) = job("PUT", Some(serde_json::json!({})));
        let (read, read_reply) = job("GET", None);
        jobs.blocking_send(change).unwrap();
        jobs.blocking_send(read).unwrap();
        drop(jobs);
        serve(receiver, session);
        assert!(
            change_reply
                .blocking_recv()
                .unwrap()
                .unwrap_err()
                .contains("fresh state")
        );
        assert_eq!(read_reply.blocking_recv().unwrap().unwrap().status, 200);
        assert_eq!(reopens.load(Ordering::SeqCst), 1);
    }
}
