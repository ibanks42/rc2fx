package installer

// Check downloaded app updates against signed release metadata.
//
// App releases have their own TUF repository, beside the mixer's: the same
// trust root and role keys, but separate metadata on the `app-metadata`
// branch. Every asset of a release is a target named `v{VERSION}/{ASSET}`
// whose custom `version` field repeats the version. Velopack's own feed
// says which package to fetch; this says whether the bytes fetched are the
// ones that were signed.
//
// Each check starts from the trusted root with no saved state. Both
// repositories share one root, so mixer metadata served at the app's
// address verifies; saved state would let such a replay raise the app's
// rollback floor and freeze its updates. Without it a replay can only offer
// older metadata, which is acceptable here: the app only checks versions
// newer than itself, which metadata from before their release cannot name,
// and expiry (every role must be unexpired; app metadata lasts 7 days)
// bounds how long stale metadata can hide a newer release.

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/url"
	"os"
	"sort"
	"strings"
	"time"

	"github.com/theupdateframework/go-tuf/v2/metadata"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer/internal/tuf"
)

const (
	appMetadataHost = "raw.githubusercontent.com"
	appTargetsHost  = "github.com"
	appMetadataURL  = "https://raw.githubusercontent.com/ibanks42/rc2fx/app-metadata/"
	appTargetsURL   = "https://github.com/ibanks42/rc2fx/releases/download/"
	// appTimeout covers metadata only: the package itself is already on disk.
	appTimeout = 120 * time.Second

	updateUnsigned = "This update has not been signed yet. Try again later."
	// updateSuperseded: signing lists only the latest release, so an older
	// download disappears from the signed list once a newer release is
	// signed.
	updateSuperseded = "A newer update has replaced this one. Download the update again."
	updateMismatch   = "The downloaded update does not match its signed release and was discarded."
)

// VerifyError is why a package was not accepted.
type VerifyError struct {
	// Rejected: the signed release says this file must not be applied
	// (unsigned, different bytes, a bad name or version, or unreadable);
	// discard it. Otherwise the check could not be made (network, metadata,
	// timeout): the file may still be good, so keep it and try again.
	Rejected bool
	Message  string
}

func (e *VerifyError) Error() string { return e.Message }

func unavailable(message string) *VerifyError { return &VerifyError{Message: message} }

func rejected(message string) *VerifyError { return &VerifyError{Rejected: true, Message: message} }

// AppUpdatesConfigured is false in a checkout without a trust root: there is
// nothing to check updates against, so callers must not offer them.
func AppUpdatesConfigured() bool {
	return string(bytes.TrimSpace(trustedRoot)) != "null"
}

// VerifyUpdatePackage checks that file is exactly the signed asset asset of
// app version. The error is a *VerifyError. Blocking: it fetches the app metadata, so call it from a
// background goroutine. A file whose bytes do not match is removed, so
// nothing can apply it afterwards. The metadata is kept only in memory, for
// this check; nothing is written to disk.
func VerifyUpdatePackage(version, asset, file string) error {
	if err := verifyUpdatePackage(version, asset, file); err != nil {
		return err
	}
	return nil
}

func verifyUpdatePackage(version, asset, file string) *VerifyError {
	if !AppUpdatesConfigured() {
		return unavailable("This build has no trust root for signed app updates.")
	}
	metadataBase, err := url.Parse(appMetadataURL)
	if err != nil {
		return unavailable(err.Error())
	}
	targetsBase, err := url.Parse(appTargetsURL)
	if err != nil {
		return unavailable(err.Error())
	}
	if err := checkAppRepositoryURL(metadataBase, appMetadataHost); err != nil {
		return unavailable(err.Error())
	}
	if err := checkAppRepositoryURL(targetsBase, appTargetsHost); err != nil {
		return unavailable(err.Error())
	}
	return verifyAppPackage(trustedRoot, tufHTTP(metadataBase, targetsBase, appTimeout), version, asset, file)
}

// verifyAppPackage: repository is the official one, or local files in tests.
func verifyAppPackage(root []byte, repository tuf.Repository, version, asset, file string) *VerifyError {
	if !comparable(version) || asset == "" || strings.ContainsAny(asset, `/\`) || strings.HasPrefix(asset, ".") {
		return rejected(fmt.Sprintf("%s %s is not an app update package.", asset, version))
	}
	name := "v" + version + "/" + asset
	target, newest, err := signedTarget(root, repository, name)
	if err != nil {
		return unavailable(err.Error())
	}
	if target == nil {
		order, ok := core.CompareVersions(newest, version)
		if newest != "" && ok && order > 0 {
			return rejected(updateSuperseded)
		}
		return rejected(updateUnsigned)
	}
	if v, ok := versionOf(target); !ok || v != version {
		return rejected("The signed release information does not describe this update.")
	}
	length, sha, err := fileDigest(file)
	if err != nil {
		return rejected(fmt.Sprintf("Could not read the downloaded update: %v", err))
	}
	want, ok := targetSha256(target)
	if !ok || length != uint64(target.Length) || sha != want {
		// Best effort: the caller discards the update either way.
		os.Remove(file)
		return rejected(updateMismatch)
	}
	return nil
}

// signedTarget is the target name from freshly verified metadata, or nil
// when no release with that name has been signed, and the newest version
// signed.
func signedTarget(root []byte, repository tuf.Repository, name string) (*metadata.TargetFiles, string, error) {
	fail := func(e any) error {
		return fmt.Errorf("Could not check this update against its signed release: %v. Try again later.", e)
	}
	// No store: every check starts from the trusted root (see the package
	// comment).
	updater, err := tuf.NewUpdater(repository, root)
	if err != nil {
		return nil, "", fail(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), appTimeout)
	defer cancel()
	// Expired metadata could be a replay that hides a newer release.
	if err := updater.Refresh(ctx, time.Now()); err != nil {
		if errors.Is(ctx.Err(), context.DeadlineExceeded) {
			return nil, "", fail("timed out")
		}
		return nil, "", fail(err)
	}
	if err := distinctKeys(updater.Trusted().Root()); err != nil {
		return nil, "", fail(err)
	}
	targets := updater.Trusted().TargetsRole(metadata.TARGETS)
	if targets == nil {
		return nil, "", fail("the signed metadata is incomplete")
	}
	// Only top-level targets are published. A delegation would extend trust
	// to roles whose lifetimes nothing here checks.
	if d := targets.Delegations; d != nil && (len(d.Roles) > 0 || d.SuccinctRoles != nil) {
		return nil, "", fail("delegated app targets are not supported")
	}
	names := make([]string, 0, len(targets.Targets))
	for n := range targets.Targets {
		names = append(names, n)
	}
	sort.Strings(names)
	newest, have := "", false
	for _, n := range names {
		v, ok := versionOf(targets.Targets[n])
		if !ok {
			continue
		}
		if order, ok := core.CompareVersions(v, newest); !have || (ok && order > 0) {
			newest, have = v, true
		}
	}
	return targets.Targets[name], newest, nil
}

// versionOf is the version a signed asset says it belongs to.
func versionOf(target *metadata.TargetFiles) (string, bool) {
	v, ok := customValue(target, "version").(string)
	return v, ok
}

// fileDigest is the length and SHA-256 of a file, read once in pieces.
func fileDigest(path string) (uint64, string, error) {
	file, err := os.Open(path)
	if err != nil {
		return 0, "", err
	}
	defer file.Close()
	h := sha256.New()
	n, err := io.CopyBuffer(h, file, make([]byte, 1<<16))
	if err != nil {
		return 0, "", err
	}
	return uint64(n), hex.EncodeToString(h.Sum(nil)), nil
}

// checkAppRepositoryURL: the two addresses app metadata may come from, over
// plain HTTPS.
func checkAppRepositoryURL(u *url.URL, host string) error {
	if !officialURL(u, host) {
		return Error("App update repository URLs must use the official HTTPS hosts.")
	}
	return nil
}
