//! `RC2FX_DEV_PAGE`: open the app on a chosen page, for development and
//! screenshots. Unset, the app opens as usual.
//!
//! Values: `home`, `settings`, `diagnostics`, `installer`, or
//! `channel:<name or id>[:<effect>]`, where the effect is one of `highpass`,
//! `suppression`, `eq`, `compressor`, `gain` or `limiter`. Names match
//! case-insensitively, e.g. `channel:mic 1:suppression`.
//!
//! It only navigates: it never changes settings or sends anything to the
//! mixer beyond what opening that page normally does.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum DevPage {
    Home,
    Settings,
    Diagnostics,
    Installer,
    Channel {
        name: String,
        effect: Option<Effect>,
    },
}

impl DevPage {
    pub(super) fn from_env() -> Option<Self> {
        let value = std::env::var("RC2FX_DEV_PAGE").ok()?;
        match parse(&value) {
            Ok(page) => Some(page),
            Err(e) => {
                eprintln!("RC2FX_DEV_PAGE ignored: {e}");
                None
            }
        }
    }

    /// Channels need the mixer's state; the other pages can open at once.
    pub(super) fn needs_mixer(&self) -> bool {
        matches!(self, Self::Channel { .. } | Self::Home)
    }
}

pub(super) fn parse(value: &str) -> std::result::Result<DevPage, String> {
    let value = value.trim();
    let mut parts = value.splitn(3, ':');
    let page = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    let page = match page.as_str() {
        "home" => DevPage::Home,
        "settings" => DevPage::Settings,
        "diagnostics" => DevPage::Diagnostics,
        "installer" => DevPage::Installer,
        "channel" => {
            let name = parts.next().map(str::trim).unwrap_or_default();
            if name.is_empty() {
                return Err("`channel:` needs a channel name or id".into());
            }
            let effect = match parts.next().map(|e| e.trim().to_ascii_lowercase()) {
                None => None,
                Some(e) => Some(match e.as_str() {
                    "highpass" => Effect::Highpass,
                    "suppression" => Effect::Suppression,
                    "eq" => Effect::Eq,
                    "compressor" => Effect::Compressor,
                    "gain" => Effect::Gain,
                    "limiter" => Effect::Limiter,
                    other => return Err(format!("unknown effect `{other}`")),
                }),
            };
            return Ok(DevPage::Channel {
                name: name.to_owned(),
                effect,
            });
        }
        other => return Err(format!("unknown page `{other}`")),
    };
    if parts.next().is_some() {
        return Err(format!("`{value}` has extra parts"));
    }
    Ok(page)
}

/// The channel whose name or id matches, ignoring case.
pub(super) fn find_channel<'a>(
    names: impl IntoIterator<Item = (&'a str, &'a str)>,
    wanted: &str,
) -> Option<usize> {
    names
        .into_iter()
        .position(|(id, name)| id.eq_ignore_ascii_case(wanted) || name.eq_ignore_ascii_case(wanted))
}

impl Rc2FxApp {
    /// Open the requested page, once. Pages that need the mixer wait for
    /// its first state.
    pub(super) fn open_dev_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self.dev_page.as_ref() else {
            return;
        };
        if page.needs_mixer() && !self.session.connected {
            return;
        }
        let page = self.dev_page.take().expect("checked");
        match page {
            DevPage::Home => self.open_overview(cx),
            DevPage::Settings => self.set_settings_open(true),
            DevPage::Diagnostics => {
                self.set_settings_open(true);
                self.diagnostics_open = true;
            }
            DevPage::Installer => self.toggle_installer_page(cx),
            DevPage::Channel { name, effect } => {
                let found = find_channel(
                    self.session
                        .draft
                        .iter()
                        .map(|t| (t.id.as_str(), t.name.as_str())),
                    &name,
                );
                match found {
                    Some(index) => {
                        if let Some(effect) = effect {
                            self.effect = effect;
                        }
                        self.open_channel(index, window, cx);
                    }
                    None => eprintln!("RC2FX_DEV_PAGE: no channel named `{name}`"),
                }
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{DevPage, Effect, find_channel, parse};

    #[test]
    fn pages_and_channels_parse() {
        assert_eq!(parse("home"), Ok(DevPage::Home));
        assert_eq!(parse(" Settings "), Ok(DevPage::Settings));
        assert_eq!(parse("installer"), Ok(DevPage::Installer));
        assert_eq!(
            parse("channel:Mic 1:suppression"),
            Ok(DevPage::Channel {
                name: "Mic 1".into(),
                effect: Some(Effect::Suppression)
            })
        );
        assert_eq!(
            parse("channel:headphones-1"),
            Ok(DevPage::Channel {
                name: "headphones-1".into(),
                effect: None
            })
        );
    }

    #[test]
    fn bad_values_are_refused() {
        for bad in [
            "",
            "nope",
            "channel",
            "channel:",
            "channel:mic:reverb",
            "home:x",
        ] {
            assert!(parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn channels_match_by_name_or_id_ignoring_case() {
        let channels = [("mic-1", "Mic 1"), ("hp-1", "Headphones 1")];
        assert_eq!(find_channel(channels, "mic 1"), Some(0));
        assert_eq!(find_channel(channels, "HP-1"), Some(1));
        assert_eq!(find_channel(channels, "Mic 2"), None);
    }
}
