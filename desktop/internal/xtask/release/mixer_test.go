package release

// Publisher-to-client contract using disposable keys and a synthetic
// payload.

import (
	"bytes"
	"fmt"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// fixturePackage lays out a source tree in base with stand-in ARM64
// binaries, and the packaged release 1.2.3 at targets/mixer-v1.2.3, as
// mixer-package writes it. It returns the payload.
func fixturePackage(t *testing.T, base string) []byte {
	t.Helper()
	runtimeDir := filepath.Join(base, "runtime")
	scripts := filepath.Join(base, "mixer", "scripts")
	config := filepath.Join(base, "config")
	elf := make([]byte, 96)
	copy(elf, []byte{127, 'E', 'L', 'F', 2, 1})
	elf[18] = 0xb7
	for _, name := range []string{
		"librc2fx_native.so", "rc2fxd", "rc2fx-supervisor", "rc2fx-firmware",
		"librc2fx_gui.so", "librc2fx_jackerr.so", "rc2fx_jackerr_snapshot",
	} {
		mustWrite(t, filepath.Join(runtimeDir, name), elf)
	}
	catalog := mustRead(t, filepath.Join(task.Root(), "config", "firmware-profiles.json"))
	for name, data := range map[string][]byte{
		"firmware-profiles.json": catalog,
		"mixer-release.json":     []byte(`{"schemaVersion":1,"version":"1.2.3","protocolVersion":1}`),
	} {
		mustWrite(t, filepath.Join(runtimeDir, name), data)
		mustWrite(t, filepath.Join(config, name), data)
	}
	var sums strings.Builder
	for _, name := range []string{
		"librc2fx_native.so", "rc2fxd", "rc2fx-supervisor", "rc2fx-firmware", "firmware-profiles.json",
		"librc2fx_gui.so", "librc2fx_jackerr.so", "rc2fx_jackerr_snapshot", "mixer-release.json",
	} {
		fmt.Fprintf(&sums, "%s  %s\n", task.Sha256(mustRead(t, filepath.Join(runtimeDir, name))), name)
	}
	mustWrite(t, filepath.Join(runtimeDir, "SHA256SUMS"), []byte(sums.String()))
	for _, name := range []string{"rc2fx.sh", "rc2fx-shim.sh", "rc2fx-gui-shim.sh", "install.sh"} {
		mustWrite(t, filepath.Join(scripts, name), []byte("#!/bin/sh\nrc2fx_build=development\n"))
	}
	envelope, err := embed.Expected(base, true)
	if err != nil {
		t.Fatal(err)
	}
	pkg := filepath.Join(base, "targets", "mixer-v1.2.3")
	mustWrite(t, filepath.Join(pkg, "rc2fx-mixer-1.2.3.payload"), envelope.AsBytes())
	mustWrite(t, filepath.Join(pkg, "firmware-profiles.json"), catalog)
	mustWrite(t, filepath.Join(pkg, "mixer-compatibility.json"), []byte(`{"minimumAppVersion":"1.0.1"}`))
	mustWrite(t, filepath.Join(pkg, "THIRD-PARTY-NOTICES.html"), []byte("fixture notices"))
	return bytes.Clone(envelope.AsBytes())
}

func TestSigningOutputVerifiesAndPreservesTheInstallerPayloadContract(t *testing.T) {
	base := t.TempDir()
	payload := fixturePackage(t, base)
	testRoot(t, base)
	expiry := weekAhead()
	arguments := func(out, targets string) []string {
		return []string{
			filepath.Join(base, "root.json"),
			filepath.Join(base, "targets", "mixer-v1.2.3"),
			filepath.Join(base, out),
			"1",
			expiry,
			filepath.Join(base, targets),
			filepath.Join(base, "snapshot.pk8"),
			filepath.Join(base, "timestamp.pk8"),
		}
	}
	if err := Sign(arguments("rejected", "root.pk8")); err == nil || !strings.Contains(err.Error(), "root private keys") {
		t.Fatalf("%v", err)
	}
	if exists(filepath.Join(base, "rejected")) {
		t.Fatal("rejected output written")
	}
	late := arguments("rejected", "targets.pk8")
	late[4] = "2099-01-01T00:00:00Z"
	if err := Sign(late); err == nil || !strings.Contains(err.Error(), "expire before") {
		t.Fatalf("%v", err)
	}
	if exists(filepath.Join(base, "rejected")) {
		t.Fatal("rejected output written")
	}
	if err := Sign(arguments("metadata", "targets.pk8")); err != nil {
		t.Fatal(err)
	}
	rootFile := mustRead(t, filepath.Join(base, "root.json"))
	repository, err := loadRepository(rootFile, filepath.Join(base, "metadata"), filepath.Join(base, "targets"), time.Now())
	if err != nil {
		t.Fatal(err)
	}
	if !repository.Targets.Expires.Equal(repository.Root.Expires) {
		t.Fatal("targets last as long as the root")
	}
	if !repository.Timestamp.Expires.Before(repository.Targets.Expires) || !repository.Snapshot.Expires.Before(repository.Targets.Expires) {
		t.Fatal("discovery metadata must expire before targets")
	}
	name := "mixer-v1.2.3/rc2fx-mixer-1.2.3.payload"
	verified, err := repository.ReadTarget(name)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(verified, payload) {
		t.Fatal("payload differs")
	}
	envelope, err := embed.Standalone(verified)
	if err != nil {
		t.Fatal(err)
	}
	metadata, err := installer.MixerTargetForPayload(envelope, "1.0.1")
	if err != nil {
		t.Fatal(err)
	}
	signedMixer, _ := repository.Targets.Entries[name].Custom.Get("rc2fxMixer")
	if !signedMixer.Equal(MixerTargetValue(metadata)) {
		t.Fatalf("%s", signedMixer.String())
	}
	catalogName := "mixer-v1.2.3/firmware-profiles.json"
	catalog, err := repository.ReadTarget(catalogName)
	if err != nil {
		t.Fatal(err)
	}
	bundle, err := envelope.Bundle()
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(catalog, bundle["firmware-profiles.json"]) {
		t.Fatal("catalog differs")
	}
	if v, _ := repository.Targets.Entries[catalogName].Custom.Get("rc2fxCatalog"); !v.Equal(core.StringValue("1.2.3")) {
		t.Fatalf("rc2fxCatalog %s", v.String())
	}
	mustWrite(t, filepath.Join(base, "targets", "mixer-v1.2.3", "rc2fx-mixer-1.2.3.payload"), []byte("tampered"))
	if _, err := repository.ReadTarget(name); err == nil {
		t.Fatal("a tampered payload was read")
	}
	// Restore the payload so this failure specifically exercises catalog
	// binding.
	mustWrite(t, filepath.Join(base, "targets", "mixer-v1.2.3", "rc2fx-mixer-1.2.3.payload"), payload)
	mustWrite(t, filepath.Join(base, "targets", "mixer-v1.2.3", "firmware-profiles.json"), []byte("{}"))
	if err := Sign(arguments("bad-catalog", "targets.pk8")); err == nil || !strings.Contains(err.Error(), "catalog differs") {
		t.Fatalf("%v", err)
	}
	if exists(filepath.Join(base, "bad-catalog")) {
		t.Fatal("bad-catalog output written")
	}
	if runtime.GOOS == "windows" {
		return
	}
	// Exercise the exact PEM files emitted by the user-facing key tool,
	// through release signing and the client's TUF verifier.
	keys := filepath.Join(base, "generated-keys")
	if err := createKeys(keys, task.Root()); err != nil {
		t.Fatal(err)
	}
	mustWrite(t, filepath.Join(base, "targets", "mixer-v1.2.3", "firmware-profiles.json"),
		mustRead(t, filepath.Join(task.Root(), "config", "firmware-profiles.json")))
	if err := Sign([]string{
		filepath.Join(keys, "public", "root.json"),
		filepath.Join(base, "targets", "mixer-v1.2.3"),
		filepath.Join(base, "pem-metadata"),
		"2",
		expiry,
		filepath.Join(keys, "release", "targets.pem"),
		filepath.Join(keys, "release", "snapshot.pem"),
		filepath.Join(keys, "release", "timestamp.pem"),
	}); err != nil {
		t.Fatal(err)
	}
	pemRoot := mustRead(t, filepath.Join(keys, "public", "root.json"))
	pemRepository, err := loadRepository(pemRoot, filepath.Join(base, "pem-metadata"), filepath.Join(base, "targets"), time.Now())
	if err != nil {
		t.Fatal(err)
	}
	data, err := pemRepository.ReadTarget(name)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := embed.Standalone(data); err != nil {
		t.Fatal(err)
	}
}
