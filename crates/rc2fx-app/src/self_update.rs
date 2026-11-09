//! Updating RC2FX in place, through Velopack, when Velopack installed it:
//! the Windows Setup.exe, the macOS Setup.pkg, their portable zips, or the
//! Linux AppImage. Other copies (development builds, builds from source)
//! only link to the release page.
//!
//! Nothing happens on its own. The update check says a version exists; the
//! user downloads it, then chooses "Restart to update". That restart is held
//! back while the installer has a run going or the mixer may still be
//! updating, and it still asks about unsaved edits like any other quit. The
//! Velopack updater starts as RC2FX quits and waits for it to exit.
//!
//! Updates come from the latest GitHub release: its `releases.{channel}.json`
//! names the package and its SHA-256, which Velopack checks after download.
//! That feed is not signed, so a package is also checked against the signed
//! app release metadata (`rc2fx_installer::app_release`) as it downloads,
//! before Velopack moves it into place, and again when one is found at
//! startup. Only full packages are taken from the feed.
use super::*;
use rc2fx_installer::app_release::VerifyError;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use velopack::locator::{LocationContext, VelopackLocatorConfig, auto_locate_app_manifest};
use velopack::sources::{HttpSource, UpdateSource};
use velopack::{UpdateCheck, UpdateManager, VelopackAsset, VelopackAssetFeed};

/// Assets of the release GitHub marks latest; only app releases are.
const FEED: &str = "https://github.com/ibanks42/rc2fx/releases/latest/download";

/// Sent on the download's progress channel once the bytes are in and the
/// signed check starts. Velopack sends only 0 to 100 there.
const VERIFYING: i16 = i16::MIN;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum State {
    /// Waiting for the user to ask.
    Idle,
    /// Percent downloaded.
    Downloading(u8),
    /// Downloaded; being checked against its signed release.
    Verifying(String),
    /// Downloaded and checked; applied when RC2FX restarts.
    Ready(String),
    /// The last attempt failed; the user may try again.
    Failed(String),
}

pub(crate) struct SelfUpdate {
    /// `None` when Velopack did not install this copy.
    manager: Option<UpdateManager>,
    /// Where Velopack keeps downloaded packages.
    packages: Option<PathBuf>,
    /// `RC2FX_DEV_UPDATE` set a state to show; nothing can be downloaded.
    preview: bool,
    state: State,
    /// A package found at startup, until it is checked.
    pending: Option<VelopackAsset>,
    /// A kept package whose check could not be made; trying again checks
    /// it without downloading.
    retry: Option<VelopackAsset>,
    /// What the feed's check of the last download found.
    checks: Checks,
    /// The downloaded package to apply as RC2FX quits.
    apply_on_quit: Option<VelopackAsset>,
    ready: Option<VelopackAsset>,
}

impl SelfUpdate {
    pub(crate) fn new() -> Self {
        if let Some(preview) = preview() {
            return preview;
        }
        let checks = Checks::default();
        let (manager, packages) = velopack_manager(&checks).unzip();
        // A package downloaded earlier and not yet applied is checked again
        // before it may be applied.
        let pending = manager
            .as_ref()
            .and_then(UpdateManager::get_update_pending_restart);
        let state = match &pending {
            Some(asset) => State::Verifying(asset.Version.clone()),
            None => State::Idle,
        };
        Self {
            packages,
            state,
            pending,
            checks,
            ..Self::empty(manager)
        }
    }

    fn empty(manager: Option<UpdateManager>) -> Self {
        Self {
            manager,
            packages: None,
            preview: false,
            state: State::Idle,
            pending: None,
            retry: None,
            checks: Checks::default(),
            apply_on_quit: None,
            ready: None,
        }
    }

    /// Whether this copy can update itself.
    pub(crate) fn supported(&self) -> bool {
        self.manager.is_some() || self.preview
    }

    /// Showing a `RC2FX_DEV_UPDATE` state, which needs no offered update.
    pub(crate) fn previewing(&self) -> bool {
        self.preview
    }

    /// A download has started or finished, so its button stays even when
    /// today's check found nothing.
    pub(crate) fn in_progress(&self) -> bool {
        matches!(
            self.state,
            State::Downloading(_) | State::Verifying(_) | State::Ready(_)
        )
    }

    /// Its button stays even when today's check found nothing: a download
    /// is under way or done, or a check failed, which the user must see.
    pub(crate) fn stays_shown(&self) -> bool {
        self.in_progress() || matches!(self.state, State::Failed(_))
    }

    /// A failed check kept its package, so trying again only checks it.
    pub(crate) fn retrying(&self) -> bool {
        self.retry.is_some()
    }

    pub(crate) fn state(&self) -> &State {
        &self.state
    }

    /// Whether the package `asset` is still in the packages folder.
    fn on_disk(&self, asset: &VelopackAsset) -> bool {
        let name = package_name(&asset.FileName);
        match (&self.packages, name) {
            (Some(packages), Some(name)) => packages.join(name).is_file(),
            _ => false,
        }
    }

    /// Start the updater for a confirmed restart. Called as RC2FX quits; the
    /// updater waits for this process to exit.
    pub(crate) fn launch_on_quit(&mut self) {
        let (Some(manager), Some(asset)) = (&self.manager, self.apply_on_quit.take()) else {
            return;
        };
        if let Err(e) =
            manager.wait_exit_then_apply_updates(&asset, false, true, Vec::<String>::new())
        {
            eprintln!("RC2FX could not start the updater: {e}");
        }
    }

    /// A restart was asked for but the quit was then cancelled.
    pub(crate) fn cancel_restart(&mut self) {
        self.apply_on_quit = None;
    }
}

/// `RC2FX_DEV_UPDATE`, in development builds only: show the update actions
/// in a given state, for screenshots: `idle`, `downloading`, `verifying`,
/// `ready` or `failed`.
fn preview() -> Option<SelfUpdate> {
    if !cfg!(debug_assertions) {
        return None;
    }
    let state = match std::env::var("RC2FX_DEV_UPDATE").ok()?.as_str() {
        "idle" => State::Idle,
        "downloading" => State::Downloading(42),
        "verifying" => State::Verifying("9.9.9".into()),
        "ready" => State::Ready("9.9.9".into()),
        "failed" => State::Failed(
            "The update could not be downloaded. Check the internet connection and try again."
                .into(),
        ),
        other => {
            eprintln!("RC2FX_DEV_UPDATE ignored: {other}");
            return None;
        }
    };
    Some(SelfUpdate {
        preview: true,
        state,
        ..SelfUpdate::empty(None)
    })
}

/// What the user sees for a failed download.
fn failure(error: &velopack::Error) -> String {
    match error {
        velopack::Error::Network(_) => {
            "The update could not be downloaded. Check the internet connection and try again."
                .into()
        }
        velopack::Error::ChecksumInvalid(..) | velopack::Error::SizeInvalid(..) => {
            "The downloaded update did not match the release and was discarded. Try again.".into()
        }
        other => format!("The update could not be downloaded ({other})."),
    }
}

/// The package the feed last checked as it downloaded, shared with it.
#[derive(Clone, Default)]
struct Checks(Arc<Mutex<Option<Checked>>>);

#[derive(Debug)]
struct Checked {
    version: String,
    file_name: String,
    result: std::result::Result<(), VerifyError>,
}

impl Checks {
    fn record(&self, asset: &VelopackAsset, result: &std::result::Result<(), VerifyError>) {
        if let Ok(mut last) = self.0.lock() {
            *last = Some(Checked {
                version: asset.Version.clone(),
                file_name: asset.FileName.clone(),
                result: result.clone(),
            });
        }
    }

    /// How the feed's check of `asset` went, if it made one, forgetting it.
    fn take(&self, asset: &VelopackAsset) -> Option<std::result::Result<(), VerifyError>> {
        let checked = self.0.lock().ok()?.take()?;
        (checked.version == asset.Version && checked.file_name == asset.FileName)
            .then_some(checked.result)
    }
}

/// The release feed, checking each package against its signed release as
/// it downloads, before Velopack renames it into place. A later check is too
/// late on Windows: right after that rename Velopack replaces its own
/// updater with the one in the package. Delta packages are left out of the
/// feed, so only whole, signed release assets are ever fetched.
struct SignedFeed {
    http: HttpSource,
    checks: Checks,
    verify: Verify,
}

impl UpdateSource for SignedFeed {
    fn get_release_feed(
        &self,
        channel: &str,
        app: &velopack::bundle::Manifest,
        staged_user_id: &str,
    ) -> std::result::Result<VelopackAssetFeed, velopack::Error> {
        let feed = self.http.get_release_feed(channel, app, staged_user_id)?;
        Ok(full_packages(feed))
    }

    fn download_release_entry(
        &self,
        asset: &VelopackAsset,
        local_file: &Path,
        progress: Option<mpsc::Sender<i16>>,
    ) -> std::result::Result<(), velopack::Error> {
        self.http
            .download_release_entry(asset, local_file, progress.clone())?;
        if let Some(progress) = &progress {
            let _ = progress.send(VERIFYING);
        }
        let work = installer::work_directory();
        let result = verify_file(work.as_deref(), asset, local_file, self.verify);
        self.checks.record(asset, &result);
        if let (Err(VerifyError::Unavailable(_)), Some(name)) =
            (&result, package_name(&asset.FileName))
        {
            // Kept under its final name to check again without a download.
            // The error below still stops Velopack, so on Windows it never
            // takes its updater from an unchecked package; a package already
            // in place is only checked, never unpacked, by the next attempt.
            let _ = fs::rename(local_file, local_file.with_file_name(name));
        }
        result.map_err(|e| velopack::Error::Other(e.to_string()))
    }
}

/// The feed without delta packages and without other platforms' packages:
/// with no deltas Velopack always fetches the whole package, which is what
/// the release signs, and every platform's package is signed alike, so only
/// its name ties a package to this platform.
fn full_packages(mut feed: VelopackAssetFeed) -> VelopackAssetFeed {
    feed.Assets.retain(|asset| {
        asset.Type.eq_ignore_ascii_case("Full") && is_own_package(&asset.Version, &asset.FileName)
    });
    feed
}

/// Velopack's package id for RC2FX, as `vpk pack --packId` sets it.
const PACK_ID: &str = "io.github.ibanks42.rc2fx";

/// Whether `file_name` is exactly this platform's full package of `version`,
/// as `vpk` names it (the default Windows channel's name has no channel).
fn is_own_package(version: &str, file_name: &str) -> bool {
    let channel = if cfg!(windows) {
        ""
    } else if cfg!(target_os = "macos") {
        "-osx"
    } else {
        "-linux"
    };
    package_name(file_name) == Some(&format!("{PACK_ID}-{version}{channel}-full.nupkg"))
}

fn signed_feed(checks: &Checks) -> SignedFeed {
    SignedFeed {
        http: HttpSource::new(FEED),
        checks: checks.clone(),
        verify: rc2fx_installer::app_release::verify_update_package,
    }
}

/// Velopack's paths for this AppImage, with downloaded packages kept in a
/// folder only this user can open instead of the shared `/var/tmp`, where
/// another account could swap a package between its check and its use.
/// `None` when Velopack did not package this copy. `main` gives the same
/// paths to Velopack's startup hooks, so both see the same packages.
#[cfg(target_os = "linux")]
pub fn velopack_locator() -> Option<VelopackLocatorConfig> {
    let found = auto_locate_app_manifest(LocationContext::FromCurrentExe).ok()?;
    let bin = found.get_current_bin_dir();
    Some(VelopackLocatorConfig {
        RootAppDir: found.get_appimage_path(),
        UpdateExePath: found.get_update_path(),
        PackagesDir: private_packages_dir()?,
        ManifestPath: bin.join("sq.version"),
        CurrentBinaryDir: bin,
        IsPortable: found.get_is_portable(),
    })
}

/// Elsewhere Velopack's own choice is already private to the user.
#[cfg(not(target_os = "linux"))]
pub fn velopack_locator() -> Option<VelopackLocatorConfig> {
    None
}

/// `$XDG_CACHE_HOME/rc2fx/velopack-packages`, or under `~/.cache`.
#[cfg(target_os = "linux")]
fn private_packages_dir() -> Option<PathBuf> {
    let absolute = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let cache = absolute("XDG_CACHE_HOME").or_else(|| Some(absolute("HOME")?.join(".cache")))?;
    Some(cache.join("rc2fx").join("velopack-packages"))
}

/// Create `dir` with mode 0700, or check an existing one is a real folder
/// this user owns, and close it to others.
#[cfg(target_os = "linux")]
fn ensure_private(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    // `/proc/self` belongs to this process's user.
    let me = fs::metadata("/proc/self").map(|m| m.uid()).ok();
    if !meta.is_dir() || me.is_some_and(|uid| uid != meta.uid()) {
        return Err(std::io::Error::other(format!(
            "{} is not a folder of this user",
            dir.display()
        )));
    }
    if meta.mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn velopack_manager(checks: &Checks) -> Option<(UpdateManager, PathBuf)> {
    let config = velopack_locator()?;
    let packages = config.PackagesDir.clone();
    if let Err(e) = ensure_private(&packages) {
        eprintln!("RC2FX cannot keep updates in {}: {e}", packages.display());
        return None;
    }
    let manager = UpdateManager::new(signed_feed(checks), None, Some(config)).ok()?;
    Some((manager, packages))
}

#[cfg(not(target_os = "linux"))]
fn velopack_manager(checks: &Checks) -> Option<(UpdateManager, PathBuf)> {
    // The same search UpdateManager makes, to learn where packages go.
    let packages = auto_locate_app_manifest(LocationContext::FromCurrentExe)
        .ok()?
        .get_packages_dir();
    let manager = UpdateManager::new(signed_feed(checks), None, None).ok()?;
    Some((manager, packages))
}

/// The file name Velopack stores `file_name` under in the packages folder,
/// which is also the release asset's name. `None` when Velopack would have
/// to change it, so the two could differ.
fn package_name(file_name: &str) -> Option<&str> {
    let separator = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    let name = file_name.rsplit(separator).next()?;
    let changed = |c: char| {
        if cfg!(windows) {
            matches!(c, '"' | '<' | '>' | '|' | ':' | '*' | '?') || (c as u32) < 32
        } else {
            c == '\0'
        }
    };
    (!name.is_empty() && !name.chars().any(changed)).then_some(name)
}

/// The signature of `rc2fx_installer::app_release::verify_update_package`:
/// work folder, version, asset name, file.
type Verify = fn(&Path, &str, &str, &Path) -> std::result::Result<(), VerifyError>;

/// Check `file`, the download of `asset`, against its signed release before
/// it may be applied. A package the release rejects is removed, so nothing
/// can apply it unchecked later. One that could not be checked (offline, say)
/// is kept: it may be good, and checking again needs no download.
fn verify_file(
    work: Option<&Path>,
    asset: &VelopackAsset,
    file: &Path,
    verify: Verify,
) -> std::result::Result<(), VerifyError> {
    let own = is_own_package(&asset.Version, &asset.FileName);
    let result = match (work, package_name(&asset.FileName).filter(|_| own)) {
        (_, None) => Err(VerifyError::Rejected(format!(
            "{} is not an update package for this computer.",
            asset.FileName
        ))),
        (None, _) => Err(VerifyError::Unavailable(
            "RC2FX could not find its working folder to check this update.".into(),
        )),
        (Some(work), Some(name)) => verify(work, &asset.Version, name, file),
    };
    if let Err(VerifyError::Rejected(_)) = result {
        let _ = fs::remove_file(file);
    }
    result
}

/// [`verify_file`] for a package in the packages folder.
fn verify_package(
    work: Option<&Path>,
    packages: &Path,
    asset: &VelopackAsset,
    verify: Verify,
) -> std::result::Result<(), VerifyError> {
    match package_name(&asset.FileName) {
        Some(name) => verify_file(work, asset, &packages.join(name), verify),
        None => Err(VerifyError::Rejected(format!(
            "{} is not an app update package.",
            asset.FileName
        ))),
    }
}

/// What a finished check leaves.
#[derive(Debug)]
struct Outcome {
    state: State,
    /// The package to apply on restart, only when it passed.
    ready: Option<VelopackAsset>,
    /// The package kept to check again, when no check could be made.
    retry: Option<VelopackAsset>,
}

fn checked(asset: VelopackAsset, result: std::result::Result<(), VerifyError>) -> Outcome {
    match result {
        Ok(()) => Outcome {
            state: State::Ready(asset.Version.clone()),
            ready: Some(asset),
            retry: None,
        },
        Err(VerifyError::Unavailable(message)) => Outcome {
            state: State::Failed(message),
            ready: None,
            retry: Some(asset),
        },
        Err(VerifyError::Rejected(message)) => Outcome {
            state: State::Failed(message),
            ready: None,
            retry: None,
        },
    }
}

impl Rc2FxApp {
    /// At startup: check a package an earlier run downloaded.
    pub(super) fn start_self_update(&mut self, cx: &mut Context<Self>) {
        if let Some(asset) = self.self_update.pending.take() {
            self.check_update_package(asset, cx);
        }
    }
    /// Check a downloaded package in the background; it becomes ready only
    /// if it is the signed release.
    fn check_update_package(&mut self, asset: VelopackAsset, cx: &mut Context<Self>) {
        let Some(packages) = self.self_update.packages.clone() else {
            return;
        };
        self.self_update.ready = None;
        self.self_update.retry = None;
        self.self_update.state = State::Verifying(asset.Version.clone());
        cx.notify();
        let work = installer::work_directory();
        let check = cx.background_executor().spawn(async move {
            let result = verify_package(
                work.as_deref(),
                &packages,
                &asset,
                rc2fx_installer::app_release::verify_update_package,
            );
            (asset, result)
        });
        cx.spawn(async move |this, cx| {
            let (asset, result) = check.await;
            let _ = this.update(cx, |app, cx| {
                app.finish_check(checked(asset, result));
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_check(&mut self, outcome: Outcome) {
        self.self_update.state = outcome.state;
        self.self_update.ready = outcome.ready;
        self.self_update.retry = outcome.retry;
    }

    /// Download the newest release in the background, reporting progress;
    /// the feed checks it as it arrives. A kept package that could not be
    /// checked is only checked again.
    pub(super) fn download_app_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(manager) = self.self_update.manager.clone() else {
            return;
        };
        if self.self_update.in_progress() {
            return;
        }
        if let Some(asset) = self.self_update.retry.take()
            && self.self_update.on_disk(&asset)
        {
            self.check_update_package(asset, cx);
            return;
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(packages) = &self.self_update.packages
                && let Err(e) = ensure_private(packages)
            {
                self.self_update.state =
                    State::Failed(format!("The update could not be downloaded ({e})."));
                cx.notify();
                return;
            }
        }
        self.self_update.state = State::Downloading(0);
        cx.notify();
        let (sender, progress) = mpsc::channel::<i16>();
        let checks = self.self_update.checks.clone();
        // The version being fetched, for the state shown while it is checked.
        let fetching = Arc::new(Mutex::new(None::<String>));
        let version = fetching.clone();
        let download = cx.background_executor().spawn(async move {
            let update = match manager.check_for_updates() {
                Ok(UpdateCheck::UpdateAvailable(update)) => update,
                Ok(_) => {
                    return Err((
                        "This update is not ready to download yet. Try again later.".to_owned(),
                        None,
                    ));
                }
                Err(e) => return Err((failure(&e), None)),
            };
            let asset = update.TargetFullRelease.clone();
            if let Ok(mut version) = version.lock() {
                *version = Some(asset.Version.clone());
            }
            checks.take(&asset);
            let downloaded = manager.download_updates(&update, Some(sender));
            let checked = checks.take(&asset);
            match (downloaded, checked) {
                (Ok(()), Some(Ok(()))) => Ok((asset, true)),
                // Already on disk from an earlier download, so the feed
                // fetched nothing; it is checked next.
                (Ok(()), _) => Ok((asset, false)),
                (Err(_), Some(Err(VerifyError::Unavailable(message)))) => {
                    Err((message, Some(asset)))
                }
                (Err(_), Some(Err(rejected))) => Err((rejected.to_string(), None)),
                (Err(e), _) => Err((failure(&e), None)),
            }
        });
        // Progress until the download ends and drops its sender.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                let mut latest = None;
                let ended = loop {
                    match progress.try_recv() {
                        Ok(percent) => latest = Some(percent),
                        Err(mpsc::TryRecvError::Empty) => break false,
                        Err(mpsc::TryRecvError::Disconnected) => break true,
                    }
                };
                let verifying = (latest == Some(VERIFYING))
                    .then(|| fetching.lock().ok()?.clone())
                    .flatten();
                if let Some(percent) = latest {
                    let _ = this.update(cx, |app, cx| {
                        let state = &mut app.self_update.state;
                        if !matches!(state, State::Downloading(_)) {
                            return;
                        }
                        *state = match &verifying {
                            Some(version) => State::Verifying(version.clone()),
                            None => State::Downloading(percent.clamp(0, 100) as u8),
                        };
                        cx.notify();
                    });
                }
                if ended {
                    return;
                }
            }
        })
        .detach();
        cx.spawn_in(window, async move |this, cx| {
            let result = download.await;
            let _ = this.update(cx, |app, cx| {
                match result {
                    Ok((asset, true)) => app.finish_check(checked(asset, Ok(()))),
                    Ok((asset, false)) => app.check_update_package(asset, cx),
                    Err((message, retry)) => {
                        app.self_update.state = State::Failed(message);
                        app.self_update.retry = retry;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Why RC2FX should not restart for an update now, if it should not.
    pub(super) fn update_restart_blocker(&self) -> Option<&'static str> {
        self.installer_restart_blocker()
    }

    /// Quit and apply the downloaded update, once nothing holds it back.
    /// Unsaved edits are asked about as for any quit.
    pub(super) fn restart_to_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.update_restart_blocker().is_some() {
            return;
        }
        let Some(asset) = self.self_update.ready.clone() else {
            return;
        };
        // Cleaned up or removed since it was checked: nothing to apply.
        if !self.self_update.on_disk(&asset) {
            self.self_update.ready = None;
            self.self_update.state = State::Failed(
                "The downloaded update is no longer on this computer. Download it again.".into(),
            );
            cx.notify();
            return;
        }
        self.self_update.apply_on_quit = Some(asset);
        self.quit_for_update(window, cx);
    }

    fn quit_for_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_close(window, cx) {
            cx.quit();
        } else if !self.closing_prompt {
            // Refused outright (a file dialog is open): no later quit may
            // restart unasked. An open prompt clears it on cancel.
            self.self_update.cancel_restart();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(file: &str) -> VelopackAsset {
        VelopackAsset {
            Version: "1.2.3".into(),
            FileName: file.into(),
            ..Default::default()
        }
    }

    /// This platform's full package of `version`, as vpk names it.
    fn own(version: &str) -> String {
        let channel = if cfg!(windows) {
            ""
        } else if cfg!(target_os = "macos") {
            "-osx"
        } else {
            "-linux"
        };
        format!("{PACK_ID}-{version}{channel}-full.nupkg")
    }

    #[test]
    fn only_this_platforms_package_of_that_version_is_accepted() {
        assert!(is_own_package("1.2.3", &own("1.2.3")));
        assert!(!is_own_package("1.2.4", &own("1.2.3")));
        for other in [
            format!("{PACK_ID}-1.2.3-full.nupkg"),
            format!("{PACK_ID}-1.2.3-osx-full.nupkg"),
            format!("{PACK_ID}-1.2.3-linux-full.nupkg"),
        ] {
            if other != own("1.2.3") {
                assert!(!is_own_package("1.2.3", &other), "{other}");
            }
        }
        let feed = VelopackAssetFeed {
            Assets: vec![
                VelopackAsset {
                    Type: "Full".into(),
                    ..asset(&own("1.2.3"))
                },
                VelopackAsset {
                    Type: "Full".into(),
                    ..asset(&format!("{PACK_ID}-1.2.3-elsewhere-full.nupkg"))
                },
                VelopackAsset {
                    Type: "Delta".into(),
                    ..asset(&own("1.2.3"))
                },
            ],
        };
        let kept = full_packages(feed).Assets;
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].FileName, own("1.2.3"));
        // A signed package of another platform is refused before any check.
        let file = std::env::temp_dir().join(format!("rc2fx-other-{}", std::process::id()));
        fs::write(&file, b"x").unwrap();
        let other = asset(&format!("{PACK_ID}-1.2.3-elsewhere-full.nupkg"));
        let result = verify_file(Some(Path::new("/")), &other, &file, |_, _, _, _| Ok(()));
        assert!(matches!(result, Err(VerifyError::Rejected(_))));
        assert!(!file.exists());
    }

    #[test]
    fn a_development_build_cannot_update_itself() {
        let update = SelfUpdate::new();
        assert!(!update.supported());
        assert_eq!(update.state(), &State::Idle);
    }

    #[test]
    fn download_failures_say_what_to_do() {
        let checksum = velopack::Error::ChecksumInvalid("x".into(), "a".into(), "b".into());
        assert!(failure(&checksum).contains("did not match"));
        let other = velopack::Error::Other("disk full".into());
        assert_eq!(
            failure(&other),
            "The update could not be downloaded (disk full)."
        );
    }

    #[test]
    fn a_package_its_release_rejects_is_removed_and_shown() {
        let work = tempfile::tempdir().unwrap();
        let packages = tempfile::tempdir().unwrap();
        let file = packages.path().join(own("1.2.3"));
        fs::write(&file, "package").unwrap();
        let refuse: Verify = |_, _, _, _| {
            Err(VerifyError::Rejected(
                "This update has not been signed yet.".into(),
            ))
        };
        let package = asset(&own("1.2.3"));
        let result = verify_package(Some(work.path()), packages.path(), &package, refuse);
        assert!(!file.exists());
        let outcome = checked(package, result);
        assert_eq!(
            outcome.state,
            State::Failed("This update has not been signed yet.".into())
        );
        assert!(outcome.ready.is_none() && outcome.retry.is_none());
    }

    #[test]
    fn a_package_that_could_not_be_checked_is_kept_to_check_again() {
        let work = tempfile::tempdir().unwrap();
        let packages = tempfile::tempdir().unwrap();
        let file = packages.path().join(own("1.2.3"));
        fs::write(&file, "package").unwrap();
        let offline: Verify = |_, _, _, _| {
            Err(VerifyError::Unavailable(
                "Could not check this update.".into(),
            ))
        };
        let package = asset(&own("1.2.3"));
        let result = verify_package(Some(work.path()), packages.path(), &package, offline);
        assert!(file.exists());
        let outcome = checked(package, result);
        assert_eq!(
            outcome.state,
            State::Failed("Could not check this update.".into())
        );
        assert!(outcome.ready.is_none());
        let mut update = SelfUpdate {
            packages: Some(packages.path().to_owned()),
            state: outcome.state,
            retry: outcome.retry,
            ..SelfUpdate::empty(None)
        };
        // Shown even when today's check offers nothing, with a retry.
        assert!(update.stays_shown() && !update.in_progress());
        assert!(update.retrying());
        assert!(update.on_disk(update.retry.as_ref().unwrap()));
        update.state = State::Idle;
        assert!(!update.stays_shown());
    }

    #[test]
    fn a_package_that_passes_is_ready() {
        let work = tempfile::tempdir().unwrap();
        let packages = tempfile::tempdir().unwrap();
        let file = packages.path().join(own("1.2.3"));
        fs::write(&file, "package").unwrap();
        let accept: Verify = |work, version, name, file| {
            assert!(work.is_dir());
            assert_eq!((version, name), ("1.2.3", own("1.2.3").as_str()));
            assert!(file.is_file());
            Ok(())
        };
        // A feed may name a package in a sub-folder; Velopack stores and
        // the release names only its file name.
        let package = asset(&format!("sub/{}", own("1.2.3")));
        let result = verify_package(Some(work.path()), packages.path(), &package, accept);
        assert!(file.exists());
        let outcome = checked(package, result);
        assert_eq!(outcome.state, State::Ready("1.2.3".into()));
        assert_eq!(outcome.ready.unwrap().Version, "1.2.3");
    }

    #[test]
    fn a_package_without_a_work_folder_is_not_trusted() {
        let packages = tempfile::tempdir().unwrap();
        let file = packages.path().join(own("1.2.3"));
        fs::write(&file, "package").unwrap();
        let never: Verify = |_, _, _, _| panic!("nothing to check against");
        let result = verify_package(None, packages.path(), &asset(&own("1.2.3")), never);
        // Nothing is wrong with the package itself.
        assert!(matches!(result, Err(VerifyError::Unavailable(_))));
        assert!(file.exists());
        assert!(matches!(
            verify_package(
                Some(packages.path()),
                packages.path(),
                &asset("dir/"),
                never
            ),
            Err(VerifyError::Rejected(_))
        ));
    }

    #[test]
    fn a_download_is_checked_under_its_temporary_name_and_discarded_if_rejected() {
        let work = tempfile::tempdir().unwrap();
        let packages = tempfile::tempdir().unwrap();
        // Velopack downloads beside the final name and renames only after
        // the source returns.
        let partial = packages
            .path()
            .join(own("1.2.3").replace(".nupkg", ".partial"));
        fs::write(&partial, "package").unwrap();
        let package = asset(&own("1.2.3"));
        let seen: Verify = |_, version, name, file| {
            assert_eq!((version, name), ("1.2.3", own("1.2.3").as_str()));
            assert!(file.ends_with(own("1.2.3").replace(".nupkg", ".partial")));
            Err(VerifyError::Rejected("mismatch".into()))
        };
        let checks = Checks::default();
        let result = verify_file(Some(work.path()), &package, &partial, seen);
        checks.record(&package, &result);
        assert!(!partial.exists());
        assert!(checks.take(&asset(&own("1.2.4"))).is_none());
        checks.record(&package, &result);
        assert_eq!(
            checks.take(&package),
            Some(Err(VerifyError::Rejected("mismatch".into())))
        );
        assert!(checks.take(&package).is_none(), "a check is used once");
    }

    #[test]
    fn only_full_packages_are_taken_from_the_feed() {
        let typed = |kind: &str| VelopackAsset {
            Type: kind.into(),
            ..asset(&own("1.2.3"))
        };
        let feed = full_packages(VelopackAssetFeed {
            Assets: vec![typed("Delta"), typed("full"), typed("Full"), typed("")],
        });
        assert_eq!(feed.Assets.len(), 2);
        assert!(
            feed.Assets
                .iter()
                .all(|a| a.Type.eq_ignore_ascii_case("Full"))
        );
    }

    #[test]
    fn a_missing_package_is_not_offered_to_restart_into() {
        let packages = tempfile::tempdir().unwrap();
        let update = SelfUpdate {
            packages: Some(packages.path().to_owned()),
            ..SelfUpdate::empty(None)
        };
        let package = asset(&own("1.2.3"));
        assert!(!update.on_disk(&package));
        fs::write(packages.path().join(own("1.2.3")), "package").unwrap();
        assert!(update.on_disk(&package));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_packages_folder_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let packages = dir.path().join("rc2fx/velopack-packages");
        ensure_private(&packages).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&packages), 0o700);
        fs::set_permissions(&packages, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private(&packages).unwrap();
        assert_eq!(mode(&packages), 0o700);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&packages, &link).unwrap();
        assert!(ensure_private(&link).is_err());
    }
}
