//! Developer mode: SSH access to the connected mixer, managed over USB 1.
//!
//! Developer mode only shows these controls. Hiding them changes nothing on
//! the mixer; only Disable removes RC2FX's key. Nothing here flashes.
use super::*;
use rc2fx_core::{SshRequest, SshStatus};

/// What the mixer last said about SSH, for the current connection only.
#[derive(Default)]
pub(super) struct Ssh {
    /// `None`: not read yet. `Some(Ok(None))`: the mixer software is too old.
    status: Option<std::result::Result<Option<SshStatus>, String>>,
    /// An Enable (`true`) or Disable (`false`) sent and not yet answered.
    pending: Option<bool>,
    /// The mixer (serial) whose host keys were pinned, and where; an error
    /// when pinning failed. Reset with every status.
    pinned: Option<(
        String,
        std::result::Result<rc2fx_installer::PinnedHost, String>,
    )>,
    /// Preparing this computer's key before an Enable is sent.
    preparing_key: bool,
    /// Requests sent and not yet answered, oldest first: (id, is a change).
    /// A reply whose id is not here belongs to an earlier connection or
    /// state and is ignored.
    sent: std::collections::VecDeque<(u64, bool)>,
    /// The next request id. Never reset, so ids are not reused after a clear.
    next_id: u64,
    /// This computer's RC2FX key fingerprint, read without creating a key;
    /// `None` when there is no key or it could not be read.
    local_fingerprint: Option<String>,
    /// Bumped by every clear. Confirmations and background work carry it and
    /// count only if it is unchanged, even across a same-mixer reconnect.
    epoch: u64,
}

impl Ssh {
    /// Forget everything: a disconnect, a (re)connection, a lost or restored
    /// stream, or Developer mode changing.
    pub(super) fn clear(&mut self) {
        let epoch = self.epoch.wrapping_add(1);
        let next_id = self.next_id;
        *self = Self::default();
        self.epoch = epoch;
        self.next_id = next_id;
    }

    fn track(&mut self, change: bool) -> u64 {
        self.next_id += 1;
        self.sent.push_back((self.next_id, change));
        self.next_id
    }

    /// Match a reply to its request. Unknown ids are stale. Replies come in
    /// order, so anything sent before this request was answered already. A
    /// read answered while a change is still on its way must not replace
    /// what is shown.
    fn accept_reply(&mut self, id: u64) -> bool {
        let Some(at) = self.sent.iter().position(|&(sent, _)| sent == id) else {
            return false;
        };
        let change = self.sent[at].1;
        self.sent.drain(..=at);
        if change {
            self.pending = None;
        }
        change || !self.sent.iter().any(|&(_, change)| change)
    }
}

/// The status line and an optional caution, in plain words.
pub(super) fn describe(
    status: Option<&std::result::Result<Option<SshStatus>, String>>,
    pending: Option<bool>,
    local_fingerprint: Option<&str>,
) -> (String, Option<String>) {
    if let Some(enable) = pending {
        let what = if enable { "Enabling" } else { "Disabling" };
        return (format!("{what} SSH access…"), None);
    }
    let status = match status {
        None => return ("Checking…".into(), None),
        Some(Err(error)) => return (format!("Could not read SSH access: {error}"), None),
        Some(Ok(None)) => {
            return (
                "Update the RC2FX mixer software to manage SSH access from here.".into(),
                None,
            );
        }
        Some(Ok(Some(status))) => status,
    };
    let mut line = match (status.managed, status.managed_fingerprint.as_deref()) {
        (false, _) => "Off: no RC2FX-managed key on the mixer".into(),
        (true, Some(fp)) if Some(fp) == local_fingerprint => {
            format!("On with this computer's key ({fp})")
        }
        (true, Some(fp)) if local_fingerprint.is_some() => {
            format!("On with an RC2FX key from another computer ({fp})")
        }
        (true, Some(fp)) => format!("On with an RC2FX-managed key ({fp})"),
        (true, None) => "On with an RC2FX-managed key".into(),
    };
    if status.server_running == Some(false) {
        line.push_str(". The SSH server is not running");
    }
    let caution = (status.unmanaged_keys > 0).then(|| {
        format!(
            "{} other key{} can also log in. RC2FX did not add {} and leaves {} alone.",
            status.unmanaged_keys,
            if status.unmanaged_keys == 1 { "" } else { "s" },
            if status.unmanaged_keys == 1 {
                "it"
            } else {
                "them"
            },
            if status.unmanaged_keys == 1 {
                "it"
            } else {
                "them"
            },
        )
    });
    (line, caution)
}

/// Whether the mixer's managed key is this computer's, so the login
/// command will work from here.
pub(super) fn uses_local_key(status: &SshStatus, local_fingerprint: Option<&str>) -> bool {
    status.managed
        && local_fingerprint.is_some()
        && status.managed_fingerprint.as_deref() == local_fingerprint
}

/// This computer's key fingerprint from its public half. Never creates a key.
fn read_local_fingerprint() -> Option<String> {
    let mut path = rc2fx_installer::ssh_key_path().ok()?.into_os_string();
    path.push(".pub");
    let text = std::fs::read_to_string(path).ok()?;
    let line: Vec<&str> = text.split_whitespace().take(2).collect();
    rc2fx_core::Ed25519PublicKey::parse(&line.join(" "))
        .ok()
        .map(|k| k.fingerprint())
}

/// Which action to offer: `Some(true)` Enable, `Some(false)` Disable.
pub(super) fn action(
    status: Option<&std::result::Result<Option<SshStatus>, String>>,
    pending: Option<bool>,
) -> Option<bool> {
    match (status, pending) {
        (Some(Ok(Some(s))), None) => Some(!s.managed),
        _ => None,
    }
}

impl Rc2FxApp {
    pub(super) fn developer_visible(&self) -> bool {
        self.developer_mode && self.session.connected && self.stream_connected
    }

    /// Ask the mixer for its SSH status, when the controls are visible.
    pub(super) fn read_ssh(&mut self) {
        if self.developer_visible() && self.ssh.pending.is_none() {
            let id = self.ssh.track(false);
            if !self.command(Command::Ssh {
                generation: self.generation,
                id,
            }) {
                self.ssh.sent.retain(|&(sent, _)| sent != id);
            }
        }
    }

    pub(super) fn set_developer_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        match connection::remember_developer(on) {
            Ok(()) => {
                self.developer_mode = on;
                self.ssh.clear();
                self.read_ssh();
            }
            // The switch stays as it was: the choice would not survive.
            Err(e) => self.message = format!("Could not save Developer mode: {e}"),
        }
        cx.notify();
    }

    pub(super) fn receive_ssh(
        &mut self,
        id: u64,
        result: std::result::Result<Option<SshStatus>, String>,
        cx: &mut Context<Self>,
    ) {
        // Stale replies are dropped before they change or pin anything.
        if !self.ssh.accept_reply(id) {
            return;
        }
        // Never carry one status's (or one mixer's) host identity into the next.
        self.ssh.pinned = None;
        if let (Ok(Some(status)), Some(serial)) = (&result, self.connected_mixer.clone()) {
            let keys = status.host_keys.clone();
            let epoch = self.ssh.epoch;
            cx.spawn(async move |this, cx| {
                let pin_serial = serial.clone();
                let (pinned, local) = cx
                    .background_executor()
                    .spawn(async move {
                        let pinned = (!keys.is_empty()).then(|| {
                            rc2fx_installer::pin_mixer_host_keys(&pin_serial, &keys)
                                .map_err(|e| e.to_string())
                        });
                        (pinned, read_local_fingerprint())
                    })
                    .await;
                let _ = this.update(cx, |app, cx| {
                    if app.ssh.epoch == epoch
                        && app.connected_mixer.as_deref() == Some(serial.as_str())
                    {
                        app.ssh.local_fingerprint = local;
                        app.ssh.pinned = pinned.map(|p| (serial, p));
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        self.ssh.status = Some(result);
    }

    fn send_ssh(&mut self, request: SshRequest) {
        let enable = request.enabled;
        let id = self.ssh.track(true);
        if self.command(Command::SetSsh {
            generation: self.generation,
            id,
            request,
        }) {
            self.ssh.pending = Some(enable);
        } else {
            self.ssh.sent.retain(|&(sent, _)| sent != id);
        }
    }

    /// Prepare this computer's key off the UI thread, then send it, but only
    /// for the connection the user confirmed on.
    fn same_connection(&self, epoch: u64, serial: &Option<String>) -> bool {
        self.ssh.epoch == epoch && &self.connected_mixer == serial && self.developer_visible()
    }

    fn enable_ssh(&mut self, epoch: u64, serial: Option<String>, cx: &mut Context<Self>) {
        if !self.same_connection(epoch, &serial) || self.ssh.pending.is_some() {
            self.message = "The mixer connection changed; SSH was not enabled.".into();
            return;
        }
        self.ssh.preparing_key = true;
        cx.spawn(async move |this, cx| {
            let key = cx
                .background_executor()
                .spawn(async { rc2fx_installer::ssh_public_key().map_err(|e| e.to_string()) })
                .await;
            let _ = this.update(cx, |app, cx| {
                if app.ssh.epoch == epoch {
                    app.ssh.preparing_key = false;
                }
                if !app.same_connection(epoch, &serial) {
                    app.message = "The mixer connection changed; SSH was not enabled.".into();
                } else {
                    match key {
                        Ok(key) => app.send_ssh(SshRequest {
                            enabled: true,
                            public_key: Some(key),
                        }),
                        Err(e) => app.message = format!("Could not prepare the SSH key: {e}"),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Enabling gives root access over the network, so it is confirmed. The
    /// confirmation only counts for the connection it was asked on.
    fn confirm_enable_ssh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let epoch = self.ssh.epoch;
        let serial = self.connected_mixer.clone();
        let app = cx.weak_entity();
        ui::confirm(
            window,
            cx,
            ui::Confirm {
                eyebrow: Some("Developer mode".into()),
                title: "Enable SSH access?".into(),
                detail:
                    "This adds this computer's RC2FX key to the mixer, so anyone with that key \
                         can log in as root over the network. Audio keeps running and nothing is \
                         flashed. You can disable it here at any time."
                        .into(),
                cancel: Some("Cancel"),
                action: "Enable SSH".into(),
            },
            move |_, cx| {
                let _ = app.update(cx, |app, cx| {
                    app.enable_ssh(epoch, serial.clone(), cx);
                    cx.notify();
                });
            },
            |_, _| {},
        );
    }

    pub(super) fn developer_section(&self, cx: &mut Context<Self>) -> Div {
        let mut rows = div().child(settings::row(
            settings::labelled(
                "Developer mode",
                "Shows developer controls, such as SSH access, for the connected mixer. \
                 Turning it off only hides them: it does not remove access.",
            ),
            Switch::new("developer-mode")
                .checked(self.developer_mode)
                .on_click(cx.listener(|app, on: &bool, _, cx| app.set_developer_mode(*on, cx))),
        ));
        if self.developer_mode {
            if !self.developer_visible() {
                rows = rows.child(settings::row(
                    settings::labelled("SSH access", "Connect the mixer over USB 1 to manage SSH."),
                    div(),
                ));
            } else {
                let pending = self.ssh.pending.or(self.ssh.preparing_key.then_some(true));
                let local = self.ssh.local_fingerprint.as_deref();
                let (line, caution) = describe(self.ssh.status.as_ref(), pending, local);
                let button = action(self.ssh.status.as_ref(), pending).map(|enable| {
                    if enable {
                        settings::ghost("enable-ssh", "Enable SSH").on_click(
                            cx.listener(|app, _, window, cx| app.confirm_enable_ssh(window, cx)),
                        )
                    } else {
                        settings::ghost("disable-ssh", "Disable SSH").on_click(cx.listener(
                            |app, _, _, cx| {
                                app.send_ssh(SshRequest {
                                    enabled: false,
                                    public_key: None,
                                });
                                cx.notify();
                            },
                        ))
                    }
                });
                let mut detail = div().v_flex().gap_1().child(line);
                if let Some(caution) = caution {
                    detail = detail.child(div().text_color(rgb(WARN)).child(caution));
                }
                let managed = match &self.ssh.status {
                    Some(Ok(Some(s))) if s.managed => Some(uses_local_key(s, local)),
                    _ => None,
                };
                if let Some(ours) = managed {
                    detail = detail.child(
                        "Disabling removes the RC2FX-managed key and stops new logins with it. \
                         Sessions already open stay open until they end.",
                    );
                    if !ours {
                        detail = detail.child(
                            "The login command is shown only when the managed key is this \
                             computer's. To use this computer's key, disable SSH and enable it again \
                             from here.",
                        );
                    }
                    let serial = self.connected_mixer.as_deref();
                    match (&self.ssh.pinned, rc2fx_installer::ssh_key_path()) {
                        (Some((pinned_for, Ok(pinned))), Ok(key))
                            if ours && Some(pinned_for.as_str()) == serial =>
                        {
                            detail =
                                detail.child(div().text_xs().font_family("monospace").child(
                                    login_command(&key, &pinned.known_hosts, &pinned.alias),
                                ));
                            detail = detail.child(div().text_color(rgb(FAINT)).child(
                                "Replace MIXER_IP with the address on the mixer's screen under \
                                 Settings › Network.",
                            ));
                        }
                        (Some((_, Err(e))), _) => {
                            detail = detail.child(div().text_color(rgb(WARN)).child(format!(
                                "Could not save the mixer's host identity, so no login command \
                                 is shown: {e}"
                            )));
                        }
                        _ => {}
                    }
                }
                let refresh = (pending.is_none()).then(|| {
                    settings::ghost("refresh-ssh", "Refresh").on_click(cx.listener(
                        |app, _, _, cx| {
                            app.ssh.status = None;
                            app.ssh.pinned = None;
                            app.read_ssh();
                            cx.notify();
                        },
                    ))
                });
                rows = rows.child(settings::row(
                    settings::labelled("SSH access", ""),
                    div().h_flex().gap_2().children(refresh).children(button),
                ));
                rows = rows.child(div().text_sm().text_color(rgb(MUTED)).child(detail));
            }
        }
        settings::section("Developer", div(), rows)
    }
}

/// Quote a path for the user's shell, so spaces and quotes survive pasting.
fn quote(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    if cfg!(windows) {
        format!("\"{}\"", text.replace('"', "\\\""))
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

/// The login command, with this mixer's pinned host identity. MIXER_IP is a
/// placeholder: the mixer does not report its network address.
pub(super) fn login_command(
    key: &std::path::Path,
    known_hosts: &std::path::Path,
    alias: &str,
) -> String {
    format!(
        "ssh -i {} -o UserKnownHostsFile={} -o HostKeyAlias={} root@MIXER_IP",
        quote(key),
        quote(known_hosts),
        alias
    )
}

#[cfg(test)]
mod tests {
    use super::{SshStatus, action, describe, uses_local_key};
    #[cfg(not(target_os = "windows"))]
    use super::login_command;

    fn status(managed: bool, unmanaged: u32, running: Option<bool>) -> SshStatus {
        SshStatus {
            managed,
            managed_fingerprint: managed.then(|| "SHA256:abc".into()),
            unmanaged_keys: unmanaged,
            host_keys: vec![],
            server_running: running,
            detail: None,
        }
    }

    #[test]
    fn old_mixer_software_says_update_and_is_never_shown_as_off() {
        let old = Some(Ok(None));
        let (line, _) = describe(old.as_ref(), None, None);
        assert!(line.contains("Update the RC2FX mixer software"));
        assert!(!line.contains("Off"));
        assert_eq!(action(old.as_ref(), None), None);
    }

    #[test]
    fn managed_and_unmanaged_access_are_both_reported() {
        let s = Some(Ok(Some(status(true, 2, Some(true)))));
        let (line, caution) = describe(s.as_ref(), None, Some("SHA256:abc"));
        assert!(line.contains("this computer's key") && line.contains("SHA256:abc"));
        assert!(caution.unwrap().contains("2 other keys"));
        assert_eq!(action(s.as_ref(), None), Some(false));
        let off = Some(Ok(Some(status(false, 1, None))));
        let (line, caution) = describe(off.as_ref(), None, None);
        assert!(line.starts_with("Off"));
        assert!(caution.unwrap().contains("1 other key can"));
        assert_eq!(action(off.as_ref(), None), Some(true));
    }

    #[test]
    fn unknown_or_pending_offers_no_action() {
        assert_eq!(describe(None, None, None).0, "Checking…");
        assert_eq!(action(None, None), None);
        let err = Some(Err("busy".to_owned()));
        assert!(describe(err.as_ref(), None, None).0.contains("busy"));
        assert_eq!(action(err.as_ref(), None), None);
        let s = Some(Ok(Some(status(false, 0, Some(true)))));
        assert_eq!(action(s.as_ref(), Some(true)), None);
        assert!(
            describe(s.as_ref(), Some(true), None)
                .0
                .starts_with("Enabling")
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn the_login_command_quotes_paths() {
        let cmd = login_command(
            std::path::Path::new("/home/a b/.ssh/rc2fx"),
            std::path::Path::new("/tmp/it's/known_hosts"),
            "rc2fx-0123456789ab",
        );
        assert_eq!(
            cmd,
            "ssh -i '/home/a b/.ssh/rc2fx' -o UserKnownHostsFile='/tmp/it'\\''s/known_hosts' \
             -o HostKeyAlias=rc2fx-0123456789ab root@MIXER_IP"
        );
    }

    #[test]
    fn a_read_answered_before_a_change_is_ignored() {
        let mut ssh = super::Ssh::default();
        let read = ssh.track(false);
        let change = ssh.track(true);
        ssh.pending = Some(true);
        assert!(!ssh.accept_reply(read), "stale read");
        assert_eq!(ssh.pending, Some(true));
        assert!(ssh.accept_reply(change), "the change's own reply");
        assert_eq!(ssh.pending, None);
        let read = ssh.track(false);
        assert!(ssh.accept_reply(read));
    }

    #[test]
    fn replies_from_before_a_clear_are_ignored() {
        let mut ssh = super::Ssh::default();
        let old = ssh.track(true);
        ssh.clear();
        let new = ssh.track(false);
        assert_ne!(old, new, "ids are never reused");
        assert!(!ssh.accept_reply(old));
        assert_eq!(ssh.sent.len(), 1, "a stale reply consumes nothing");
        assert!(ssh.accept_reply(new));
        assert!(!ssh.accept_reply(new), "each reply counts once");
    }

    #[test]
    fn another_computers_key_is_named_and_gets_no_login_command() {
        let s = status(true, 0, Some(true));
        let st = Some(Ok(Some(s.clone())));
        let (line, _) = describe(st.as_ref(), None, Some("SHA256:mine"));
        assert!(line.contains("another computer"));
        assert!(!uses_local_key(&s, Some("SHA256:mine")));
        assert!(!uses_local_key(&s, None));
        assert!(uses_local_key(&s, Some("SHA256:abc")));
        let (line, _) = describe(st.as_ref(), None, None);
        assert!(line.contains("RC2FX-managed key") && !line.contains("this computer"));
    }

    #[test]
    fn clearing_always_moves_the_epoch() {
        let mut ssh = super::Ssh::default();
        let before = ssh.epoch;
        ssh.pending = Some(true);
        ssh.track(true);
        ssh.clear();
        assert_ne!(ssh.epoch, before);
        assert!(ssh.pending.is_none() && ssh.sent.is_empty());
    }

    #[test]
    fn a_stopped_server_is_mentioned() {
        let s = Some(Ok(Some(status(true, 0, Some(false)))));
        assert!(describe(s.as_ref(), None, None).0.contains("not running"));
    }
}
