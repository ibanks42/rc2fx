package release

import (
	"bytes"
	"crypto/sha256"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/licenses"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/tufsign"
)

var sha256Sum = sha256.Sum256

const mixerSignUsage = "usage: ./xtask mixer-sign ROOT PACKAGE OUT SEQUENCE EXPIRES TARGETS_KEY SNAPSHOT_KEY TIMESTAMP_KEY"

// Package is `mixer-package DIR`: the exact standalone bytes that the signed
// update repository serves, in a new directory.
func Package(destination string) error {
	return packageIn(task.Root(), destination, licenses.WriteLicenses)
}

func packageIn(root, destination string, writeLicenses func(string) error) error {
	// Create a new directory: never replace an already packaged release.
	envelope, err := embed.Expected(root, true)
	if err != nil {
		return asTask(err)
	}
	checked, err := embed.Standalone(envelope.AsBytes())
	if err != nil {
		return asTask(err)
	}
	bundle, err := checked.Bundle()
	if err != nil {
		return asTask(err)
	}
	manifest, err := core.ParseJSONBytes(bundle["mixer-release.json"], core.DefaultMaxJSONBytes, core.DefaultMaxJSONNodes)
	if err != nil {
		return asTask(err)
	}
	versionValue, _ := manifest.Get("version")
	version, ok := versionValue.AsString()
	if !ok {
		return task.Error("missing mixer version")
	}
	name := "rc2fx-mixer-" + version + ".payload"
	parent := filepath.Dir(destination)
	if err := os.MkdirAll(parent, 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	if exists(destination) {
		return task.Errorf("%s already exists; use a fresh output directory", destination)
	}
	staging, err := tufsign.StageDir(parent)
	if err != nil {
		return err
	}
	moved := false
	defer func() {
		if !moved {
			_ = os.RemoveAll(staging)
		}
	}()
	write := func(file string, data []byte) error {
		if err := os.WriteFile(filepath.Join(staging, file), data, 0o666); err != nil {
			return task.Error(task.IOError(err))
		}
		return nil
	}
	if err := write(name, checked.AsBytes()); err != nil {
		return err
	}
	if err := write("firmware-profiles.json", bundle["firmware-profiles.json"]); err != nil {
		return err
	}
	compatibility, err := readAll(filepath.Join(root, "config", "mixer-compatibility.json"))
	if err != nil {
		return err
	}
	if err := write("mixer-compatibility.json", compatibility); err != nil {
		return err
	}
	if err := writeLicenses(filepath.Join(staging, "THIRD-PARTY-NOTICES.html")); err != nil {
		return err
	}
	notices, err := readAll(filepath.Join(staging, "THIRD-PARTY-NOTICES.html"))
	if err != nil {
		return err
	}
	sums := fmt.Sprintf("%s  %s\n%s  THIRD-PARTY-NOTICES.html\n%s  firmware-profiles.json\n",
		task.Sha256(checked.AsBytes()), name, task.Sha256(notices), task.Sha256(bundle["firmware-profiles.json"]))
	if err := write("SHA256SUMS", []byte(sums)); err != nil {
		return err
	}
	if err := os.Rename(staging, destination); err != nil {
		return task.Error(task.IOError(err))
	}
	moved = true
	fmt.Printf("Packaged mixer %s: %s (unsigned; signing is required before distribution)\n", version, destination)
	return nil
}

// loadRoot reads, parses and verifies a root file; readErr words a read
// failure.
func loadRoot(path string, readErr func(error) error) (*tufsign.Root, []byte, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, nil, readErr(err)
	}
	root, err := tufsign.ParseRoot(data)
	if err != nil {
		return nil, nil, asTask(err)
	}
	if err := root.VerifyRole(root.Document); err != nil {
		return nil, nil, asTask(err)
	}
	return root, data, nil
}

// releaseKeys loads release-role keys, refusing a root key or a key the
// root does not authorize for its role.
func releaseKeys(root *tufsign.Root, roles [3][2]string) ([]tufsign.Signer, error) {
	var signers []tufsign.Signer
	for _, pair := range roles {
		role, path := pair[0], pair[1]
		signer, err := tufsign.LoadKey(path)
		if err != nil {
			return nil, asTask(err)
		}
		id, err := tufsign.SignerID(signer)
		if err != nil {
			return nil, asTask(err)
		}
		rootKeys, ok := root.Roles[tufsign.RoleRoot]
		if !ok {
			return nil, task.Error("root role missing")
		}
		for _, rootID := range rootKeys.KeyIDs {
			if rootID == id {
				return nil, task.Error("root private keys must never be supplied to release signing")
			}
		}
		if !root.RoleHas(role, id) {
			return nil, task.Errorf("key is not authorized for %s", tufsign.RoleDebug(role))
		}
		signers = append(signers, signer)
	}
	return signers, nil
}

// MixerTargetValue is the signed claims as serde writes MixerTarget.
func MixerTargetValue(m *installer.MixerTarget) core.Value {
	profiles := make([]core.Value, len(m.FirmwareProfiles))
	for i, p := range m.FirmwareProfiles {
		profiles[i] = core.StringValue(p)
	}
	return core.ObjectValue(
		core.Member{Key: "version", Value: core.StringValue(m.Version)},
		core.Member{Key: "payloadFormat", Value: core.U64Value(uint64(m.PayloadFormat))},
		core.Member{Key: "protocolVersion", Value: core.U64Value(uint64(m.ProtocolVersion))},
		core.Member{Key: "minimumAppVersion", Value: core.StringValue(m.MinimumAppVersion)},
		core.Member{Key: "firmwareProfiles", Value: core.ArrayValue(profiles...)},
	)
}

// Sign is `mixer-sign`: sign only a fully packaged, checked release. Key
// creation and root changes are deliberately separate from this operation
// and from the build worker.
func Sign(args []string) error {
	if len(args) != 8 {
		return task.Error(mixerSignUsage)
	}
	rootFile, packageDir, out, sequenceText, expiresText := args[0], args[1], args[2], args[3], args[4]
	targetsKey, snapshotKey, timestampKey := args[5], args[6], args[7]
	sequence, ok := task.ParseUint(sequenceText, 64)
	if !ok || sequence == 0 {
		return task.Error("sequence must be positive")
	}
	expires, err := tufsign.ParseTimestamp(expiresText)
	if err != nil {
		return task.Errorf("invalid expiry: %s", err)
	}
	if !expires.After(time.Now()) {
		return task.Error("metadata expiry must be in the future")
	}
	root, rootBytes, err := loadRoot(rootFile, func(err error) error { return task.Error(task.IOError(err)) })
	if err != nil {
		return err
	}
	// A release stays authorized for as long as the root trusts its key, so
	// nothing needs signing with the targets key between releases.
	targetsExpires := root.Expires
	if !expires.Before(targetsExpires) {
		return task.Error("discovery metadata must expire before the targets and root")
	}
	if root.ConsistentSnapshot {
		return task.Error("GitHub release target paths require consistent_snapshot=false")
	}
	signers, err := releaseKeys(root, [3][2]string{
		{tufsign.RoleTargets, targetsKey},
		{tufsign.RoleSnapshot, snapshotKey},
		{tufsign.RoleTimestamp, timestampKey},
	})
	if err != nil {
		return err
	}
	entries, err := os.ReadDir(packageDir)
	if err != nil {
		return task.Error(task.IOError(err))
	}
	var payloads []string
	for _, entry := range entries {
		if extension(entry.Name()) == "payload" {
			payloads = append(payloads, filepath.Join(packageDir, entry.Name()))
		}
	}
	if len(payloads) != 1 {
		return task.Error("package must contain exactly one mixer payload")
	}
	data, err := readAll(payloads[0])
	if err != nil {
		return err
	}
	standalone, err := embed.Standalone(data)
	if err != nil {
		return asTask(err)
	}
	bundle, err := standalone.Bundle()
	if err != nil {
		return asTask(err)
	}
	manifest, err := core.ParseMixerRelease(bundle["mixer-release.json"])
	if err != nil {
		return asTask(err)
	}
	compatibilityData, err := readAll(filepath.Join(packageDir, "mixer-compatibility.json"))
	if err != nil {
		return err
	}
	compatibility, err := core.ParseJSONBytes(compatibilityData, core.DefaultMaxJSONBytes, core.DefaultMaxJSONNodes)
	if err != nil {
		return asTask(err)
	}
	minimumValue, _ := compatibility.Get("minimumAppVersion")
	minimumApp, ok := minimumValue.AsString()
	if !ok {
		return task.Error("minimumAppVersion is required in mixer-compatibility.json")
	}
	again, err := embed.Standalone(data)
	if err != nil {
		return asTask(err)
	}
	metadata, err := installer.MixerTargetForPayload(again, minimumApp)
	if err != nil {
		return asTask(err)
	}
	if err := root.CheckStable(); err != nil {
		return err
	}
	catalog, err := readAll(filepath.Join(packageDir, "firmware-profiles.json"))
	if err != nil {
		return err
	}
	if !bytes.Equal(catalog, bundle["firmware-profiles.json"]) {
		return task.Error("standalone catalog differs from the mixer payload")
	}
	var targets []tufsign.Target
	for _, file := range []struct {
		path  string
		mixer bool
	}{
		{payloads[0], true},
		{filepath.Join(packageDir, "THIRD-PARTY-NOTICES.html"), false},
		{filepath.Join(packageDir, "firmware-profiles.json"), false},
	} {
		target, err := tufsign.BuildTarget(file.path)
		if err != nil {
			return err
		}
		if file.mixer {
			target.Custom = append(target.Custom, core.Member{Key: "rc2fxMixer", Value: MixerTargetValue(metadata)})
		}
		if filepath.Base(file.path) == "firmware-profiles.json" {
			target.Custom = append(target.Custom, core.Member{Key: "rc2fxCatalog", Value: core.StringValue(manifest.Version)})
		}
		target.Name = "mixer-v" + manifest.Version + "/" + target.Name
		targets = append(targets, target)
	}
	signed, err := tufsign.SignRelease(tufsign.Release{
		RootFile: rootBytes, Root: root,
		TargetsVersion: sequence, TargetsExpires: targetsExpires,
		SnapshotVersion: sequence, SnapshotExpires: expires,
		TimestampVersion: sequence, TimestampExpires: expires,
		Targets: targets,
	}, signers)
	if err != nil {
		return err
	}
	if exists(out) {
		return task.Error("metadata output already exists; use a new directory")
	}
	if err := writeStaged(out, signed); err != nil {
		return err
	}
	fmt.Printf("Signed mixer %s metadata sequence %s; expires %s\n",
		manifest.Version, strconv.FormatUint(sequence, 10), tufsign.DisplayTimestamp(expires))
	return nil
}

// writeStaged writes a signed repository to a private directory beside out
// and renames it to out.
func writeStaged(out string, signed *tufsign.Repository) error {
	parent := outputParent(out)
	if err := os.MkdirAll(parent, 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	stage, err := tufsign.StageDir(parent)
	if err != nil {
		return err
	}
	if err := signed.Write(stage); err != nil {
		_ = os.RemoveAll(stage)
		return err
	}
	if err := os.Rename(stage, out); err != nil {
		_ = os.RemoveAll(stage)
		return task.Error(task.IOError(err))
	}
	return nil
}

// extension is Path::extension: what follows the last dot of a name that
// does not start with its only dot.
func extension(name string) string {
	at := -1
	for i := len(name) - 1; i > 0; i-- {
		if name[i] == '.' {
			at = i
			break
		}
	}
	if at <= 0 {
		return ""
	}
	return name[at+1:]
}
