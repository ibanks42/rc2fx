use super::*;
use aws_lc_rs::{rand::SystemRandom, signature::Ed25519KeyPair};
use std::{collections::HashMap, fs, num::NonZeroU64, path::PathBuf};
use tough::{
    editor::{RepositoryEditor, signed::SignedRole},
    key_source::{KeySource, LocalKeySource},
    schema::{KeyHolder, RoleKeys, RoleType, Root},
};

const PACKAGE: &str = "io.github.ibanks42.rc2fx-1.2.3-full.nupkg";

/// A test-only root and key, with metadata written under `dir/metadata`
/// and the client's temporary state under `dir/work`.
struct Fixture {
    dir: tempfile::TempDir,
    root: Vec<u8>,
    key: PathBuf,
}

async fn key(path: PathBuf) -> (LocalKeySource, tough::schema::key::Key) {
    fs::write(
        &path,
        Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .unwrap()
            .as_ref(),
    )
    .unwrap();
    let source = LocalKeySource { path };
    let public = source.as_sign().await.unwrap().tuf_key();
    (source, public)
}

impl Fixture {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-only-key.pk8");
        let (source, public) = key(path.clone()).await;
        let id = public.key_id().unwrap();
        let roles = [
            RoleType::Root,
            RoleType::Targets,
            RoleType::Snapshot,
            RoleType::Timestamp,
        ]
        .into_iter()
        .map(|role| {
            let keys = RoleKeys {
                keyids: vec![id.clone()],
                threshold: NonZeroU64::new(1).unwrap(),
                _extra: HashMap::new(),
            };
            (role, keys)
        })
        .collect();
        // Shaped like the production root: fixed names under release tags.
        let root = Root {
            spec_version: "1.0.1".into(),
            consistent_snapshot: false,
            version: NonZeroU64::new(1).unwrap(),
            expires: "2099-01-01T00:00:00Z".parse().unwrap(),
            keys: HashMap::from([(id, public)]),
            roles,
            _extra: HashMap::new(),
        };
        let signed = SignedRole::new(
            root.clone(),
            &KeyHolder::Root(root),
            &[Box::new(source)],
            &SystemRandom::new(),
        )
        .await
        .unwrap();
        let root = signed.buffer().clone();
        fs::write(dir.path().join("root.json"), &root).unwrap();
        fs::write(dir.path().join(PACKAGE), b"signed update package").unwrap();
        Self {
            dir,
            root,
            key: path,
        }
    }

    /// Sign the package as target `name` with `version` in its custom field,
    /// at the given targets, snapshot and timestamp versions.
    async fn sign_with(
        &self,
        name: &str,
        version: &str,
        [targets, snapshot, timestamp]: [u64; 3],
        expires: &str,
    ) {
        let (_, mut target) = RepositoryEditor::build_target(self.dir.path().join(PACKAGE))
            .await
            .unwrap();
        target.custom.insert("version".into(), version.into());
        let expires: jiff::Timestamp = expires.parse().unwrap();
        let mut editor = RepositoryEditor::new(self.dir.path().join("root.json"))
            .await
            .unwrap();
        editor
            .targets_version(NonZeroU64::new(targets).unwrap())
            .unwrap()
            .targets_expires(expires)
            .unwrap()
            .snapshot_version(NonZeroU64::new(snapshot).unwrap())
            .snapshot_expires(expires)
            .timestamp_version(NonZeroU64::new(timestamp).unwrap())
            .timestamp_expires(expires)
            .add_target(name, target)
            .unwrap();
        let signed = editor
            .sign(&[Box::new(LocalKeySource {
                path: self.key.clone(),
            })])
            .await
            .unwrap();
        signed
            .write(self.dir.path().join("metadata"))
            .await
            .unwrap();
    }

    async fn sign(&self, versions: [u64; 3]) {
        let name = format!("v1.2.3/{PACKAGE}");
        self.sign_with(&name, "1.2.3", versions, "2099-01-01T00:00:00Z")
            .await;
    }

    /// The package as the app downloaded it, in its own file so a rejected
    /// copy can be removed.
    fn download(&self, bytes: &[u8]) -> PathBuf {
        let path = self.dir.path().join("downloaded.nupkg");
        fs::write(&path, bytes).unwrap();
        path
    }

    fn verify(&self, version: &str, asset: &str, file: &Path) -> Result<(), VerifyError> {
        verify(
            Repository {
                root: &self.root,
                metadata: Url::from_directory_path(self.dir.path().join("metadata")).unwrap(),
                targets: Url::from_directory_path(self.dir.path()).unwrap(),
                state_parent: &self.dir.path().join("work"),
            },
            tough::FilesystemTransport,
            version,
            asset,
            file,
        )
    }
}

/// Run fixture set-up to completion. The client builds its own runtime, so
/// checks run outside this one.
fn block<T>(work: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(work)
}

/// A fixture with version 1.2.3's package signed at metadata version 2.
fn signed() -> Fixture {
    let fixture = block(Fixture::new());
    block(fixture.sign([2, 2, 2]));
    fixture
}

#[test]
fn configured_production_root_is_valid_and_shared_with_mixer_updates() {
    assert_eq!(configured(), crate::mixer_update::configured());
    if !configured() {
        return;
    }
    let root: tough::schema::Signed<Root> = serde_json::from_slice(TRUSTED_ROOT).unwrap();
    root.signed.verify_role(&root).unwrap();
    // Release assets keep fixed names under their tags.
    assert!(!root.signed.consistent_snapshot);
}

#[test]
fn a_correctly_signed_package_is_accepted() {
    let fixture = signed();
    let file = fixture.download(b"signed update package");
    fixture.verify("1.2.3", PACKAGE, &file).unwrap();
    assert!(file.exists());
}

#[test]
fn a_package_with_the_wrong_bytes_or_length_is_rejected_and_removed() {
    let fixture = signed();
    for bytes in [
        &b"signed update packagf"[..],
        b"signed update package!",
        b"",
    ] {
        let file = fixture.download(bytes);
        assert_eq!(
            fixture.verify("1.2.3", PACKAGE, &file).unwrap_err(),
            VerifyError::Rejected(MISMATCH.into())
        );
        assert!(!file.exists(), "a rejected download is discarded");
    }
}

#[test]
fn an_unsigned_release_or_asset_is_rejected_and_says_to_try_again_later() {
    let fixture = signed();
    let file = fixture.download(b"signed update package");
    for (version, asset) in [
        ("1.2.4", PACKAGE),
        ("1.2.3", "releases.osx.json"),
        ("1.2.3", "io.github.ibanks42.rc2fx-1.2.4-full.nupkg"),
    ] {
        assert_eq!(
            fixture.verify(version, asset, &file).unwrap_err(),
            VerifyError::Rejected(UNSIGNED.into())
        );
    }
    assert!(file.exists(), "nothing was compared, so the caller decides");
}

#[test]
fn an_older_download_than_the_signed_release_says_it_was_replaced() {
    let fixture = signed();
    let file = fixture.download(b"signed update package");
    assert_eq!(
        fixture.verify("1.2.2", PACKAGE, &file).unwrap_err(),
        VerifyError::Rejected(SUPERSEDED.into())
    );
}

#[test]
fn a_target_whose_signed_version_differs_from_its_name_is_rejected() {
    let fixture = block(Fixture::new());
    let name = format!("v1.2.3/{PACKAGE}");
    block(fixture.sign_with(&name, "1.2.4", [2, 2, 2], "2099-01-01T00:00:00Z"));
    let file = fixture.download(b"signed update package");
    let error = fixture.verify("1.2.3", PACKAGE, &file).unwrap_err();
    assert!(matches!(error, VerifyError::Rejected(_)), "{error:?}");
    assert!(error.message().contains("does not describe"));
}

#[test]
fn metadata_signed_by_a_key_outside_the_root_is_rejected() {
    let fixture = signed();
    let file = fixture.download(b"signed update package");
    fixture.verify("1.2.3", PACKAGE, &file).unwrap();
    // A repository of its own, with newer versions and the same package,
    // signed by a key the trusted root does not name.
    let impostor = block(Fixture::new());
    block(impostor.sign([3, 3, 3]));
    for role in ["targets.json", "snapshot.json", "timestamp.json"] {
        fs::copy(
            impostor.dir.path().join("metadata").join(role),
            fixture.dir.path().join("metadata").join(role),
        )
        .unwrap();
    }
    let error = fixture.verify("1.2.3", PACKAGE, &file).unwrap_err();
    assert!(matches!(error, VerifyError::Unavailable(_)), "{error:?}");
    assert!(error.message().starts_with("Could not check"), "{error}");
    assert!(file.exists());
}

#[test]
fn expired_metadata_is_not_trusted() {
    let fixture = block(Fixture::new());
    let name = format!("v1.2.3/{PACKAGE}");
    block(fixture.sign_with(&name, "1.2.3", [2, 2, 2], "2001-01-01T00:00:00Z"));
    let file = fixture.download(b"signed update package");
    assert!(matches!(
        fixture.verify("1.2.3", PACKAGE, &file),
        Err(VerifyError::Unavailable(_))
    ));
    assert!(file.exists());
}

#[test]
fn older_roles_mixed_into_newer_metadata_are_refused() {
    let fixture = block(Fixture::new());
    let metadata = fixture.dir.path().join("metadata");
    let check = || {
        fixture.verify(
            "1.2.3",
            PACKAGE,
            &fixture.download(b"signed update package"),
        )
    };
    block(fixture.sign([5, 5, 5]));
    let old = |role: &str| fs::read(metadata.join(role)).unwrap();
    let (old_targets, old_snapshot) = (old("targets.json"), old("snapshot.json"));
    block(fixture.sign([6, 6, 6]));
    check().unwrap();
    // The timestamp and snapshot one load fetches name the versions of the
    // roles after them, so an older role is refused within that load.
    fs::write(metadata.join("targets.json"), &old_targets).unwrap();
    assert!(matches!(check(), Err(VerifyError::Unavailable(_))));
    block(fixture.sign([6, 6, 6]));
    fs::write(metadata.join("snapshot.json"), &old_snapshot).unwrap();
    assert!(matches!(check(), Err(VerifyError::Unavailable(_))));
}

#[test]
fn every_check_starts_fresh_and_leaves_no_state() {
    let fixture = block(Fixture::new());
    let check = || {
        fixture.verify(
            "1.2.3",
            PACKAGE,
            &fixture.download(b"signed update package"),
        )
    };
    block(fixture.sign([5, 5, 5]));
    check().unwrap();
    // Nothing seen before holds a later check back: a replay at this
    // address cannot freeze app updates (see the module comment).
    block(fixture.sign([4, 4, 4]));
    check().unwrap();
    let work = fixture.dir.path().join("work");
    assert_eq!(fs::read_dir(&work).unwrap().count(), 0, "state was kept");
}

#[test]
fn invalid_versions_and_asset_names_are_refused_before_any_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("package");
    fs::write(&file, b"x").unwrap();
    for (version, asset) in [
        ("1.2.3", ""),
        ("1.2.3", "../SHA256SUMS"),
        ("1.2.3", "a/b"),
        ("1.2.3", "a\\b"),
        ("1.2.3", ".hidden"),
        ("one", PACKAGE),
    ] {
        let error = verify(
            Repository {
                root: b"null",
                metadata: Url::parse("file:///nonexistent/").unwrap(),
                targets: Url::parse("file:///nonexistent/").unwrap(),
                state_parent: &dir.path().join("state"),
            },
            tough::FilesystemTransport,
            version,
            asset,
            &file,
        )
        .unwrap_err();
        assert!(matches!(error, VerifyError::Rejected(_)), "{error:?}");
        assert!(
            error.message().contains("is not an app update package"),
            "{error}"
        );
    }
    assert!(!dir.path().join("state").exists());
}

#[test]
fn app_metadata_comes_only_from_its_own_branch_on_the_official_hosts() {
    let metadata = Url::parse(METADATA_URL).unwrap();
    let targets = Url::parse(TARGETS_URL).unwrap();
    check_repository_url(&metadata, METADATA_HOST).unwrap();
    check_repository_url(&targets, TARGETS_HOST).unwrap();
    // tough joins file names onto the base, so the branch path must survive.
    assert_eq!(
        metadata.join("timestamp.json").unwrap().as_str(),
        "https://raw.githubusercontent.com/ibanks42/rc2fx/app-metadata/timestamp.json"
    );
    assert_eq!(
        targets
            .join("v1.2.3/rc2fx-linux-x64.AppImage")
            .unwrap()
            .as_str(),
        "https://github.com/ibanks42/rc2fx/releases/download/v1.2.3/rc2fx-linux-x64.AppImage"
    );
    for bad in [
        "http://raw.githubusercontent.com/ibanks42/rc2fx/app-metadata/",
        "https://raw.githubusercontent.com.example.com/x/",
        "https://raw.githubusercontent.com:8443/x/",
        "https://user@raw.githubusercontent.com/x/",
    ] {
        let url = Url::parse(bad).unwrap();
        assert!(check_repository_url(&url, METADATA_HOST).is_err(), "{bad}");
    }
}
