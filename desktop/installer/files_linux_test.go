//go:build linux

package installer

import (
	"os"
	"path/filepath"
	"syscall"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
)

func TestLinksAndSpecialFilesInTheCacheAreNotOpened(t *testing.T) {
	dir := t.TempDir()
	real, link := filepath.Join(dir, "real"), filepath.Join(dir, "link")
	put(t, real, []byte("data"))
	must(t, os.Symlink(real, link))
	file, err := openWorkFile(real)
	must(t, err)
	file.Close()
	if f, err := openRegular(link); err == nil {
		f.Close()
		t.Fatal("openRegular followed a link")
	}
	if f, err := openWorkFile(link); err == nil {
		f.Close()
		t.Fatal("openWorkFile followed a link")
	}
	if f, err := openLocalFile(link, false); err == nil {
		f.Close()
		t.Fatal("openLocalFile followed a link")
	}
	if _, _, err := hashBoth(link, func(uint64, uint64) {}); err == nil {
		t.Fatal("hashBoth followed a link")
	}
	if _, err := fileHash(link, algorithmSha256); err == nil {
		t.Fatal("fileHash followed a link")
	}
	if err := copyWithProgress(link, filepath.Join(dir, "copy"), func(uint64, uint64) {}); err == nil {
		t.Fatal("copyWithProgress followed a link")
	}
	// A FIFO neither blocks the open nor passes as a file.
	fifo := filepath.Join(dir, "fifo")
	must(t, syscall.Mkfifo(fifo, 0o600))
	if f, err := openRegular(fifo); err == nil {
		f.Close()
		t.Fatal("a FIFO passed as a file")
	}
	if f, err := openRegular(dir); err == nil {
		f.Close()
		t.Fatal("a directory passed as a file")
	}
}

func TestPrivilegedHelpersUseTheInvokersHomeNotRootsEnvironment(t *testing.T) {
	uid := uint32(os.Getuid())
	_, expected, ok := archive.Account(uid)
	if !ok {
		t.Fatal("test account exists")
	}
	s := func(v string) *string { return &v }
	if got := homeForIdentity(s("/wrong-helper-home"), true, &uid); got == nil || *got != expected {
		t.Fatalf("%v", got)
	}
	if got := homeForIdentity(s("/normal-home"), false, &uid); got == nil || *got != "/normal-home" {
		t.Fatalf("%v", got)
	}
	if got := homeForIdentity(s("/root"), true, nil); got == nil || *got != "/root" {
		t.Fatalf("%v", got)
	}
	unknown := ^uint32(0)
	if got := homeForIdentity(s("/root"), true, &unknown); got != nil {
		t.Fatalf("%v", *got)
	}
}
