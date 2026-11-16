//go:build linux

package installer

import (
	"os"
	"path/filepath"
	"reflect"
	"syscall"
	"testing"
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

func TestAPrivilegedHelperReadsOnlyItsInvokersFiles(t *testing.T) {
	u := func(v uint32) *uint32 { return &v }
	if !invokerOwns(1000, u(1000)) {
		t.Fatal("the invoker's own file")
	}
	if invokerOwns(0, u(1000)) {
		t.Fatal("a root file placed in the folder")
	}
	if invokerOwns(1001, u(1000)) {
		t.Fatal("another user's file")
	}
	if !invokerOwns(0, nil) || invokerOwns(1000, nil) {
		t.Fatal("root's own folder")
	}
}

func TestTheInvokerComesFromPkexecOrASudoUserTheSystemKnows(t *testing.T) {
	found := func(vars map[string]string) (*[2]uint32, error) {
		return invoker(
			func(name string) (string, bool) { v, ok := vars[name]; return v, ok },
			func(uid uint32) (uint32, bool) { return 100, uid == 1000 },
			func(name string) (uint32, bool) { return 1000, name == "me" },
		)
	}
	if got, err := found(nil); err != nil || got != nil {
		t.Fatalf("%v %v", got, err)
	}
	// pkexec wins over anything claimed through sudo's variables.
	both := map[string]string{"PKEXEC_UID": "1000", "SUDO_UID": "1", "SUDO_GID": "1", "SUDO_USER": "someone"}
	if got, err := found(both); err != nil || !reflect.DeepEqual(got, &[2]uint32{1000, 100}) {
		t.Fatalf("%v %v", got, err)
	}
	sudo := func(uid, user string) map[string]string {
		return map[string]string{"SUDO_UID": uid, "SUDO_GID": "100", "SUDO_USER": user}
	}
	if got, err := found(sudo("1000", "me")); err != nil || !reflect.DeepEqual(got, &[2]uint32{1000, 100}) {
		t.Fatalf("%v %v", got, err)
	}
	for _, bad := range []map[string]string{
		sudo("1", "me"),        // uid is not that user's
		sudo("1000", "nobody"), // unknown user
		{"SUDO_UID": "1000", "SUDO_GID": "100"},
		{"PKEXEC_UID": "2000"}, // unknown uid
	} {
		if _, err := found(bad); err == nil {
			t.Fatalf("%v accepted", bad)
		}
	}
}

func TestPrivilegedHelpersUseTheInvokersHomeNotRootsEnvironment(t *testing.T) {
	uid := uint32(os.Getuid())
	_, expected, ok := account(uid)
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
