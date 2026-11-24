//go:build linux

package helper

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
)

func executable(t *testing.T, path string, contents []byte) {
	t.Helper()
	if err := os.WriteFile(path, contents, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(path, 0o755); err != nil {
		t.Fatal(err)
	}
}

func layout(t *testing.T) (string, string, string) {
	temp := t.TempDir()
	root := filepath.Join(temp, "App Dir ' ; $(false)")
	for _, dir := range []string{"usr/bin", "usr/lib"} {
		if err := os.MkdirAll(filepath.Join(root, dir), 0o755); err != nil {
			t.Fatal(err)
		}
	}
	exe := filepath.Join(root, "usr/bin/rc2fx")
	executable(t, exe, []byte("fixture executable"))
	executable(t, filepath.Join(root, "AppRun"), []byte(`#!/bin/sh
APPDIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
export APPDIR
export LD_LIBRARY_PATH="$APPDIR/usr/lib"
exec "$APPDIR/usr/bin/rc2fx" "$@"
`))
	return temp, root, exe
}

func str(s string) *string { return &s }

func TestStandaloneLaunchesIgnoreUnrelatedOrMissingPackagingHints(t *testing.T) {
	temp, root, _ := layout(t)
	exe := filepath.Join(temp, "standalone")
	executable(t, exe, []byte("plain"))
	for _, dir := range []*string{nil, &root, str("/nonexistent/appdir")} {
		launch, guard, err := prepare(exe, dir)
		if err != nil || launch != exe || guard != nil {
			t.Fatalf("%v: %s %v %v", dir, launch, guard, err)
		}
	}
	if _, _, err := prepare(exe, str("/")); err == nil {
		t.Fatal("/ accepted as an AppDir")
	}
}

func TestInvalidLaunchersAndLayoutsAreRejected(t *testing.T) {
	_, root, exe := layout(t)
	if err := os.Chmod(filepath.Join(root, "AppRun"), 0o644); err != nil {
		t.Fatal(err)
	}
	if _, _, err := prepare(exe, &root); err == nil {
		t.Fatal("a non-executable AppRun")
	}
	os.Remove(filepath.Join(root, "AppRun"))
	if err := os.Symlink("usr/bin/rc2fx", filepath.Join(root, "AppRun")); err != nil {
		t.Fatal(err)
	}
	if _, _, err := prepare(exe, &root); err == nil {
		t.Fatal("a linked AppRun")
	}
	other := filepath.Join(root, "other")
	executable(t, other, []byte("other"))
	if _, _, err := prepare(other, &root); err == nil {
		t.Fatal("another executable")
	}
}

func TestCopyPreservesContainedLinksModesAndGuardLifetime(t *testing.T) {
	_, root, exe := layout(t)
	library := filepath.Join(root, "usr/lib/library")
	if err := os.WriteFile(library, []byte("library"), 0o640); err != nil {
		t.Fatal(err)
	}
	os.Chmod(library, 0o640)
	if err := os.Symlink("library", filepath.Join(root, "usr/lib/relative")); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(library, filepath.Join(root, "usr/lib/absolute")); err != nil {
		t.Fatal(err)
	}
	launch, guard, err := prepare(exe, &root)
	if err != nil {
		t.Fatal(err)
	}
	info, err := os.Stat(guard.path)
	if err != nil || info.Mode().Perm() != 0o700 {
		t.Fatalf("%v %v", info, err)
	}
	copied := filepath.Dir(launch)
	if link, _ := os.Readlink(filepath.Join(copied, "usr/lib/relative")); link != "library" {
		t.Fatal(link)
	}
	if data, _ := os.ReadFile(filepath.Join(copied, "usr/lib/absolute")); string(data) != "library" {
		t.Fatal(string(data))
	}
	resolved, err := filepath.EvalSymlinks(filepath.Join(copied, "usr/lib/absolute"))
	if err != nil || !strings.HasPrefix(resolved, guard.path) {
		t.Fatalf("%s %v", resolved, err)
	}
	if info, err := os.Stat(filepath.Join(copied, "usr/lib/library")); err != nil || info.Mode().Perm() != 0o640 {
		t.Fatalf("%v %v", info, err)
	}
	if _, err := os.Stat(launch); err != nil {
		t.Fatal(err)
	}
	path := guard.path
	guard.Remove()
	if _, err := os.Stat(path); err == nil {
		t.Fatal("the staged copy outlived its guard")
	}
}

func TestEscapingSymlinksAndSpecialFilesAreRejected(t *testing.T) {
	temp, root, exe := layout(t)
	if err := os.WriteFile(filepath.Join(temp, "outside"), []byte("outside"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("../../../outside", filepath.Join(root, "usr/lib/escape")); err != nil {
		t.Fatal(err)
	}
	if _, guard, err := prepare(exe, &root); err == nil {
		guard.Remove()
		t.Fatal("an escaping link")
	}
	os.Remove(filepath.Join(root, "usr/lib/escape"))
	// Create the socket inode with mknod(2) rather than net.Listen so the
	// test is not bound by Linux's 108-byte sockaddr_un.sun_path limit under
	// long TMPDIR paths. Unprivileged S_IFSOCK creation is permitted on Linux.
	if err := syscall.Mknod(filepath.Join(root, "socket"), syscall.S_IFSOCK|0o600, 0); err != nil {
		t.Fatal(err)
	}
	if _, guard, err := prepare(exe, &root); err == nil {
		guard.Remove()
		t.Fatal("a socket")
	}
}

func TestCopyBudgetsBoundEntriesAndBytes(t *testing.T) {
	_, root, _ := layout(t)
	for _, b := range []budget{{1, maxBytes}, {maxEntries, 1}} {
		destination := filepath.Join(t.TempDir(), "copy")
		if err := copyEntry(root, destination, root, destination, 0, &budget{b.entries, b.bytes}); err == nil {
			t.Fatalf("%+v", b)
		}
	}
}

func TestCopiedLauncherLoadsBundledLibraryAndForwardsLiteralArguments(t *testing.T) {
	if _, err := exec.LookPath("cc"); err != nil {
		t.Skip("no C compiler")
	}
	temp, root, exe := layout(t)
	// Real ELF fixtures test dynamic loading of bundled shared libraries in
	// the staged AppDir rather than only checking an environment variable.
	library := filepath.Join(temp, "library.c")
	os.WriteFile(library, []byte("int fixture(void) { return 42; }\n"), 0o644)
	main := filepath.Join(temp, "main.c")
	os.WriteFile(main, []byte(`#include <stdio.h>
#include <stdlib.h>
extern int fixture(void);
int main(int argc, char **argv) {
    printf("library=%d\nappdir=%s\n", fixture(), getenv("APPDIR"));
    for (int i=1; i<argc; i++) printf("arg=%s\n", argv[i]);
    return argc > 2 ? 17 : 0;
}
`), 0o644)
	if err := exec.Command("cc", "-shared", "-fPIC", library, "-o", filepath.Join(root, "usr/lib/libfixture.so")).Run(); err != nil {
		t.Fatal(err)
	}
	if err := exec.Command("cc", main, "-L", filepath.Join(root, "usr/lib"), "-lfixture", "-o", exe).Run(); err != nil {
		t.Fatal(err)
	}
	launch, guard, err := prepare(exe, &root)
	if err != nil {
		t.Fatal(err)
	}
	defer guard.Remove()
	appdir := filepath.Dir(launch)
	for _, args := range [][]string{
		{"--install-usb-rule"},
		{"--installer-helper", "root ' ; $(false)", "work\nsecond line"},
	} {
		cmd := exec.Command(launch, args...)
		var env []string
		for _, kv := range os.Environ() {
			if !strings.HasPrefix(kv, "APPDIR=") && !strings.HasPrefix(kv, "APPIMAGE=") && !strings.HasPrefix(kv, "LD_LIBRARY_PATH=") {
				env = append(env, kv)
			}
		}
		cmd.Env = env
		var stdout, stderr strings.Builder
		cmd.Stdout, cmd.Stderr = &stdout, &stderr
		err := cmd.Run()
		code := 0
		if exit, ok := err.(*exec.ExitError); ok {
			code = exit.ExitCode()
		} else if err != nil {
			t.Fatal(err)
		}
		want := 0
		if len(args) > 2 {
			want = 17
		}
		if code != want || stderr.Len() != 0 {
			t.Fatalf("%d %q", code, stderr.String())
		}
		expected := "library=42\nappdir=" + appdir + "\n"
		for _, arg := range args {
			expected += "arg=" + arg + "\n"
		}
		if stdout.String() != expected {
			t.Fatalf("%q", stdout.String())
		}
	}
}
