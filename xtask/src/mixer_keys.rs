//! Local, explicit key creation. Never contacts GitHub or modifies the checkout.
use crate::Result;
use std::path::Path;

pub fn run(destination: &Path) -> Result {
    #[cfg(unix)]
    {
        create(destination, &crate::root())?;
        println!("Created signing material in {}", destination.display());
        println!("Read SETUP.md there. Back up offline/root.pem offline; never upload it.");
        println!(
            "Only public/root.json belongs in the checkout. No repository files or GitHub settings were changed."
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = destination;
        Err(
            "mixer-keys currently requires Linux or macOS to enforce private file permissions"
                .into(),
        )
    }
}

#[cfg(unix)]
pub(super) fn create(destination: &Path, checkout: &Path) -> Result {
    use aws_lc_rs::{encoding::AsDer, rand::SystemRandom, rsa::KeySize, signature::RsaKeyPair};
    use base64::{Engine, engine::general_purpose::STANDARD};
    use std::{
        collections::HashMap,
        fs,
        io::Write,
        num::NonZeroU64,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };
    use tough::{
        editor::signed::SignedRole,
        key_source::{KeySource, LocalKeySource},
        schema::{KeyHolder, RoleKeys, RoleType, Root},
    };

    if !destination.is_absolute() || destination.file_name().is_none() {
        return Err("choose an absolute, new directory outside every git checkout".into());
    }
    let parent = destination
        .parent()
        .ok_or("destination needs a parent")?
        .canonicalize()
        .map_err(|e| format!("destination parent must already exist: {e}"))?;
    let checkout = checkout.canonicalize().map_err(|e| e.to_string())?;
    if parent.starts_with(&checkout) || parent.ancestors().any(|p| p.join(".git").exists()) {
        return Err("private keys must be outside every git checkout".into());
    }
    let destination = parent.join(destination.file_name().unwrap());
    // create_dir is exclusive; neither overwrite nor follow an existing symlink.
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&destination)
        .map_err(|e| format!("use a new key directory (existing paths are never replaced): {e}"))?;
    let write_private = |path: &Path, bytes: &[u8]| -> Result {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())
    };
    for name in ["offline", "release", "public"] {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(destination.join(name))
            .map_err(|e| e.to_string())?;
    }
    // Leave clear recovery instructions even if generation later fails. Never
    // automatically erase possibly valuable private keys after a partial error.
    write_private(&destination.join("SETUP.md"), INSTRUCTIONS.as_bytes())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let mut public = HashMap::new();
        let mut roles = HashMap::new();
        let mut root_source: Vec<Box<dyn KeySource>> = Vec::new();
        for role in [
            RoleType::Root,
            RoleType::Targets,
            RoleType::Snapshot,
            RoleType::Timestamp,
        ] {
            let key_pair = RsaKeyPair::generate(KeySize::Rsa3072).map_err(|e| e.to_string())?;
            let document = key_pair.as_der().map_err(|e| e.to_string())?;
            let encoded = STANDARD.encode(document.as_ref());
            let lines = encoded
                .as_bytes()
                .chunks(64)
                .map(|line| std::str::from_utf8(line).unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            let pem = format!("-----BEGIN PRIVATE KEY-----\n{lines}\n-----END PRIVATE KEY-----\n");
            let folder = if role == RoleType::Root {
                "offline"
            } else {
                "release"
            };
            let path = destination.join(folder).join(format!("{role}.pem"));
            write_private(&path, pem.as_bytes())?;
            let source = LocalKeySource { path };
            let key = source.as_sign().await.map_err(|e| e.to_string())?.tuf_key();
            let id = key.key_id().map_err(|e| e.to_string())?;
            public.insert(id.clone(), key);
            roles.insert(
                role,
                RoleKeys {
                    keyids: vec![id],
                    threshold: NonZeroU64::new(1).unwrap(),
                    _extra: HashMap::new(),
                },
            );
            if role == RoleType::Root {
                root_source.push(Box::new(source));
            }
        }
        let root = Root {
            spec_version: "1.0.0".into(),
            consistent_snapshot: false,
            version: NonZeroU64::new(1).unwrap(),
            expires: jiff::Timestamp::now() + std::time::Duration::from_secs(3 * 365 * 86400),
            keys: public,
            roles,
            _extra: HashMap::new(),
        };
        let signed = SignedRole::new(
            root.clone(),
            &KeyHolder::Root(root),
            &root_source,
            &SystemRandom::new(),
        )
        .await
        .map_err(|e| e.to_string())?;
        signed
            .signed()
            .signed
            .verify_role(signed.signed())
            .map_err(|e| e.to_string())?;
        write_private(&destination.join("public/root.json"), signed.buffer())?;
        write_private(
            &destination.join("public/root.sha256"),
            format!("{}  root.json\n", crate::sha256(signed.buffer())).as_bytes(),
        )?;
        // Written last: its presence distinguishes a finished setup from a
        // partial directory that must not be used for production trust.
        write_private(
            &destination.join("COMPLETE"),
            b"Root signatures verified. Follow SETUP.md before use.\n",
        )
    })
}

#[cfg(unix)]
const INSTRUCTIONS: &str = r#"# RC2FX signing setup

Generation is complete only if COMPLETE exists. If it does not, keep this
folder private and retry in a different new folder; do not use a partial root.
The command never uploads keys, changes the checkout, or publishes releases.

## 1. Back up the root key

The private keys here are unencrypted PEM files protected by owner-only Unix
permissions. Use an encrypted disk and encrypted offline backup storage.
The root has one signer (you), a threshold of one, and a three-year expiry.
Each release role has a separate key and a threshold of one. Keys are RSA-3072
with RSA-PSS/SHA-256 signatures, in the PEM format accepted by the signer.

Copy offline/root.pem, public/root.json and public/root.sha256 to secure offline
backup storage. Verify the backup is readable before removing any original.
Keep a second protected backup. Keep the root private key off GitHub and out of
the checkout, logs, chat, and ordinary cloud-sync folders. Generation on this
computer is not an air-gapped ceremony; for that, run this tool on an offline
Linux/macOS machine with the executable and dependencies prepared beforehand.
After verifying backups, retain root.pem offline, not on the build machine.

## 2. Install only the public root

Copy public/root.json to config/mixer-update-root.json in the RC2FX checkout.
Only this JSON contains public keys and signatures safe to commit. Review its
role IDs and expiry and record a reminder to rotate the root well before expiry.
Keep the numbered root chain for existing clients. Do not replace a deployed
root with an unrelated root or rerun this tool to rotate deployed keys.
Rebuild and test the app after installing its initial public trust root.

## 3. Configure GitHub environments

In repository settings, create environments restricted to protected main:

- mixer-signing: require a release reviewer. Add MIXER_TARGETS_KEY from
  release/targets.pem, MIXER_SNAPSHOT_KEY from release/snapshot.pem and
  MIXER_TIMESTAMP_KEY from release/timestamp.pem.
- mixer-renewal: add only MIXER_SNAPSHOT_KEY and MIXER_TIMESTAMP_KEY, from the
  same files. It must not contain the targets key or the root key.

Use GitHub's secret-entry UI or gh secret set with file input. Never put private
key contents in shell arguments. Protect backups of release keys as well.
Do not upload the key directory as an artifact. Private keys are never copied
into release packages. No environment receives offline/root.pem.

## 4. Publish only when separately approved

Follow docs/developer/mixer-releases.md. Review both minimum app versions and
current qualification results before approving a mixer release. Do not enable
MIXER_UPDATES_ENABLED until the first signed payload and metadata are public.
Then set that repository variable to true for scheduled metadata renewal.
Short-lived metadata requires monitoring and renewal; signing setup alone does
not make updates available. This setup does not authorize publication.
"#;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };
    use tough::schema::{RoleType, Root, Signed};

    #[test]
    fn creates_separate_private_keys_and_verified_public_root_without_overwriting() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        let output = temp.path().join("keys");
        create(&output, &checkout).unwrap();
        let bytes = fs::read(output.join("public/root.json")).unwrap();
        let root: Signed<Root> = serde_json::from_slice(&bytes).unwrap();
        root.signed.verify_role(&root).unwrap();
        assert!(!root.signed.consistent_snapshot);
        assert_eq!(root.signed.keys.len(), 4);
        assert_eq!(root.signatures.len(), 1);
        assert!(
            root.signed.expires
                > jiff::Timestamp::now() + std::time::Duration::from_secs(365 * 86400)
        );
        let mut ids = std::collections::HashSet::new();
        for role in [
            RoleType::Root,
            RoleType::Targets,
            RoleType::Snapshot,
            RoleType::Timestamp,
        ] {
            let settings = &root.signed.roles[&role];
            assert_eq!(settings.threshold.get(), 1);
            assert!(ids.insert(settings.keyids[0].clone()));
            let folder = if role == RoleType::Root {
                "offline"
            } else {
                "release"
            };
            let file = output.join(folder).join(format!("{role}.pem"));
            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert!(
                fs::read_to_string(file)
                    .unwrap()
                    .starts_with("-----BEGIN PRIVATE KEY-----")
            );
        }
        assert_eq!(
            fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(output.join("COMPLETE").exists());
        assert!(create(&output, &checkout).is_err());
        assert_eq!(fs::read(output.join("public/root.json")).unwrap(), bytes);
        assert_eq!(fs::read_dir(checkout).unwrap().count(), 0);
    }

    #[test]
    fn refuses_checkout_paths_relative_paths_and_symlink_destinations() {
        let temp = tempfile::tempdir().unwrap();
        let checkout = temp.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        assert!(create(&checkout.join("keys"), &checkout).is_err());
        assert!(create(Path::new("relative-keys"), &checkout).is_err());
        let alias = temp.path().join("alias");
        symlink(&checkout, &alias).unwrap();
        assert!(create(&alias.join("keys"), &checkout).is_err());
        assert!(create(&alias, &checkout).is_err());
        assert!(!checkout.join("keys").exists());
    }
}
