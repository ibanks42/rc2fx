//! Commits a prepared settings transaction after the controller has persisted it.
use crate::{Client, ErrorCode, Operation, Reply, Transaction};
use std::io;

#[derive(Debug)]
pub enum ControlFailure {
    Transport(io::Error),
    Refused(ErrorCode),
    InvalidReply,
}
#[derive(Debug)]
#[must_use = "publication can remain pending after settings were saved"]
pub enum Publication {
    /// Published to the audio thread (application on the next callback is tracked separately).
    Published(Transaction),
    /// Settings are saved on disk, but the audio commit needs reconciliation.
    Pending {
        transaction: Transaction,
        failure: ControlFailure,
    },
}
impl Client {
    /// Commits (or retries committing) an already-saved transaction. Transport
    /// errors, refusals, or mismatched replies return `Publication::Pending` so
    /// callers don't mistake a commit failure for a failed save.
    pub fn commit_saved(&mut self, transaction: &Transaction) -> Publication {
        let failure = match self.request(Operation::Commit {
            transaction: transaction.clone(),
        }) {
            Ok(response) => match response.reply {
                Reply::Published {
                    transaction: receipt,
                } if receipt == *transaction => return Publication::Published(receipt),
                Reply::Error { code } => ControlFailure::Refused(code),
                _ => ControlFailure::InvalidReply,
            },
            Err(error) => ControlFailure::Transport(error),
        };
        Publication::Pending {
            transaction: transaction.clone(),
            failure,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MAX_REQUEST, PathMode, Request, Response, Session, VERSION};
    use rc2fx_core::Target;
    use std::{
        fs,
        os::unix::{fs::DirBuilderExt, net::UnixDatagram},
        path::PathBuf,
        thread::{self, JoinHandle},
        time::Duration,
    };
    #[derive(Clone, Copy)]
    enum Step {
        Prepare,
        CommitRefused,
        CommitWrong,
    }
    struct Peer {
        directory: PathBuf,
        thread: Option<JoinHandle<usize>>,
    }
    impl Peer {
        fn new(steps: Vec<Step>) -> Self {
            let session = Session::local().unwrap();
            let name = format!("rc2fx-save-{}-{}", std::process::id(), &session.nonce[..12]);
            let base = std::env::temp_dir();
            let directory = if base
                .join(&name)
                .join("s")
                .as_os_str()
                .as_encoded_bytes()
                .len()
                <= 103
            {
                base.join(&name)
            } else {
                std::path::Path::new("/tmp").join(&name)
            };
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
            let socket = UnixDatagram::bind(directory.join("s")).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let task = thread::spawn(move || {
                let mut buffer = vec![0; MAX_REQUEST + 1];
                let receive = |buffer: &mut [u8]| {
                    let (length, peer) = socket.recv_from(buffer).unwrap();
                    (
                        serde_json::from_slice::<Request>(&buffer[..length]).unwrap(),
                        peer,
                    )
                };
                let send = |request_id, peer: &std::os::unix::net::SocketAddr, reply| {
                    let bytes = serde_json::to_vec(&Response {
                        version: VERSION,
                        request_id,
                        session: session.clone(),
                        reply,
                    })
                    .unwrap();
                    socket.send_to_addr(&bytes, peer).unwrap();
                };
                let (hello, peer) = receive(&mut buffer);
                assert!(matches!(hello.operation, Operation::Hello));
                send(hello.request_id, &peer, Reply::Hello);
                let mut applications = 0;
                let mut staged = None;
                for step in steps {
                    let (request, peer) = receive(&mut buffer);
                    assert_eq!(request.version, VERSION);
                    assert_eq!(request.session.as_ref(), Some(&session));
                    let reply = match (step, request.operation) {
                        (
                            Step::Prepare,
                            Operation::Prepare {
                                transaction,
                                target,
                                mode,
                                ..
                            },
                        ) => {
                            assert_eq!(mode, PathMode::Processed);
                            assert_eq!(transaction.target_id, target.id);
                            staged = Some(transaction.clone());
                            Reply::Prepared { transaction }
                        }
                        (Step::CommitRefused, Operation::Commit { .. }) => Reply::Error {
                            code: ErrorCode::Busy,
                        },
                        (Step::CommitWrong, Operation::Commit { mut transaction }) => {
                            assert_eq!(staged.take().as_ref(), Some(&transaction));
                            applications += 1;
                            transaction.revision += 1;
                            Reply::Published { transaction }
                        }
                        _ => panic!("unexpected save transaction message"),
                    };
                    send(request.request_id, &peer, reply);
                }
                applications
            });
            Self {
                directory,
                thread: Some(task),
            }
        }
        fn client(&self) -> Client {
            Client::connect(&self.directory.join("s")).unwrap()
        }
        fn finish(mut self) -> usize {
            self.thread.take().unwrap().join().unwrap()
        }
    }
    impl Drop for Peer {
        fn drop(&mut self) {
            if let Some(task) = self.thread.take() {
                let _ = task.join();
            }
            let _ = fs::remove_file(self.directory.join("s"));
            let _ = fs::remove_dir(&self.directory);
        }
    }
    fn prepared(client: &mut Client) -> Transaction {
        let target = Target::new("source", "Source", rc2fx_core::TargetKind::Mic, 1);
        let transaction = client.transaction(7, &target.id, 2).unwrap();
        let response = client
            .request(Operation::Prepare {
                transaction: transaction.clone(),
                target: Box::new(target),
                mode: PathMode::Processed,
                reset: crate::ResetPolicy::Allow,
            })
            .unwrap();
        assert!(matches!(
            response.reply,
            Reply::Prepared { transaction: ref receipt } if *receipt == transaction
        ));
        transaction
    }
    #[test]
    fn a_refused_commit_stays_pending_with_the_original_receipt() {
        let peer = Peer::new(vec![Step::Prepare, Step::CommitRefused]);
        let mut client = peer.client();
        let transaction = prepared(&mut client);
        match client.commit_saved(&transaction) {
            Publication::Pending {
                transaction: receipt,
                failure: ControlFailure::Refused(ErrorCode::Busy),
            } => assert_eq!(receipt, transaction),
            other => panic!("unexpected publication {other:?}"),
        }
        assert_eq!(peer.finish(), 0);
    }
    #[test]
    fn a_mismatched_publication_reply_cannot_acknowledge_saved_audio() {
        let peer = Peer::new(vec![Step::Prepare, Step::CommitWrong]);
        let mut client = peer.client();
        let transaction = prepared(&mut client);
        assert!(matches!(
            client.commit_saved(&transaction),
            Publication::Pending {
                failure: ControlFailure::InvalidReply,
                ..
            }
        ));
        assert_eq!(peer.finish(), 1);
    }
}
