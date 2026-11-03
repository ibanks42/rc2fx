//! Single-target control transaction. Persistence stays with the controller.
use crate::{Client, ErrorCode, Operation, PathMode, Reply, Transaction};
use rc2fx_core::Target;
use std::io;

#[derive(Debug)]
pub enum ControlFailure {
    Transport(io::Error),
    Refused(ErrorCode),
    InvalidReply,
}
#[derive(Debug)]
pub enum SaveFailure<E> {
    Preparation {
        transaction: Option<Transaction>,
        failure: ControlFailure,
    },
    /// Includes uncertain persistence errors unchanged. Cancellation does not
    /// assert that the old file was restored; the Store defines that outcome.
    Persistence {
        transaction: Transaction,
        error: E,
        cancellation_confirmed: bool,
    },
}
#[derive(Debug)]
#[must_use = "publication can remain pending after settings were saved"]
pub enum Publication {
    /// Published to the audio reader. Actual applied acknowledgement is separate.
    Published(Transaction),
    /// Desired settings were saved; retain them and reconcile this receipt.
    Pending {
        transaction: Transaction,
        failure: ControlFailure,
    },
}
impl Publication {
    pub fn transaction(&self) -> &Transaction {
        match self {
            Self::Published(transaction) | Self::Pending { transaction, .. } => transaction,
        }
    }
}
#[derive(Debug)]
#[must_use = "check publication before claiming audio applied the saved value"]
pub struct Saved<T> {
    pub value: T,
    pub publication: Publication,
}
impl Client {
    /// Prepare one live target, invoke persistence exactly once, then publish.
    /// No audio commit is sent if preparation or persistence fails. After a
    /// successful save, a missing/refused commit reply returns Saved + Pending,
    /// never an error that could be mistaken for a failed save or rolled back.
    /// This does not coordinate a multi-path preset or write any files itself.
    pub fn save_and_commit<T, E>(
        &mut self,
        context: u64,
        revision: u64,
        target: &Target,
        mode: PathMode,
        save: impl FnOnce() -> Result<T, E>,
    ) -> Result<Saved<T>, SaveFailure<E>> {
        let transaction = self
            .transaction(context, &target.id, revision)
            .map_err(|error| SaveFailure::Preparation {
                transaction: None,
                failure: ControlFailure::Transport(error),
            })?;
        let prepared = match self.request(Operation::Prepare {
            transaction: transaction.clone(),
            target: Box::new(target.clone()),
            mode,
        }) {
            Ok(response) => match response.reply {
                Reply::Prepared {
                    transaction: receipt,
                } if receipt == transaction => Ok(()),
                Reply::Error { code } => Err(ControlFailure::Refused(code)),
                _ => Err(ControlFailure::InvalidReply),
            },
            Err(error) => Err(ControlFailure::Transport(error)),
        };
        if let Err(failure) = prepared {
            return Err(SaveFailure::Preparation {
                transaction: Some(transaction),
                failure,
            });
        }
        let value = match save() {
            Ok(value) => value,
            Err(error) => {
                let cancellation_confirmed = self.request(Operation::Cancel { transaction: transaction.clone() }).is_ok_and(|response| matches!(response.reply, Reply::Cancelled { transaction: receipt } if receipt == transaction));
                return Err(SaveFailure::Persistence {
                    transaction,
                    error,
                    cancellation_confirmed,
                });
            }
        };
        Ok(Saved {
            value,
            publication: self.commit_saved(&transaction),
        })
    }
    /// Retry only the audio commit for an already-saved desired value. The
    /// receipt remains pinned to its original process, even on a new connection.
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
    use crate::{MAX_REQUEST, Request, Response, Session, VERSION};
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
        PrepareLost,
        PrepareRefused,
        Cancel,
        Commit,
        CommitLost,
        CommitRefused,
        CommitWrong,
    }
    #[derive(Default)]
    struct Trace {
        applications: usize,
        commits: usize,
        cancellations: usize,
    }
    struct Peer {
        directory: PathBuf,
        thread: Option<JoinHandle<Trace>>,
    }
    impl Peer {
        fn new(steps: Vec<Step>) -> Self {
            let session = Session::local().unwrap();
            let directory = std::env::temp_dir().join(format!(
                "rc2fx-control-save-{}-{}",
                std::process::id(),
                session.nonce
            ));
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
                let mut trace = Trace::default();
                let mut staged = None;
                let mut published = None;
                for step in steps {
                    let (request, peer) = receive(&mut buffer);
                    assert_eq!(request.version, VERSION);
                    assert_eq!(request.session.as_ref(), Some(&session));
                    let reply = match (step, request.operation) {
                        (
                            Step::Prepare | Step::PrepareLost,
                            Operation::Prepare {
                                transaction,
                                target,
                                mode,
                            },
                        ) => {
                            assert_eq!(mode, PathMode::Processed);
                            assert_eq!(transaction.target_id, target.id);
                            staged = Some(transaction.clone());
                            // A lost reply is simulated after the receiver accepted the stage.
                            if matches!(step, Step::PrepareLost) {
                                None
                            } else {
                                Some(Reply::Prepared { transaction })
                            }
                        }
                        (Step::PrepareRefused, Operation::Prepare { .. }) => Some(Reply::Error {
                            code: ErrorCode::Busy,
                        }),
                        (Step::Cancel, Operation::Cancel { transaction }) => {
                            assert_eq!(staged.take().as_ref(), Some(&transaction));
                            trace.cancellations += 1;
                            Some(Reply::Cancelled { transaction })
                        }
                        (
                            Step::Commit
                            | Step::CommitLost
                            | Step::CommitRefused
                            | Step::CommitWrong,
                            Operation::Commit { mut transaction },
                        ) => {
                            trace.commits += 1;
                            if matches!(step, Step::CommitRefused) {
                                Some(Reply::Error {
                                    code: ErrorCode::Busy,
                                })
                            } else {
                                if let Some(receipt) = &published {
                                    assert_eq!(receipt, &transaction);
                                } else {
                                    assert_eq!(staged.take().as_ref(), Some(&transaction));
                                    trace.applications += 1;
                                    published = Some(transaction.clone());
                                }
                                if matches!(step, Step::CommitLost) {
                                    None
                                } else {
                                    if matches!(step, Step::CommitWrong) {
                                        transaction.revision += 1;
                                    }
                                    Some(Reply::Published { transaction })
                                }
                            }
                        }
                        _ => panic!("unexpected save transaction message"),
                    };
                    if let Some(reply) = reply {
                        send(request.request_id, &peer, reply);
                    }
                }
                trace
            });
            Self {
                directory,
                thread: Some(task),
            }
        }
        fn client(&self) -> Client {
            Client::connect(&self.directory.join("s")).unwrap()
        }
        fn finish(mut self) -> Trace {
            self.thread.take().unwrap().join().unwrap()
        }
    }
    impl Drop for Peer {
        fn drop(&mut self) {
            if let Some(task) = self.thread.take() {
                let _ = task.join();
            }
            // Only the socket in our fresh private directory; unknown entries survive.
            let _ = fs::remove_file(self.directory.join("s"));
            let _ = fs::remove_dir(&self.directory);
        }
    }
    fn target() -> Target {
        Target::new("source", "Source", rc2fx_core::TargetKind::Mic, 1)
    }
    #[test]
    fn refused_or_lost_preparation_never_invokes_persistence() {
        for step in [Step::PrepareRefused, Step::PrepareLost] {
            let peer = Peer::new(vec![step]);
            let mut client = peer.client();
            let mut saves = 0;
            let outcome = client.save_and_commit(7, 2, &target(), PathMode::Processed, || {
                saves += 1;
                Ok::<_, ()>(77)
            });
            assert!(matches!(
                outcome,
                Err(SaveFailure::Preparation {
                    transaction: Some(_),
                    ..
                })
            ));
            assert_eq!(saves, 0);
            assert_eq!(peer.finish().commits, 0);
        }
    }
    #[test]
    fn persistence_failure_preserves_the_error_and_cancels_without_committing_audio() {
        let peer = Peer::new(vec![Step::Prepare, Step::Cancel]);
        let mut client = peer.client();
        let outcome = client.save_and_commit(7, 2, &target(), PathMode::Processed, || {
            Err::<(), _>("uncertain persistence")
        });
        assert!(matches!(
            outcome,
            Err(SaveFailure::Persistence {
                error: "uncertain persistence",
                cancellation_confirmed: true,
                ..
            })
        ));
        let trace = peer.finish();
        assert_eq!(trace.commits, 0);
        assert_eq!(trace.cancellations, 1);
    }
    #[test]
    fn lost_or_refused_commit_is_saved_pending_and_retry_does_not_repeat_the_save() {
        for step in [Step::CommitLost, Step::CommitRefused] {
            let peer = Peer::new(vec![Step::Prepare, step, Step::Commit]);
            let mut client = peer.client();
            let mut saves = 0;
            let saved = client
                .save_and_commit(7, 2, &target(), PathMode::Processed, || {
                    saves += 1;
                    Ok::<_, ()>(77)
                })
                .unwrap();
            assert_eq!(saved.value, 77);
            assert!(matches!(saved.publication, Publication::Pending { .. }));
            assert!(matches!(
                client.commit_saved(saved.publication.transaction()),
                Publication::Published(_)
            ));
            assert_eq!(saves, 1);
            let trace = peer.finish();
            assert_eq!(trace.commits, 2);
            assert_eq!(trace.applications, 1);
        }
    }
    #[test]
    fn a_mismatched_publication_reply_cannot_acknowledge_saved_audio() {
        let peer = Peer::new(vec![Step::Prepare, Step::CommitWrong]);
        let mut client = peer.client();
        let saved = client
            .save_and_commit(7, 2, &target(), PathMode::Processed, || Ok::<_, ()>(77))
            .unwrap();
        assert!(matches!(
            saved.publication,
            Publication::Pending {
                failure: ControlFailure::InvalidReply,
                ..
            }
        ));
        assert_eq!(saved.value, 77);
        assert_eq!(peer.finish().applications, 1);
    }
}
