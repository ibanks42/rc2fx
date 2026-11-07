# Signed mixer releases

Desktop and mixer versions are independent. Normal desktop builds download a
signed mixer payload and firmware catalog from GitHub. The installer still adds
that payload to official RØDE firmware and flashes the resulting image over USB.
Nothing in this release system updates files in place on the device.

## Build modes

`just app` and `just desktop` need no staged mixer runtime. To include an optional
offline payload, use `just app-embedded`, `just desktop-embedded`, or build the
app with `--features embedded` after `cargo xtask mixer-build`. Package that mode
with `cargo xtask package --embedded`. The ordinary packaging command builds
download-only. `--skip-build` still verifies that the executable has the requested
mode. An embedded payload is an explicit installer choice, never a network-error
fallback.

Download-only builds contain the public update trust root and installer code,
but no firmware profile catalog or mixer binaries. Before a signed catalog has
been fetched, firmware choices and readiness are unavailable. Stock restores
also use authenticated firmware pins. The Linux helper independently verifies
metadata and payloads, keeping its rollback state under
`/var/lib/rc2fx/mixer-updates`; the desktop cannot redirect those writes.

## Compatibility declarations

- `config/mixer-release.json`: mixer version and control protocol.
- `config/mixer-compatibility.json`: `minimumAppVersion` for the whole mixer
  release. Raise it when a new mixer feature requires app support even if the
  control protocol number stays the same.
- `config/firmware-profiles.json`: each profile has its own
  `minimumAppVersion`. Raise it when preparing or controlling that particular
  firmware requires newer installer/app behavior.

Both minimum versions are enforced. For example, a mixer release requiring
app 1.2.0 and a firmware profile requiring 1.3.0 can only be installed by app
1.3.0 or newer. Compatibility is checked again after download and on cache reuse.
The app explains withheld versions as requiring an app update.

The profile catalog is a separate TUF target. Release signing proves it is
byte-for-byte the catalog inside the payload. The client checks its signature,
size, hash, schema, profile identities, firmware pins, and compatibility list.
New firmware profiles can therefore ship without a desktop rebuild when the
existing installer understands their format and runtime ABI.

## Initial trust setup

`config/mixer-update-root.json` contains the signed public production trust root.
Private keys are kept outside the checkout. A `null` root disables downloaded
releases in an unconfigured checkout. Test signing keys are generated in
temporary directories and never become app trust anchors. A configured root
alone does not provide downloads: signed release assets must also be published.

Prepare the production root on the machine that will hold the offline root
private key. For a single custodian on Linux or macOS, run:

```sh
cargo xtask mixer-keys "$HOME/rc2fx-signing"
```

The destination must be new, outside every Git checkout, with an existing
parent directory. Use an encrypted local disk, not a cloud-synced folder.
The tool creates owner-only files and directories, four separate RSA-3072 PEM
keys, and a signed public root with one signer per role and a three-year expiry.
It verifies the root before writing `COMPLETE`. It never modifies the checkout,
contacts GitHub, or publishes a release. On error, it retains the partial
directory privately; do not use it unless `COMPLETE` exists.

Read the generated `SETUP.md`. Verify offline backups of `offline/root.pem`
before removing the online original. Copy only `public/root.json` into
`config/mixer-update-root.json`. The files under `release/` are the PEM values
for the GitHub secrets below. Keep all private keys out of chat, logs, artifacts
and version control. Local generation is not an air-gapped ceremony: build and
run the tool on an offline machine if that is your custody policy. Do not use
this initialization command to replace a root already deployed to users.

For multiple custodians or custom thresholds, use `tuftool` and follow its
[root management documentation](https://github.com/awslabs/tough/tree/develop/tuftool).
Create separate keys for the `root`, `targets`, `snapshot`, and `timestamp` roles;
never reuse a root key for another role. Set the thresholds deliberately (the
tool's initial thresholds are unusable placeholders), choose a root expiry long
enough for operational recovery, and make secure backups of offline keys.

Before signing the root, set its public `consistent_snapshot` field to `false`:
GitHub release assets have fixed names beneath versioned release tags. Sign the
root with `tuftool root sign`, verify its signatures, and copy **only the signed
public root JSON** into `config/mixer-update-root.json`. Never commit private keys.
Root changes require a new desktop build for initial trust, or a sequential
cross-signed root chain for existing clients.

Configure two GitHub environments restricted to the protected main branch:

| Environment | Secrets | Policy |
| --- | --- | --- |
| `mixer-signing` | `MIXER_TARGETS_KEY`, `MIXER_SNAPSHOT_KEY`, `MIXER_TIMESTAMP_KEY` | Require a release reviewer. No keys are exposed to the build job. |
| `mixer-renewal` | `MIXER_SNAPSHOT_KEY`, `MIXER_TIMESTAMP_KEY` | Scheduled renewal only; no target or root signing key. |

Secret values are PEM private keys. Root private keys belong in neither
environment. The renewal command refuses any key authorized for root or targets.
Enable the repository variable `MIXER_UPDATES_ENABLED=true` only after the first
signed release and its metadata are public.

## Release lifecycle

1. Bump the mixer version, review both app minimums, and write curated mixer
   notes. Commit the complete release inputs on protected main.
2. Dispatch **Mixer release**. Its first job builds and tests the mixer, packages
   the fixed-file envelope, firmware catalog, and license notices, and uploads
   an unsigned build artifact. It receives no signing keys.
3. Review the artifact and approve the `mixer-signing` job. That job validates
   the complete payload and catalog again, then signs TUF targets, snapshot,
   and timestamp metadata.
4. Publication creates a draft `mixer-vVERSION` release, uploads its assets,
   downloads them again to verify their bytes, and publishes the payload first.
   Only then does it add one commit with all the metadata to the `mixer-metadata`
   branch, then waits for the new files to be served (see
   [Where metadata lives](#where-metadata-lives)). Interrupted publication fails
   closed; clients never accept a partially matching metadata set.

If publication stops after the payload release is published, that release can't be
published again, so don't rerun the job. Finish it with the same signed files, which
the job keeps as its `signed-mixer-metadata` artifact for seven days:

```sh
gh run download RUN_ID --name unsigned-mixer --dir PACKAGE
gh run download RUN_ID --name signed-mixer-metadata --dir SIGNED_METADATA
python3 .github/scripts/publish_mixer.py --resume PACKAGE SIGNED_METADATA
```

If it stops before then, delete the draft `mixer-vVERSION` release and dispatch
**Mixer release** again.

The local equivalents are `cargo xtask mixer-package NEW_DIRECTORY` and
`cargo xtask mixer-sign ROOT PACKAGE OUT SEQUENCE EXPIRES TARGETS_KEY SNAPSHOT_KEY
TIMESTAMP_KEY`. Sequence numbers must increase; workflow sequences use UTC Unix
seconds. `EXPIRES` is an RFC 3339 timestamp. Paths name files, never secret values.

Payload corrections require a new mixer version. The publisher refuses to
overwrite an existing mixer release, and with immutable releases GitHub refuses
too. Discovery metadata is mutable because TUF signatures, expiry, and stored
version numbers provide its trust boundary.
Retain every numbered root file so old clients can verify the complete rotation
chain. Do not replace an old numbered root with different bytes.

## Where metadata lives

Payloads are GitHub release assets, published once and never changed. Metadata
changes at every renewal, so it can't be a release once immutable releases are on.
It lives on the orphan branch `mixer-metadata`, which holds only `N.root.json`,
`targets.json`, `snapshot.json`, `timestamp.json` and a short README, and the app
reads it from `https://raw.githubusercontent.com/ibanks42/rc2fx/mixer-metadata/`.
That address and the payload address are compiled into the app, which refuses
any other host.

Apps older than 0.1.4 read the legacy `mixer-metadata` release instead. Nothing
renews it any more, so its metadata expires within seven days of the last renewal.
Those apps then show the mixer feed as unavailable, failing closed, until they are
updated through the app-update notice, which doesn't depend on this metadata.

Every update is one commit, so the branch never holds a partial set. A push names
the tip it was built on (`--force-with-lease`), so a concurrent writer fails the
push rather than being overwritten; nothing retries it. Run the command again.

raw.githubusercontent.com caches each file for about five minutes. The publisher
reads the files back and waits up to seven minutes for them to appear. If it gives
up, the push has still happened, and its error names the files. Because the cache
is per file, a client can briefly see a mix of old and new files. That fails TUF
verification, so it reports no update rather than a wrong one, and clears within
minutes.

**Moving to the branch:** nothing needs creating by hand. The first **Mixer release**
creates the branch when it doesn't exist. Run it right after merging this change and
before releasing an app that reads the branch: until the branch exists, **Renew mixer
metadata** fails at its fetch step and such an app finds no feed. Afterward, in the
repository's branch rules,
protect `mixer-metadata` against deletion while letting GitHub Actions and the
maintainer push to it. Force pushes can stay blocked, since every update builds on
the tip. Don't require pull requests or status checks on it: renewal pushes
directly.

## If the targets key leaks

The targets key decides which mixer software clients accept, so a leaked key
lets its holder authorize a payload until the key is revoked.

1. Set `MIXER_UPDATES_ENABLED=false` to stop publication, and replace the
   `MIXER_TARGETS_KEY` secret only once the rotation below is ready.
2. Offline, create the next sequential root version that authorizes a new
   targets key, cross-signed as described under
   [Renewal and recovery](#renewal-and-recovery). Existing clients follow the
   signed root chain, so this needs no new app build while the root key is safe.
3. Reauthorize the current release with the new key, with versions greater than
   anything the old key could have signed.
4. Ship a desktop build with the new root as its initial trust, so a fresh
   install doesn't start from the root that still trusts the old key.

If the root key leaked too, the chain can't be trusted: follow the last step of
[Renewal and recovery](#renewal-and-recovery).

`MixerTarget` and `MixerOffer` deny unknown fields, which freezes the schema of
the signed metadata: a field added by a newer publisher makes older clients
refuse that metadata. Add a field only when every supported app understands it,
or with a new payload format version.

## Renewal and recovery

Targets are signed again at every mixer release, and otherwise last until the
trust root expires (currently 2029-11-04), so nothing needs reauthorizing between
releases. Snapshot and timestamp metadata normally last seven days. **Renew mixer
metadata** runs twice weekly after enablement. It verifies the old signed chain,
preserves the exact targets bytes, and signs only new snapshot and timestamp
metadata. It cannot authorize another binary or extend the targets/root expiry. It
refuses a stale sequence, changed targets, or an expiry beyond either authorization.

A renewal failure must be investigated before metadata expires; clients otherwise
stop offering downloads. Do not bypass expiry enforcement to recover service.

Renewing the root is the one manual event. Well before it expires, sign a new root
with the offline root key, as described under key rotation below, and ship an app
build with it as the initial trust. Targets still end at the old root's expiry, so
publish a new mixer release after that, or reauthorize the current one. Renewal
stops a week before the expiry, because discovery metadata can't outlive either.

`--reauthorize` is a repair tool, for replacing the targets signature on a published
release without a new version, such as after a key rotation.

To reauthorize, retain the original package directory, including its compatibility
file. Run `cargo xtask mixer-sign` with that package, the current trusted root,
a fresh output directory, and a sequence greater than the currently published
targets, snapshot and timestamp versions. Review the resulting signed metadata,
then run:

```sh
python3 .github/scripts/publish_mixer.py --reauthorize PACKAGE SIGNED_METADATA
```

This requires an already published mixer release and metadata branch. It checks
that the target paths, hashes and compatibility requirements are unchanged and
downloads the existing assets to compare their bytes before writing anything.
Only targets, snapshot and timestamp change, in one commit.
Changing a payload or compatibility requirement requires a new mixer release.
Run this under the same exclusive publication lock as the release and renewal
workflows; do not run concurrent metadata publishers.

For key rotation, create the next sequential root version offline, authorize the
replacement keys, and cross-sign it with the required old and new root keys.
Publish each numbered root in sequence and retain the intermediates. Renew
release metadata with the newly authorized role keys. If the trusted root keys
are compromised, distribute a corrected desktop trust root through a separately
trusted app release; replacing an unsigned file on GitHub is not recovery.
