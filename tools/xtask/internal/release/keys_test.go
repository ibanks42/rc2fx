//go:build unix

package release

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/tufsign"
)

func TestCreatesSeparatePrivateKeysAndVerifiedPublicRootWithoutOverwriting(t *testing.T) {
	temp := t.TempDir()
	checkout := filepath.Join(temp, "checkout")
	if err := os.Mkdir(checkout, 0o777); err != nil {
		t.Fatal(err)
	}
	output := filepath.Join(temp, "keys")
	if err := createKeys(output, checkout); err != nil {
		t.Fatal(err)
	}
	data := mustRead(t, filepath.Join(output, "public", "root.json"))
	root, err := tufsign.ParseRoot(data)
	if err != nil {
		t.Fatal(err)
	}
	if err := root.VerifyRole(root.Document); err != nil {
		t.Fatal(err)
	}
	if root.ConsistentSnapshot || len(root.Keys) != 4 || len(root.Signatures) != 1 {
		t.Fatalf("consistent %v, %d keys, %d signatures", root.ConsistentSnapshot, len(root.Keys), len(root.Signatures))
	}
	if !root.Expires.After(time.Now().Add(365 * 86400 * time.Second)) {
		t.Fatalf("expires %v", root.Expires)
	}
	ids := map[string]bool{}
	for _, role := range allRoles {
		settings := root.Roles[role]
		if settings.Threshold != 1 || ids[settings.KeyIDs[0]] {
			t.Fatalf("%s: %+v", role, settings)
		}
		ids[settings.KeyIDs[0]] = true
		folder := "release"
		if role == tufsign.RoleRoot {
			folder = "offline"
		}
		file := filepath.Join(output, folder, role+".pem")
		info, err := os.Stat(file)
		if err != nil || info.Mode().Perm() != 0o600 {
			t.Fatalf("%s: %v %v", file, info, err)
		}
		if !strings.HasPrefix(string(mustRead(t, file)), "-----BEGIN PRIVATE KEY-----") {
			t.Fatalf("%s is not PKCS #8 PEM", file)
		}
	}
	if info, err := os.Stat(output); err != nil || info.Mode().Perm() != 0o700 {
		t.Fatalf("%v %v", info, err)
	}
	if _, err := os.Stat(filepath.Join(output, "COMPLETE")); err != nil {
		t.Fatal(err)
	}
	if createKeys(output, checkout) == nil {
		t.Fatal("an existing key directory was reused")
	}
	if !bytes.Equal(mustRead(t, filepath.Join(output, "public", "root.json")), data) {
		t.Fatal("root.json changed")
	}
	if entries, _ := os.ReadDir(checkout); len(entries) != 0 {
		t.Fatal("the checkout changed")
	}
}

func TestRefusesCheckoutPathsRelativePathsAndSymlinkDestinations(t *testing.T) {
	temp := t.TempDir()
	checkout := filepath.Join(temp, "checkout")
	if err := os.Mkdir(checkout, 0o777); err != nil {
		t.Fatal(err)
	}
	if createKeys(filepath.Join(checkout, "keys"), checkout) == nil {
		t.Fatal("inside the checkout")
	}
	if createKeys("relative-keys", checkout) == nil {
		t.Fatal("relative")
	}
	alias := filepath.Join(temp, "alias")
	if err := os.Symlink(checkout, alias); err != nil {
		t.Fatal(err)
	}
	if createKeys(filepath.Join(alias, "keys"), checkout) == nil {
		t.Fatal("through a link into the checkout")
	}
	if createKeys(alias, checkout) == nil {
		t.Fatal("an existing link")
	}
	if _, err := os.Stat(filepath.Join(checkout, "keys")); err == nil {
		t.Fatal("keys were written into the checkout")
	}
}
