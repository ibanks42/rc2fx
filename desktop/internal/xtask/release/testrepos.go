package release

import (
	"crypto/ed25519"
	"crypto/sha256"
	"crypto/x509"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/tufsign"
)

const (
	testReleaseDir     = "targets/mixer-v1.2.3"
	testEd25519Root    = "2099-01-01T00:00:00Z"
	testEd25519Expiry  = "2098-01-01T00:00:00Z"
	testEd25519Renewal = "2097-06-01T00:00:00Z"
)

var testAllRoles = []string{tufsign.RoleRoot, tufsign.RoleTargets, tufsign.RoleSnapshot, tufsign.RoleTimestamp}

func writeBytes(path string, data []byte) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	return os.WriteFile(path, data, 0o644)
}

// BuildTestPackage populates outDir/targets/mixer-v1.2.3 with a synthetic
// packaged mixer release (placeholder ARM64 ELF binaries, current firmware
// catalog, compatibility manifest, and notices) matching `./xtask mixer-package`.
func BuildTestPackage(outDir string) ([]byte, error) {
	base, err := os.MkdirTemp("", "rc2fx-test-pkg-*")
	if err != nil {
		return nil, err
	}
	defer os.RemoveAll(base)

	runtimeDir := filepath.Join(base, "runtime")
	scripts := filepath.Join(base, "mixer", "scripts")
	config := filepath.Join(base, "config")
	elf := make([]byte, 96)
	copy(elf, []byte{127, 'E', 'L', 'F', 2, 1})
	elf[18] = 0xb7
	for _, name := range embed.Binaries {
		if err := writeBytes(filepath.Join(runtimeDir, name), elf); err != nil {
			return nil, err
		}
	}
	catalog, err := os.ReadFile(filepath.Join(task.Root(), "config", "firmware-profiles.json"))
	if err != nil {
		return nil, err
	}
	for name, data := range map[string][]byte{
		"firmware-profiles.json": catalog,
		"mixer-release.json":     []byte(`{"schemaVersion":1,"version":"1.2.3","protocolVersion":1}`),
	} {
		if err := writeBytes(filepath.Join(runtimeDir, name), data); err != nil {
			return nil, err
		}
		if err := writeBytes(filepath.Join(config, name), data); err != nil {
			return nil, err
		}
	}
	var sums strings.Builder
	for _, name := range embed.Pinned {
		data, err := os.ReadFile(filepath.Join(runtimeDir, name))
		if err != nil {
			return nil, err
		}
		fmt.Fprintf(&sums, "%s  %s\n", task.Sha256(data), name)
	}
	if err := writeBytes(filepath.Join(runtimeDir, "SHA256SUMS"), []byte(sums.String())); err != nil {
		return nil, err
	}
	for _, name := range embed.Scripts {
		if err := writeBytes(filepath.Join(scripts, name), []byte("#!/bin/sh\nrc2fx_build=development\n")); err != nil {
			return nil, err
		}
	}
	envelope, err := embed.Expected(base, true)
	if err != nil {
		return nil, err
	}
	pkg := filepath.Join(outDir, testReleaseDir)
	for name, data := range map[string][]byte{
		"rc2fx-mixer-1.2.3.payload": envelope.AsBytes(),
		"firmware-profiles.json":    catalog,
		"mixer-compatibility.json":  []byte(`{"minimumAppVersion":"1.0.1"}`),
		"THIRD-PARTY-NOTICES.html":  []byte("fixture notices"),
	} {
		if err := writeBytes(filepath.Join(pkg, name), data); err != nil {
			return nil, err
		}
	}
	return envelope.AsBytes(), nil
}

// SignTestEd25519 signs targetsDir/targets/mixer-v1.2.3 deterministically with
// fixed test-only Ed25519 keys into outDir ({root.json, metadata/, refreshed/}).
func SignTestEd25519(targetsRoot, outDir string) error {
	keys, err := os.MkdirTemp("", "rc2fx-test-ed25519-*")
	if err != nil {
		return err
	}
	defer os.RemoveAll(keys)

	signers := map[string]tufsign.Signer{}
	var public []tufsign.Key
	for _, role := range testAllRoles {
		seed := sha256.Sum256([]byte("rc2fx xtask fixture " + role))
		der, err := x509.MarshalPKCS8PrivateKey(ed25519.NewKeyFromSeed(seed[:]))
		if err != nil {
			return err
		}
		path := filepath.Join(keys, role+".pk8")
		if err := writeBytes(path, der); err != nil {
			return err
		}
		signer, err := tufsign.LoadKey(path)
		if err != nil {
			return err
		}
		signers[role] = signer
		public = append(public, signer.Key())
	}
	rootExpiry, err := time.Parse(time.RFC3339, testEd25519Root)
	if err != nil {
		return err
	}
	signed, err := tufsign.NewRoot(rootExpiry, testAllRoles, public)
	if err != nil {
		return err
	}
	root, err := tufsign.RootFromSigned(signed)
	if err != nil {
		return err
	}
	rootFile, err := root.Sign(tufsign.RoleRoot, signed, []tufsign.Signer{signers[tufsign.RoleRoot]})
	if err != nil {
		return err
	}
	if err := writeBytes(filepath.Join(outDir, "root.json"), rootFile); err != nil {
		return err
	}
	key := func(role string) string { return filepath.Join(keys, role+".pk8") }
	if err := Sign([]string{
		filepath.Join(outDir, "root.json"), filepath.Join(targetsRoot, testReleaseDir), filepath.Join(outDir, "metadata"),
		"1", testEd25519Expiry, key(tufsign.RoleTargets), key(tufsign.RoleSnapshot), key(tufsign.RoleTimestamp),
	}); err != nil {
		return err
	}
	return Refresh([]string{
		filepath.Join(outDir, "root.json"), filepath.Join(outDir, "metadata"), filepath.Join(outDir, "refreshed"),
		"2", testEd25519Renewal, key(tufsign.RoleSnapshot), key(tufsign.RoleTimestamp),
	})
}

// SignTestRSA signs targetsRoot/targets/mixer-v1.2.3 with fresh RSA-3072 keys
// and fresh expiry timestamps into outDir ({root.json, metadata/, refreshed/}).
func SignTestRSA(targetsRoot, outDir string) error {
	tmp, err := os.MkdirTemp("", "rc2fx-test-rsa-*")
	if err != nil {
		return err
	}
	defer os.RemoveAll(tmp)

	keys := filepath.Join(tmp, "keys")
	if err := createKeys(keys, task.Root()); err != nil {
		return err
	}
	rootBytes, err := os.ReadFile(filepath.Join(keys, "public", "root.json"))
	if err != nil {
		return err
	}
	if err := writeBytes(filepath.Join(outDir, "root.json"), rootBytes); err != nil {
		return err
	}
	key := func(role string) string { return filepath.Join(keys, "release", role+".pem") }
	if err := Sign([]string{
		filepath.Join(outDir, "root.json"), filepath.Join(targetsRoot, testReleaseDir), filepath.Join(outDir, "metadata"),
		"1", tufsign.DisplayTimestamp(time.Now().Add(7 * 86400 * time.Second)),
		key(tufsign.RoleTargets), key(tufsign.RoleSnapshot), key(tufsign.RoleTimestamp),
	}); err != nil {
		return err
	}
	return Refresh([]string{
		filepath.Join(outDir, "root.json"), filepath.Join(outDir, "metadata"), filepath.Join(outDir, "refreshed"),
		"2", tufsign.DisplayTimestamp(time.Now().Add(6 * 86400 * time.Second)),
		key(tufsign.RoleSnapshot), key(tufsign.RoleTimestamp),
	})
}

// GenerateTestRepos builds the synthetic mixer-v1.2.3 package and signs
// ed25519/ (and optionally rsa/) TUF repositories under outDir.
func GenerateTestRepos(outDir string, includeRSA bool) error {
	if _, err := BuildTestPackage(outDir); err != nil {
		return err
	}
	if err := SignTestEd25519(outDir, filepath.Join(outDir, "ed25519")); err != nil {
		return err
	}
	if includeRSA {
		if err := SignTestRSA(outDir, filepath.Join(outDir, "rsa")); err != nil {
			return err
		}
	}
	return nil
}
