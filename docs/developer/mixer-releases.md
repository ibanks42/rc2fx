# Signed mixer releases

The desktop app and mixer runtime are versioned independently. Standard desktop
builds fetch a signed mixer payload and firmware catalog from GitHub. The
installer still patches that payload into official RØDE firmware and flashes
the image over USB whenever you're doing a first install or changing the RØDE
firmware version. If the underlying RØDE firmware isn't changing, the app can
push that same signed payload over USB 1 and let the mixer update itself in
place (see [In-place updates](#in-place-updates)).

## Build modes

`just app` and `just desktop` don't need a staged mixer runtime. If you want an
optional offline payload baked in, use `just app-embedded`,
`just desktop-embedded`, or `./xtask app --embedded` (which sets the `embedded`
build tag) after running `./xtask mixer-build`. Package that mode with
`./xtask package --embedded`. Plain `just package` builds download-only, and
`--skip-build` still checks that the binary matches the requested mode. Using
an embedded payload is always an explicit choice in the installer, never an
automatic fallback when the network fails.

Download-only builds ship with the public update trust root and installer
logic, but no firmware catalog or mixer binaries. Until a signed catalog is
fetched, firmware choices and readiness stay unavailable. Stock restores also
rely on authenticated firmware pins. On Linux, the privileged helper verifies
metadata and payloads on its own and keeps rollback state under
`/var/lib/rc2fx/mixer-updates`; the desktop process can't redirect where it
writes.

## Compatibility declarations

- `config/mixer-release.json`: mixer version and control protocol number.
- `config/mixer-compatibility.json`: `minimumAppVersion` for the mixer release
  as a whole. Bump this whenever a new mixer feature needs desktop app support,
  even if the control protocol number didn't change.
- `config/firmware-profiles.json`: per-profile `minimumAppVersion`. Bump this
  when preparing or driving a specific RØDE firmware needs newer installer or
  app behavior.

Both minimums are enforced together: if a mixer release requires app 1.2.0 and
a firmware profile requires 1.3.0, only app 1.3.0 or newer can install it.
Compatibility is re-checked after downloading and when reusing cached files,
and the UI tells the user when a withheld version needs an app update.

The firmware profile catalog is signed as its own TUF target, and release
signing proves it's byte-for-byte identical to the catalog inside the payload.
The client validates its signature, size, hash, schema, profile identities,
firmware pins, and compatibility list. That means new firmware profiles can go
out without a desktop app release as long as the existing installer understands
the format and runtime ABI.

## Initial trust setup

`config/mixer-update-root.json` holds the signed public production trust root;
private keys never live in the repo. A `null` root disables downloaded releases
in an unconfigured checkout. Test keys are generated in temp directories and
never become app trust anchors. Having a root configured isn't enough on its
own; signed release assets still have to be published before downloads work.

Generate the production root on the machine that will hold the offline root
private key. For a single custodian on Linux or macOS:

```sh
./xtask mixer-keys "$HOME/rc2fx-signing"
```

The target directory must not exist yet, must sit outside any git checkout, and
must have an existing parent directory. Put it on an encrypted local disk, not
in a cloud-synced folder. The command creates owner-only directories and files,
four separate RSA-3072 PEM keys, and a signed public root with one signer per
role and a three-year expiry. It verifies the root before writing `COMPLETE`,
and never touches the repo, calls GitHub, or publishes anything. If it errors
out, it leaves the partial directory private. Don't use it unless `COMPLETE`
is there.

Read the generated `SETUP.md`. Double-check your offline backups of
`offline/root.pem` before deleting the online copy. Copy only `public/root.json`
into `config/mixer-update-root.json`. The PEM files under `release/` go into
the GitHub secrets listed below. Keep all private keys out of chat, logs, CI
artifacts, and git. Running this locally isn't an air-gapped ceremony; if your
custody policy requires one, build and run the tool on an offline machine.
Never use `mixer-keys` to replace a root that's already shipped to users.

If you need multiple custodians or custom thresholds, use `tuftool` and follow
its [root management documentation](https://github.com/awslabs/tough/tree/develop/tuftool).
Create distinct keys for `root`, `targets`, `snapshot`, and `timestamp` (never
reuse the root key for another role), set real thresholds (the tool's defaults
are unusable placeholders), pick a root expiry long enough for operational
recovery, and back up the offline keys safely.

Before signing the root, set `consistent_snapshot` to `false` because GitHub
release assets use fixed filenames under versioned tags. Sign the root with
`tuftool root sign`, verify its signatures, and copy **only the signed public
root JSON** into `config/mixer-update-root.json` (never commit private keys).
Changing the root later requires either a new desktop build for initial trust
or a sequential cross-signed root chain for existing installs.

Set up two GitHub environments restricted to the protected `main` branch:

| Environment | Secrets | Policy |
| --- | --- | --- |
| `mixer-signing` | `MIXER_TARGETS_KEY`, `MIXER_SNAPSHOT_KEY`, `MIXER_TIMESTAMP_KEY` | Require a release reviewer. No keys are exposed to the build job. |
| `mixer-renewal` | `MIXER_SNAPSHOT_KEY`, `MIXER_TIMESTAMP_KEY` | Scheduled renewal only; no target or root signing key. |

All secret values are PEM private keys, and the root private key doesn't belong
in either environment. The renewal command rejects any key authorized for `root`
or `targets`. Only turn on the repository variable `MIXER_UPDATES_ENABLED=true`
once the first signed release and its metadata are live.

## Release lifecycle

1. Check both app minimum versions and, if you have them ready, add curated
   mixer notes for the upcoming version. Commit those on protected `main`.
2. Trigger **Mixer release** and pick the bump (`patch`, `minor`, or `major`)
   relative to the latest published `mixer-v` release.
   `.github/scripts/mixer_version.py` writes the bumped version to
   `config/mixer-release.json` before building. If `config/mixer-release.json`
   was already bumped to that exact version in git, it's kept; any other
   mismatch stops the run. The first job builds and tests the mixer, packs the
   fixed-file envelope, firmware catalog, and license notices, and uploads an
   unsigned artifact with no signing keys in sight.
3. Inspect the artifact and approve the `mixer-signing` job. It re-validates
   the entire payload and catalog, then signs the TUF `targets`, `snapshot`,
   and `timestamp` metadata.
4. If the version changed, the job commits `config/mixer-release.json` to
   `main` as `chore(mixer-release): VERSION`, provided `main` is still at the
   commit the run built (if `main` moved, kick off a new run), and tags the
   release at that commit.
5. Publishing creates a draft `mixer-vVERSION` release, uploads the assets,
   downloads them back to verify every byte, and publishes the payload first.
   Only after that does it push a single commit with all metadata files to the
   `mixer-metadata` branch and wait for the new files to be served (see
   [Where metadata lives](#where-metadata-lives)). If publishing is interrupted,
   it fails closed so clients never see a half-matched metadata set.

If publishing fails *after* the payload release went live, you can't publish
that release a second time, so don't rerun the workflow. Finish it using the
signed files saved in the `signed-mixer-metadata` artifact (kept for seven
days):

```sh
gh run download RUN_ID --name unsigned-mixer --dir PACKAGE
gh run download RUN_ID --name signed-mixer-metadata --dir SIGNED_METADATA
python3 .github/scripts/publish_mixer.py --resume PACKAGE SIGNED_METADATA
```

If it failed before the payload was published, just delete the draft
`mixer-vVERSION` release and dispatch **Mixer release** again.

Locally, the equivalents are `./xtask mixer-package NEW_DIRECTORY` and
`./xtask mixer-sign ROOT PACKAGE OUT SEQUENCE EXPIRES TARGETS_KEY SNAPSHOT_KEY TIMESTAMP_KEY`.
Sequence numbers have to strictly increase (CI uses UTC Unix seconds), `EXPIRES`
is an RFC 3339 timestamp, and all key arguments are file paths, not raw PEM
strings.

To test a mixer release locally without publishing, build the app with
`-tags rc2fxdev` (what `just app` builds) and set
`RC2FX_DEV_MIXER_REPOSITORY=FOLDER`, where `FOLDER` mirrors the published
layout (`FOLDER/metadata/` for the role files and `FOLDER/mixer-vVERSION/` for
the targets). Build the app and mixer against a throwaway root from
`./xtask mixer-keys` in `config/mixer-update-root.json` and sign with its
throwaway keys (never production keys). Builds without `rc2fxdev` ignore that
env var. Since flashing runs inside the `pkexec` helper (which doesn't see the
env var), you can only test in-place updates this way.

Fixing a broken payload always means cutting a new mixer version. The publisher
refuses to overwrite an existing mixer release, and GitHub enforces the same
once immutable releases are enabled. Discovery metadata is mutable by design,
protected by TUF signatures, expiry, and stored version numbers. Always keep
every numbered root file on the branch so older clients can walk the full
rotation chain, and never overwrite an old numbered root with different bytes.

## In-place updates

When an update only touches RC2FX, you don't need to reflash RØDE's firmware.
The app streams the signed payload over USB 1, the mixer verifies it using the
same trust root and metadata the desktop trusts, and swaps `/opt/rc2fx` in
place. RØDE's files, its audio engine binary, and the firmware stay untouched;
only the audio engine restarts. See
[the controller API](../reference/api.md#in-place-updates) for the endpoints.

Verification happens on the mixer itself against the root compiled into
`rc2fxd` (and the app), so the host computer isn't trusted to authorize a
release:

1. All four role documents are verified as a TUF repo: root signatures and
   thresholds, key rotation if intermediate roots were sent, `timestamp`,
   `snapshot`, and `targets`, plus all cross-file hashes and versions.
   Delegated targets are rejected. The mixer verifies with
   [sigstore-tuf](https://github.com/sigstore/sigstore-rust); the desktop app
   parses metadata with [go-tuf](https://github.com/theupdateframework/go-tuf)
   and mirrors sigstore-tuf's refresh, rollback, and expiry rules in
   `desktop/internal/tuf` so both sides accept the exact same repos.
   Signing uses `./xtask` (`desktop/internal/xtask/tufsign`, matching `tough`'s
   output) and `tuftool`, with shared signed fixtures in `fixtures/xtask`. A
   root listing the same key under two IDs for a role is rejected because the
   verifier counts signatures by key ID.
2. Exactly one target may carry `rc2fxMixer` compatibility claims, and its
   version, length, and SHA-256 have to match the request and uploaded bytes.
3. Its `firmwareProfiles` must include the profile validated for this running
   mixer, and its `minimumAppVersion` and release identity must be valid.
4. The payload's envelope, `SHA256SUMS`, ARM64 binaries, firmware catalog, and
   `mixer-release.json` must all match those claims. Dev builds are rejected;
   there is no unsigned or developer bypass on this API.
5. Nothing executes and nothing under `/opt` is modified until every check
   above passes.

Because the mixer has no trusted real-time clock, it can't enforce TUF expiry
(a wrong or unset clock would either block every update or silently accept a
stale one). Instead, it stores the highest verified `root`, `targets`,
`snapshot`, and `timestamp` versions, plus a digest of the verified targets,
alongside its settings in `/opt/rc2fx/data/update-state.json`, and rejects any
older set or different content under the same version. It also tracks the
installed RC2FX version and blocks downgrades unless explicitly requested. A
full flash replaces `update-state.json` and resets tracking. If the file exists
but can't be read or parsed, the mixer refuses to treat it as a clean slate and
blocks in-place updates until a full flash replaces it.

Once verified, the bundle is staged on `/opt`'s filesystem, re-checked from
disk, and verified against the running hardware with its own
`rc2fx-firmware check-device`. Live files get hard-linked into
`/opt/.rc2fx-previous`, `/opt/.rc2fx-update-pending` is written and flushed,
and only then are the new files renamed into place followed by the two
`/usr/sbin` shims. Saved settings in `/opt/rc2fx/data` are never touched.

Next, `rc2fxd` copies `rc2fx.sh` into RAM and launches it detached as
`update-commit`. That driver restarts the mixer (the same way `rc2fx.sh restart`
does), waits for the new controller to record its startup and for audio
callbacks to advance, and commits by deleting the pending marker and then
`/opt/.rc2fx-previous` (freeing the old release's space). If the new version
doesn't come up healthy, the driver restarts into `/opt/.rc2fx-previous`, and
the previous controller restores the files and logs the rollback. After the app
reconnects, `GET api/v1/update/last` reports how it went.

If an update doesn't change any file held open by RØDE's running mixer process
(the native and GUI libraries, supervisor, JACK recorder, `rc2fx.sh`, and audio
shim), audio doesn't even have to stop. `rc2fxd` passes `restart=controller` to
the driver, which triggers a reload by creating `/run/rc2fx/reload`. The
supervisor stops the old controller and starts the new `rc2fxd` (which refuses
to start unless the native library's paths and settings match what's saved).
This path is only used when the mixer was booted from `/opt/rc2fx` with
matching files and the supervisor advertises `reload` in
`/run/rc2fx/supervisor.features`. Readiness, commit, and rollback work the same
way; if the new controller isn't ready within a minute, the driver falls back
to a full mixer restart.

A power loss at any point still leaves a bootable system:

| Cut during | Next boot |
| --- | --- |
| upload or verification | The installed release; `/run` and the staged copy are gone |
| before the pending marker | The installed release; a leftover stage directory is removed |
| after the pending marker, before the restart | `.rc2fx-previous` is started, and its controller restores the files |
| during the restart or readiness wait | `.rc2fx-previous` is started and restores the files |
| after the commit | The new release |

Whenever the pending marker exists, both `/usr/sbin` shims boot
`.rc2fx-previous` unless the update driver process is still alive (which is how
the committed restart reaches the new release). Changes to the GUI shim only
take effect on the next boot, and the update outcome notes when it changed.

### Delta uploads

A full envelope is around 11 MiB, while a point release typically changes well
under 1 MiB. Mixers advertising `inPlaceUpdates: 2` expose
`GET api/v1/update/installed` with a digest for every payload file under
`/opt/rc2fx`. The app diffs the signed envelope against that list and uploads a
delta with `"encoding": "delta-1"` and its byte length as `uploadSize`. Older
mixers respond with `400`, and the app simply falls back to uploading the full
envelope.

The delta format (crate `rc2fx-delta`) is a series of ASCII headers followed by
raw bytes: a `head` containing the envelope header and manifest, then one op
per file. `copy` reuses the installed file as-is, `zstd <n>` sends the whole
file compressed, and `patch <base-sha256> <n>` sends a zstd frame decoded using
the installed file as its reference prefix (`zstd --patch-from`). The app only
patches against bytes whose digest matches what the mixer reported.

The mixer decodes the delta into `/run` before verification, checking every
file's SHA-256, exact size, and patch base digest. From there, the rebuilt
envelope goes through the exact same pipeline as a plain upload: signed target
size and digest, TUF, payload manifest, and version checks. Nothing decoded is
run or installed until all of that passes.

## Where metadata lives

Payloads are GitHub release assets, published once and never modified. Metadata
is re-signed on every renewal, so it can't live on an immutable GitHub release.
Instead it lives on the orphan branch `mixer-metadata` (holding only
`N.root.json`, `targets.json`, `snapshot.json`, `timestamp.json`, and a short
README), which the app fetches from
`https://raw.githubusercontent.com/ibanks42/rc2fx/mixer-metadata/`. Both that
URL and the payload URL are compiled into the app, which rejects any other host.

Apps older than 0.1.4 still look at the legacy `mixer-metadata` release. Since
nothing renews it anymore, its metadata expires within seven days of the last
renewal, causing those older apps to fail closed and show the mixer feed as
unavailable until the user updates the app (app update checks don't depend on
this metadata).

Each metadata update is a single commit, so the branch never contains a partial
set. Pushes use `--force-with-lease` against the tip the commit was built on;
if another writer raced it, the push fails instead of clobbering anything, and
you just re-run.

`raw.githubusercontent.com` caches each file for roughly five minutes. The
publisher polls until the new files show up, waiting up to seven minutes. If it
times out, the push still succeeded and the error lists which files lagged.
Because caching is per file, a client might briefly see a mix of old and new
files, fail TUF verification, and report no update until the cache clears a few
minutes later.

**Moving to the branch:** you don't need to create it manually. The first
**Mixer release** creates `mixer-metadata` if it doesn't exist. Run it right
after merging and before cutting an app release that reads from the branch,
otherwise **Renew mixer metadata** will fail to fetch and new apps won't see a
feed. Once it exists, add a branch protection rule on `mixer-metadata` to block
deletion while allowing GitHub Actions and the maintainer to push. Leave force
pushes blocked (every update builds on the tip), and don't require PRs or
status checks since renewal pushes straight to the branch.

## If the targets key leaks

The targets key controls which mixer software clients will trust, so anyone
holding a leaked targets key can sign a malicious payload until it's revoked.

1. Set `MIXER_UPDATES_ENABLED=false` to halt publication. Hold off on replacing
   the `MIXER_TARGETS_KEY` secret until the rotation below is ready.
2. Offline, create the next sequential root version authorizing a fresh targets
   key, cross-signed as described in
   [Renewal and recovery](#renewal-and-recovery). As long as the root key is
   safe, existing installs will follow the signed root chain without needing an
   app update.
3. Reauthorize the current release with the new key using version numbers
   higher than anything the leaked key could have signed.
4. Cut a desktop app release shipping the new root as its initial trust so new
   installs don't bootstrap from a root that still trusts the compromised key.

If the root key leaked too, the rotation chain can't be trusted; follow the
final step in [Renewal and recovery](#renewal-and-recovery).

Both `MixerTarget` and `MixerOffer` reject unknown fields, which effectively
freezes the signed metadata schema: adding a field from a newer publisher will
cause older clients to reject the metadata. Only add fields once every
supported app knows about them, or bump the payload format version.

## Renewal and recovery

`targets.json` is re-signed on every mixer release and otherwise stays valid
until the trust root expires (currently 2029-11-04), so nothing needs
reauthorizing between releases. `snapshot.json` and `timestamp.json` expire
after seven days. Once enabled, **Renew mixer metadata** runs twice a week: it
verifies the existing chain, keeps `targets.json` byte-for-byte intact, and
signs fresh snapshot and timestamp files. It can't authorize a different binary
or extend targets/root expiry, and it refuses stale sequences, altered targets,
or expiries past either authorization.

If a renewal fails, fix it before the seven-day window runs out or clients will
stop offering downloads. Don't bypass expiry checks to restore service.

Root renewal is the one manual chore. Well ahead of expiry, sign a new root
with the offline root key (see key rotation below) and ship an app build with
it as initial trust. Because `targets.json` still expires with the old root,
either publish a new mixer release afterwards or reauthorize the current one.
Automated renewal stops a week before expiry since discovery metadata can't
outlive the root or targets.

`--reauthorize` is a recovery tool for replacing the targets signature on an
already-published release without bumping the version (for instance, right
after a key rotation).

Keep the original package directory (including its compatibility file), run
`./xtask mixer-sign` with that package, the current trusted root, a clean
output directory, and a sequence number higher than the currently published
targets, snapshot, and timestamp versions. Inspect the signed metadata, then
run:

```sh
python3 .github/scripts/publish_mixer.py --reauthorize PACKAGE SIGNED_METADATA
```

This expects an already-published mixer release and metadata branch. Before
writing anything, it verifies that target paths, hashes, and compatibility
fields haven't changed and downloads the live release assets to check their
bytes. Only `targets`, `snapshot`, and `timestamp` change, in a single commit.
Any change to payloads or compatibility still requires a new mixer release. Run
this under the same exclusive publication lock as the release and renewal
workflows, and never run concurrent metadata publishers.

To rotate keys, create the next sequential root version offline, authorize the
replacement keys, and cross-sign it with both the required old and new root
keys. Publish every numbered root in order, keep all intermediates, and renew
release metadata with the new role keys. If the root keys themselves are
compromised, ship a replacement trust root in a separately trusted app release,
since swapping an unsigned file on GitHub won't recover trust.
