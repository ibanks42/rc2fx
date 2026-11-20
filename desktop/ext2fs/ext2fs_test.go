//go:build linux && cgo

package ext2fs

// Tests run against real ext4 images created with mkfs.ext4 matching a Pro II
// root partition's feature set, verified afterward with e2fsck -fn. Skipped if
// e2fsprogs tools aren't installed.

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"testing"
)

func tool(name string) string {
	for _, dir := range []string{"/usr/sbin", "/sbin", "/usr/bin", "/bin"} {
		path := filepath.Join(dir, name)
		if _, err := os.Stat(path); err == nil {
			return path
		}
	}
	return ""
}

var script = []byte("#!/bin/sh\n# mounts the settings partition\nmount -t vfat /dev/mmcblk1p3 /Application/update-staging\n")

type image struct {
	path    string
	e2fsck  string
	debugfs string
}

func newImage(t *testing.T, block int) *image {
	t.Helper()
	mkfs, e2fsck, debugfs := tool("mkfs.ext4"), tool("e2fsck"), tool("debugfs")
	if mkfs == "" || e2fsck == "" || debugfs == "" {
		t.Skip("skipped: e2fsprogs' mkfs.ext4, e2fsck or debugfs is not installed")
	}
	dir := t.TempDir()
	tree := filepath.Join(dir, "tree")
	must(t, os.MkdirAll(filepath.Join(tree, "usr/bin"), 0o755))
	must(t, os.MkdirAll(filepath.Join(tree, "etc"), 0o755))
	must(t, os.WriteFile(filepath.Join(tree, "usr/bin/rode-init-update-env.sh"), script, 0o644))
	must(t, os.WriteFile(filepath.Join(tree, "etc/hostname"), []byte("rodecaster-pro-ii\n"), 0o644))
	must(t, os.WriteFile(filepath.Join(tree, "usr/bin/big"), bigData(), 0o644))
	must(t, os.Symlink("big", filepath.Join(tree, "usr/bin/link")))
	path := filepath.Join(dir, "root.img")
	out, err := exec.Command(mkfs, "-F", "-q", "-t", "ext4", "-b", strconv.Itoa(block), "-I", "256",
		"-O", "metadata_csum,64bit,extent,flex_bg,huge_file,dir_nlink,extra_isize,^inline_data,^orphan_file,^metadata_csum_seed",
		"-d", tree, path, "8M").CombinedOutput()
	if err != nil {
		t.Fatalf("mkfs.ext4: %v\n%s", err, out)
	}
	return &image{path: path, e2fsck: e2fsck, debugfs: debugfs}
}

func bigData() []byte {
	big := make([]byte, 300_000)
	for n := range big {
		big[n] = byte(uint32(n) * 7 % 251)
	}
	return big
}

func (i *image) clean(t *testing.T) {
	t.Helper()
	out, err := exec.Command(i.e2fsck, "-f", "-n", i.path).CombinedOutput()
	if err != nil {
		t.Fatalf("e2fsck -fn: %v\n%s", err, out)
	}
}

func (i *image) debugfsRun(t *testing.T, request string) {
	t.Helper()
	if out, err := exec.Command(i.debugfs, "-w", "-R", request, i.path).CombinedOutput(); err != nil {
		t.Fatalf("debugfs %s: %v\n%s", request, err, out)
	}
}

func must(t *testing.T, err error) {
	t.Helper()
	if err != nil {
		t.Fatal(err)
	}
}

func open(t *testing.T, path string, offset uint64, writable bool) *Filesystem {
	t.Helper()
	fs, err := Open(path, offset, writable)
	if err != nil {
		t.Fatalf("open %s at %d: %v", path, offset, err)
	}
	return fs
}

func lookup(t *testing.T, fs *Filesystem, path string) Entry {
	t.Helper()
	entry, err := fs.Lookup(path)
	if err != nil || entry == nil {
		t.Fatalf("lookup %s: %v, %v", path, entry, err)
	}
	return *entry
}

func read(t *testing.T, fs *Filesystem, entry Entry) []byte {
	t.Helper()
	data, err := fs.Read(entry)
	if err != nil {
		t.Fatalf("read inode %d: %v", entry.Inode, err)
	}
	return data
}

func TestAFileIsFoundReadAndRestoredInPlace(t *testing.T) {
	for _, block := range []int{1024, 4096} {
		t.Run(fmt.Sprint(block), func(t *testing.T) {
			image := newImage(t, block)
			name := "usr/bin/rode-init-update-env.sh"
			fs := open(t, image.path, 0, true)
			if fs.NeedsRecovery() {
				t.Fatal("a fresh image needs no recovery")
			}
			entry := lookup(t, fs, name)
			if !entry.Regular || entry.Size != uint64(len(script)) {
				t.Fatalf("%+v", entry)
			}
			if !bytes.Equal(read(t, fs, entry), script) {
				t.Fatal("script contents")
			}
			// Simulate an interrupted flash that left the file zero-filled.
			must(t, fs.Overwrite(entry, make([]byte, len(script))))
			must(t, fs.Close())
			image.clean(t)
			fs = open(t, image.path, 0, true)
			entry = lookup(t, fs, name)
			if !bytes.Equal(read(t, fs, entry), make([]byte, len(script))) {
				t.Fatal("zeros did not read back")
			}
			must(t, fs.Overwrite(entry, script))
			must(t, fs.Close())
			image.clean(t)
			fs = open(t, image.path, 0, false)
			defer fs.Close()
			entry = lookup(t, fs, name)
			if !bytes.Equal(read(t, fs, entry), script) {
				t.Fatalf("block size %d", block)
			}
			big := lookup(t, fs, "usr/bin/big")
			if n := len(read(t, fs, big)); n != 300_000 {
				t.Fatalf("big file read %d bytes", n)
			}
		})
	}
}

func TestPreallocatedBlocksThatWereNeverWrittenAreFilled(t *testing.T) {
	// fallocate leaves unwritten extents that read back as zeros, which can
	// also happen after an interrupted write.
	image := newImage(t, 1024)
	image.debugfsRun(t, "write /dev/null usr/bin/prealloc")
	image.debugfsRun(t, "fallocate usr/bin/prealloc 0 7")
	image.debugfsRun(t, fmt.Sprintf("sif usr/bin/prealloc size %d", 8*1024))
	image.clean(t)
	data := make([]byte, 8*1024)
	for n := range data {
		data[n] = byte(n % 253)
	}
	fs := open(t, image.path, 0, true)
	entry := lookup(t, fs, "usr/bin/prealloc")
	if !bytes.Equal(read(t, fs, entry), make([]byte, len(data))) {
		t.Fatal("preallocated blocks should read as zeros")
	}
	must(t, fs.Overwrite(entry, data))
	must(t, fs.Close())
	image.clean(t)
	fs = open(t, image.path, 0, false)
	defer fs.Close()
	entry = lookup(t, fs, "usr/bin/prealloc")
	if !bytes.Equal(read(t, fs, entry), data) {
		t.Fatal("written data did not read back")
	}
}

func TestAFilesystemInsideADiskIsOpenedAtItsOffset(t *testing.T) {
	image := newImage(t, 1024)
	disk := filepath.Join(filepath.Dir(image.path), "disk.img")
	root, err := os.ReadFile(image.path)
	must(t, err)
	must(t, os.WriteFile(disk, append(make([]byte, 1<<20), root...), 0o644))
	fs := open(t, disk, 1<<20, false)
	defer fs.Close()
	entry := lookup(t, fs, "etc/hostname")
	if got := read(t, fs, entry); string(got) != "rodecaster-pro-ii\n" {
		t.Fatalf("%q", got)
	}
	if other, err := Open(disk, 0, false); err == nil {
		other.Close()
		t.Fatal("no filesystem at 0")
	}
}

func TestLookupsAndWritesRefuseWhatTheRepairMustNotTouch(t *testing.T) {
	image := newImage(t, 1024)
	fs := open(t, image.path, 0, false)
	missing, err := fs.Lookup("usr/bin/missing")
	if err != nil || missing != nil {
		t.Fatalf("missing: %v, %v", missing, err)
	}
	dir := lookup(t, fs, "usr/bin")
	if dir.Regular {
		t.Fatal("a directory is not regular")
	}
	if _, err := fs.Read(dir); err == nil {
		t.Fatal("a directory is not read")
	}
	link := lookup(t, fs, "usr/bin/link")
	if link.Regular {
		t.Fatal("a final symlink is not followed")
	}
	entry := lookup(t, fs, "etc/hostname")
	if err := fs.Overwrite(entry, []byte("rodecaster-pro-ii\n")); err == nil {
		t.Fatal("read-only")
	}
	must(t, fs.Close())
	fs = open(t, image.path, 0, true)
	if err := fs.Overwrite(entry, []byte("shorter")); err == nil {
		t.Fatal("size must match")
	}
	must(t, fs.Close())
	image.clean(t)
}

func TestAnUnreplayedJournalIsReported(t *testing.T) {
	image := newImage(t, 1024)
	image.debugfsRun(t, "feature needs_recovery")
	fs := open(t, image.path, 0, false)
	defer fs.Close()
	if !fs.NeedsRecovery() {
		t.Fatal("needs_recovery is reported")
	}
}
