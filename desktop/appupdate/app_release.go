package appupdate

// App releases use a separate TUF repo on the app-metadata branch that shares
// the mixer trust root and role keys. Each release asset is a target named
// "v{VERSION}/{ASSET}" with a matching custom "version" field. Velopack's feed
// picks which package to download; this verifies the downloaded bytes against
// signed metadata.
//
// Verification is intentionally stateless (starts from the embedded root every
// time). Because app and mixer repos share a root, mixer metadata served at the
// app URL would verify; persisting TUF state would let a replay bump the app's
// rollback floor and freeze updates. Without saved state, a replay can only
// serve older metadata, which can't name versions newer than the running app,
// and the 7-day role expiry bounds how long stale metadata can hide a release.

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
	"github.com/ibanks42/rc2fx/desktop/installer/trust"
	"github.com/ibanks42/rc2fx/desktop/internal/tuf"
)

var trustedRoot = trust.Root()

const (
	appMetadataHost = "raw.githubusercontent.com"
	appTargetsHost  = "github.com"
	appMetadataURL  = "https://raw.githubusercontent.com/ibanks42/rc2fx/app-metadata/"
	appTargetsURL   = "https://github.com/ibanks42/rc2fx/releases/download/"
	// Covers metadata fetches only; the package is already on disk.
	appTimeout = 120 * time.Second

	updateUnsigned = "This update has not been signed yet. Try again later."
	// Only the latest release is signed, so older downloads disappear once a
	// newer release is published.
	updateSuperseded = "A newer update has replaced this one. Download the update again."
	updateMismatch   = "The downloaded update does not match its signed release and was discarded."
)

// VerifyError describes why an update package failed verification.
type VerifyError struct {
	// Rejected is true when the file must not be applied (unsigned, hash
	// mismatch, invalid name/version, unreadable) and should be discarded.
	// False means verification couldn't complete (network, metadata, timeout)
	// and can be retried later.
	Rejected bool
	Message  string
}

func (e *VerifyError) Error() string { return e.Message }

func unavailable(message string) *VerifyError { return &VerifyError{Message: message} }

func rejected(message string) *VerifyError { return &VerifyError{Rejected: true, Message: message} }

// AppUpdatesConfigured reports whether this build has a trust root configured.
// Without one, callers must not offer app updates.
func AppUpdatesConfigured() bool {
	return string(bytes.TrimSpace(trustedRoot)) != "null"
}

// VerifyUpdatePackage checks file against the signed TUF target for version and
// asset, returning a *VerifyError on failure. It blocks on metadata fetches, so
// call it off the UI thread. Mismatched files are deleted; metadata stays in
// memory only.
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
	return verifyAppPackage(trustedRoot, tuf.NewHTTPRepository(metadataBase, targetsBase, appTimeout), version, asset, file)
}

func verifyAppPackage(root []byte, repository tuf.Repository, version, asset, file string) *VerifyError {
	if !core.ValidVersion(version) || asset == "" || strings.ContainsAny(asset, `/\`) || strings.HasPrefix(asset, ".") {
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
	want, ok := tuf.TargetSHA256(target)
	if !ok || length != uint64(target.Length) || sha != want {
		// Best-effort cleanup; caller discards the update either way.
		os.Remove(file)
		return rejected(updateMismatch)
	}
	return nil
}

// signedTarget fetches fresh metadata and returns the named target (or nil if
// absent) alongside the newest signed version found.
func signedTarget(root []byte, repository tuf.Repository, name string) (*metadata.TargetFiles, string, error) {
	fail := func(e any) error {
		return fmt.Errorf("Could not check this update against its signed release: %v. Try again later.", e)
	}
	// Stateless by design; see package comment.
	updater, err := tuf.NewUpdater(repository, root)
	if err != nil {
		return nil, "", fail(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), appTimeout)
	defer cancel()
	if err := updater.Refresh(ctx, time.Now()); err != nil {
		if errors.Is(ctx.Err(), context.DeadlineExceeded) {
			return nil, "", fail("timed out")
		}
		return nil, "", fail(err)
	}
	if err := tuf.DistinctKeys(updater.Trusted().Root()); err != nil {
		return nil, "", fail(err)
	}
	targets := updater.Trusted().TargetsRole(metadata.TARGETS)
	if targets == nil {
		return nil, "", fail("the signed metadata is incomplete")
	}
	// Reject delegations since we don't validate delegated role lifetimes.
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

func versionOf(target *metadata.TargetFiles) (string, bool) {
	v, ok := tuf.CustomValue(target, "version").(string)
	return v, ok
}

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

func checkAppRepositoryURL(u *url.URL, host string) error {
	if !tuf.OfficialURL(u, host) {
		return errors.New("App update repository URLs must use the official HTTPS hosts.")
	}
	return nil
}
