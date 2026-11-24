package release

import (
	"crypto/ed25519"
	"crypto/rand"
	"crypto/x509"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/tufsign"
)

var allRoles = []string{tufsign.RoleRoot, tufsign.RoleTargets, tufsign.RoleSnapshot, tufsign.RoleTimestamp}

func mustTime(t *testing.T, text string) time.Time {
	t.Helper()
	at, err := tufsign.ParseTimestamp(text)
	if err != nil {
		t.Fatal(err)
	}
	return at
}

// testRoot generates disposable Ed25519 keys (ROLE.pk8) and a root.json
// expiring in 2099 inside dir.
func testRoot(t *testing.T, dir string) map[string]tufsign.Signer {
	t.Helper()
	signers := map[string]tufsign.Signer{}
	var keys []tufsign.Key
	for _, role := range allRoles {
		_, private, err := ed25519.GenerateKey(rand.Reader)
		if err != nil {
			t.Fatal(err)
		}
		der, err := x509.MarshalPKCS8PrivateKey(private)
		if err != nil {
			t.Fatal(err)
		}
		path := filepath.Join(dir, role+".pk8")
		if err := os.WriteFile(path, der, 0o600); err != nil {
			t.Fatal(err)
		}
		signer, err := tufsign.LoadKey(path)
		if err != nil {
			t.Fatal(err)
		}
		signers[role] = signer
		keys = append(keys, signer.Key())
	}
	signed, err := tufsign.NewRoot(mustTime(t, "2099-01-01T00:00:00Z"), allRoles, keys)
	if err != nil {
		t.Fatal(err)
	}
	root, err := tufsign.RootFromSigned(signed)
	if err != nil {
		t.Fatal(err)
	}
	var all []tufsign.Signer
	for _, role := range allRoles {
		all = append(all, signers[role])
	}
	buffer, err := root.Sign(tufsign.RoleRoot, signed, all)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "root.json"), buffer, 0o666); err != nil {
		t.Fatal(err)
	}
	return signers
}

func weekAhead() string {
	return tufsign.DisplayTimestamp(time.Now().Add(7 * 86400 * time.Second))
}

func mustRead(t *testing.T, path string) []byte {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func mustWrite(t *testing.T, path string, data []byte) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o777); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, data, 0o666); err != nil {
		t.Fatal(err)
	}
}
