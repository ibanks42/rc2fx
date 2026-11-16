package selfupdate

import (
	"os"
	"path/filepath"
	"testing"
)

func TestThePackagesFolderIsPrivate(t *testing.T) {
	dir := t.TempDir()
	packages := filepath.Join(dir, "rc2fx/velopack-packages")
	if err := EnsurePrivate(packages); err != nil {
		t.Fatal(err)
	}
	mode := func(p string) os.FileMode {
		info, err := os.Stat(p)
		if err != nil {
			t.Fatal(err)
		}
		return info.Mode().Perm()
	}
	if mode(packages) != 0o700 {
		t.Fatalf("mode %o", mode(packages))
	}
	if err := os.Chmod(packages, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := EnsurePrivate(packages); err != nil {
		t.Fatal(err)
	}
	if mode(packages) != 0o700 {
		t.Fatalf("mode %o", mode(packages))
	}
	link := filepath.Join(dir, "link")
	if err := os.Symlink(packages, link); err != nil {
		t.Fatal(err)
	}
	if EnsurePrivate(link) == nil {
		t.Fatal("a link was accepted")
	}
}
