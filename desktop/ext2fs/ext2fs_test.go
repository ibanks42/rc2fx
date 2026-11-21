//go:build linux

package ext2fs

// Tests run against real ext4 images created with mkfs.ext4 matching a Pro II
// root partition's feature set, verified afterward with e2fsck -fn. Skipped if
// e2fsprogs tools aren't installed.

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
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

func newImageWithOpts(t *testing.T, block int, size string, features string, populate func(tree string)) *image {
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
	if populate != nil {
		populate(tree)
	}
	path := filepath.Join(dir, "root.img")
	out, err := exec.Command(mkfs, "-F", "-q", "-t", "ext4", "-b", strconv.Itoa(block), "-I", "256",
		"-O", features,
		"-d", tree, path, size).CombinedOutput()
	if err != nil {
		t.Fatalf("mkfs.ext4: %v\n%s", err, out)
	}
	return &image{path: path, e2fsck: e2fsck, debugfs: debugfs}
}

func newImage(t *testing.T, block int) *image {
	t.Helper()
	return newImageWithOpts(t, block, "8M",
		"metadata_csum,64bit,extent,flex_bg,huge_file,dir_nlink,extra_isize,^inline_data,^orphan_file,^metadata_csum_seed",
		nil)
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

func (i *image) debugfsScript(t *testing.T, commands []string) {
	t.Helper()
	cmdFile := filepath.Join(filepath.Dir(i.path), "debugfs.cmds")
	must(t, os.WriteFile(cmdFile, []byte(strings.Join(commands, "\n")+"\n"), 0o600))
	if out, err := exec.Command(i.debugfs, "-w", "-f", cmdFile, i.path).CombinedOutput(); err != nil {
		t.Fatalf("debugfs -f: %v\n%s", err, out)
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

func patternBytes(n int, mul uint32) []byte {
	b := make([]byte, n)
	for i := range b {
		b[i] = byte((uint32(i)*mul + 17) % 251)
	}
	return b
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

func TestMultiLevelExtentTreeReadAndOverwrite(t *testing.T) {
	for _, tc := range []struct {
		block    int
		runs     int
		runBlks  int
		minDepth uint16
	}{
		{block: 1024, runs: 8, runBlks: 2, minDepth: 1},
		{block: 4096, runs: 8, runBlks: 2, minDepth: 1},
		{block: 1024, runs: 350, runBlks: 1, minDepth: 2},
	} {
		name := fmt.Sprintf("%dB_depth%d", tc.block, tc.minDepth)
		t.Run(name, func(t *testing.T) {
			image := newImage(t, tc.block)
			cmds := []string{
				"write /dev/null usr/bin/frag",
				"write /dev/null usr/bin/spacer",
			}
			for r := 0; r < tc.runs; r++ {
				start := r * tc.runBlks
				end := start + tc.runBlks - 1
				cmds = append(cmds,
					fmt.Sprintf("fallocate usr/bin/frag %d %d", start, end),
					fmt.Sprintf("fallocate usr/bin/spacer %d %d", start, end),
				)
			}
			size := tc.runs * tc.runBlks * tc.block
			cmds = append(cmds, fmt.Sprintf("sif usr/bin/frag size %d", size))
			image.debugfsScript(t, cmds)
			image.clean(t)

			fs := open(t, image.path, 0, true)
			entry := lookup(t, fs, "usr/bin/frag")
			in, err := fs.readInode(entry.Inode)
			must(t, err)
			depth := binary.LittleEndian.Uint16(in.raw[inodeInlineBlocksOffset+6 : inodeInlineBlocksOffset+8])
			if depth < tc.minDepth {
				t.Fatalf("expected extent depth >= %d, got %d", tc.minDepth, depth)
			}
			extents, err := fs.collectExtents(in)
			must(t, err)
			if len(extents) < tc.runs {
				t.Fatalf("expected >= %d fragmented extents, got %d", tc.runs, len(extents))
			}

			if !bytes.Equal(read(t, fs, entry), make([]byte, size)) {
				t.Fatal("unwritten fragmented extents should read as zeros")
			}

			first := patternBytes(size, 19)
			must(t, fs.Overwrite(entry, first))
			must(t, fs.Close())
			image.clean(t)

			fs = open(t, image.path, 0, true)
			entry = lookup(t, fs, "usr/bin/frag")
			if !bytes.Equal(read(t, fs, entry), first) {
				t.Fatal("first overwrite did not persist")
			}

			// Overwrite again now that extents in external leaf blocks are initialized.
			second := patternBytes(size, 37)
			must(t, fs.Overwrite(entry, second))
			must(t, fs.Close())
			image.clean(t)

			fs = open(t, image.path, 0, false)
			defer fs.Close()
			entry = lookup(t, fs, "usr/bin/frag")
			if !bytes.Equal(read(t, fs, entry), second) {
				t.Fatal("second overwrite did not persist")
			}
		})
	}
}

func TestInodeAndExtentChecksumParityAndCorruptionDetection(t *testing.T) {
	for _, features := range []string{
		"metadata_csum,64bit,extent,flex_bg,huge_file,dir_nlink,extra_isize,^inline_data,^orphan_file,^metadata_csum_seed",
		"metadata_csum,64bit,extent,flex_bg,huge_file,dir_nlink,extra_isize,^inline_data,^orphan_file,metadata_csum_seed",
	} {
		image := newImageWithOpts(t, 1024, "8M", features, nil)
		cmds := []string{
			"write /dev/null usr/bin/extcsum",
			"write /dev/null usr/bin/spacer",
		}
		for r := 0; r < 6; r++ {
			cmds = append(cmds,
				fmt.Sprintf("fallocate usr/bin/extcsum %d %d", r, r),
				fmt.Sprintf("fallocate usr/bin/spacer %d %d", r, r),
			)
		}
		cmds = append(cmds, fmt.Sprintf("sif usr/bin/extcsum size %d", 6*1024))
		image.debugfsScript(t, cmds)
		image.clean(t)

		fs := open(t, image.path, 0, false)
		entry := lookup(t, fs, "usr/bin/extcsum")
		in, err := fs.readInode(entry.Inode)
		must(t, err)
		extents, err := fs.collectExtents(in)
		must(t, err)
		if len(extents) == 0 || extents[0].leafBlock == 0 {
			t.Fatalf("expected external extent leaf block, got %+v", extents)
		}
		inodeBlk, inodeOff, err := fs.inodeLocation(entry.Inode)
		must(t, err)
		leafBlk := extents[0].leafBlock
		must(t, fs.Close())

		orig, err := os.ReadFile(image.path)
		must(t, err)

		// 1. Corrupt superblock.
		badSB := append([]byte(nil), orig...)
		badSB[1024+0x10] ^= 0x01
		badSBPath := filepath.Join(t.TempDir(), "bad_sb.img")
		must(t, os.WriteFile(badSBPath, badSB, 0o600))
		if badFS, err := Open(badSBPath, 0, false); err == nil {
			badFS.Close()
			t.Fatal("expected superblock checksum failure")
		}

		// 2. Corrupt inode bytes.
		badIno := append([]byte(nil), orig...)
		badIno[int(inodeBlk)*1024+int(inodeOff)+0x10] ^= 0x01
		badInoPath := filepath.Join(t.TempDir(), "bad_ino.img")
		must(t, os.WriteFile(badInoPath, badIno, 0o600))
		badInoFS := open(t, badInoPath, 0, false)
		if _, err := badInoFS.Lookup("usr/bin/extcsum"); err == nil {
			badInoFS.Close()
			t.Fatal("expected inode checksum failure on lookup")
		}
		must(t, badInoFS.Close())

		// 3. Corrupt external extent leaf block.
		badExt := append([]byte(nil), orig...)
		badExt[int(leafBlk)*1024+12] ^= 0x01
		badExtPath := filepath.Join(t.TempDir(), "bad_ext.img")
		must(t, os.WriteFile(badExtPath, badExt, 0o600))
		badExtFS := open(t, badExtPath, 0, false)
		if _, err := badExtFS.Read(entry); err == nil {
			badExtFS.Close()
			t.Fatal("expected extent block checksum failure on read")
		}
		must(t, badExtFS.Close())
	}
}

func (i *image) optimizeDirs(t *testing.T) {
	t.Helper()
	cmd := exec.Command(i.e2fsck, "-f", "-D", "-y", i.path)
	out, err := cmd.CombinedOutput()
	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); !ok || exitErr.ExitCode() > 1 {
			t.Fatalf("e2fsck -fDy: %v\n%s", err, out)
		}
	}
}

func TestHTreeDirectoryAndIntermediateSymlinks(t *testing.T) {
	for _, block := range []int{1024, 4096} {
		t.Run(fmt.Sprint(block), func(t *testing.T) {
			image := newImageWithOpts(t, block, "8M",
				"dir_index,metadata_csum,64bit,extent,flex_bg,huge_file,dir_nlink,extra_isize,^inline_data,^orphan_file,^metadata_csum_seed",
				func(tree string) {
					manyDir := filepath.Join(tree, "usr/share/many")
					must(t, os.MkdirAll(manyDir, 0o755))
					for i := 0; i < 320; i++ {
						name := fmt.Sprintf("entry-%03d-long-name-to-trigger-htree", i)
						must(t, os.WriteFile(filepath.Join(manyDir, name), []byte(name+"\n"), 0o644))
					}
					must(t, os.MkdirAll(filepath.Join(tree, "usr/lib"), 0o755))
					must(t, os.Symlink("usr/bin", filepath.Join(tree, "dirlink")))
					must(t, os.Symlink("../bin", filepath.Join(tree, "usr/lib/dirsym")))
					must(t, os.Symlink("/etc/hostname", filepath.Join(tree, "usr/lib/abslink")))
				})
			image.optimizeDirs(t)
			image.clean(t)

			fs := open(t, image.path, 0, false)
			defer fs.Close()

			manyEntry := lookup(t, fs, "usr/share/many")
			manyIn, err := fs.readInode(manyEntry.Inode)
			must(t, err)
			if manyIn.flags&0x1000 == 0 {
				t.Fatalf("expected HTree EXT2_INDEX_FL on directory inode, flags=0x%08x", manyIn.flags)
			}

			for _, idx := range []int{0, 159, 319} {
				name := fmt.Sprintf("entry-%03d-long-name-to-trigger-htree", idx)
				e := lookup(t, fs, "usr/share/many/"+name)
				if got := string(read(t, fs, e)); got != name+"\n" {
					t.Fatalf("got %q, want %q", got, name+"\n")
				}
			}
			if missing, err := fs.Lookup("usr/share/many/entry-999-missing"); err != nil || missing != nil {
				t.Fatalf("expected nil, nil for missing entry; got %v, %v", missing, err)
			}

			// Intermediate symlinks (top-level and relative) are followed, trailing ones are not.
			viaTop := lookup(t, fs, "dirlink/rode-init-update-env.sh")
			if !viaTop.Regular || !bytes.Equal(read(t, fs, viaTop), script) {
				t.Fatal("failed to resolve via intermediate symlink")
			}
			viaRel := lookup(t, fs, "usr/lib/dirsym/rode-init-update-env.sh")
			if !viaRel.Regular || !bytes.Equal(read(t, fs, viaRel), script) {
				t.Fatal("failed to resolve via relative intermediate symlink")
			}
			trailingAbs := lookup(t, fs, "usr/lib/abslink")
			if trailingAbs.Regular {
				t.Fatal("trailing absolute symlink must not be followed")
			}
			if trailingTop, err := fs.Lookup("dirlink"); err != nil || trailingTop == nil || trailingTop.Regular {
				t.Fatalf("trailing dirlink must not be followed: %+v, %v", trailingTop, err)
			}
			if sub, err := fs.Lookup("usr/lib/abslink/sub"); err == nil {
				t.Fatalf("expected error traversing through symlink to regular file, got %+v", sub)
			}
		})
	}
}

func TestNonExtentIndirectAndDoubleIndirectBlocks(t *testing.T) {
	image := newImageWithOpts(t, 1024, "8M",
		"^extent,^64bit,^metadata_csum,flex_bg,huge_file,dir_nlink,extra_isize,^inline_data,^orphan_file",
		nil)
	fs := open(t, image.path, 0, true)
	entry := lookup(t, fs, "usr/bin/big")
	in, err := fs.readInode(entry.Inode)
	must(t, err)
	if in.flags&inodeExtentsFl != 0 {
		t.Fatalf("expected non-extent inode, flags=0x%08x", in.flags)
	}
	if !bytes.Equal(read(t, fs, entry), bigData()) {
		t.Fatal("initial indirect read mismatch")
	}

	replacement := patternBytes(len(bigData()), 29)
	must(t, fs.Overwrite(entry, replacement))
	must(t, fs.Close())
	image.clean(t)

	fs = open(t, image.path, 0, false)
	defer fs.Close()
	entry = lookup(t, fs, "usr/bin/big")
	if !bytes.Equal(read(t, fs, entry), replacement) {
		t.Fatal("indirect overwrite mismatch")
	}
}

func TestSparseHoleOverwriteIsRefusedWithoutCorruptingData(t *testing.T) {
	image := newImage(t, 1024)
	payloadFile := filepath.Join(t.TempDir(), "head.bin")
	head := patternBytes(2048, 11)
	must(t, os.WriteFile(payloadFile, head, 0o600))
	image.debugfsScript(t, []string{
		fmt.Sprintf("write %s usr/bin/sparse", payloadFile),
		"fallocate usr/bin/sparse 4 5",
		"sif usr/bin/sparse size 6144",
	})
	image.clean(t)

	fs := open(t, image.path, 0, true)
	entry := lookup(t, fs, "usr/bin/sparse")
	got := read(t, fs, entry)
	want := append(append([]byte(nil), head...), make([]byte, 4096)...)
	if !bytes.Equal(got, want) {
		t.Fatal("sparse hole did not read as zeros")
	}
	if err := fs.Overwrite(entry, patternBytes(6144, 23)); err == nil {
		t.Fatal("expected Overwrite to refuse sparse hole")
	}
	must(t, fs.Close())
	image.clean(t)

	fs = open(t, image.path, 0, false)
	defer fs.Close()
	if !bytes.Equal(read(t, fs, entry), want) {
		t.Fatal("sparse file contents changed after refused Overwrite")
	}
}

func TestPreallocatedExtentStraddlingEOF(t *testing.T) {
	image := newImage(t, 1024)
	image.debugfsScript(t, []string{
		"write /dev/null usr/bin/straddle",
		"fallocate usr/bin/straddle 0 9",
		"sif usr/bin/straddle size 5000",
	})
	image.clean(t)

	fs := open(t, image.path, 0, true)
	entry := lookup(t, fs, "usr/bin/straddle")
	if !bytes.Equal(read(t, fs, entry), make([]byte, 5000)) {
		t.Fatal("straddling preallocated file should read as zeros")
	}
	want := patternBytes(5000, 41)
	must(t, fs.Overwrite(entry, want))
	must(t, fs.Close())
	image.clean(t)

	fs = open(t, image.path, 0, false)
	defer fs.Close()
	entry = lookup(t, fs, "usr/bin/straddle")
	if !bytes.Equal(read(t, fs, entry), want) {
		t.Fatal("straddling overwrite did not persist")
	}
	in, err := fs.readInode(entry.Inode)
	must(t, err)
	extents, err := fs.collectExtents(in)
	must(t, err)
	if len(extents) != 2 || extents[0].unwritten || extents[0].length != 5 || !extents[1].unwritten || extents[1].length != 5 {
		t.Fatalf("expected [0..5 init, 5..10 uninit], got %+v", extents)
	}
}

func TestUnsupportedIncompatFeaturesRejected(t *testing.T) {
	for _, feat := range []string{"inline_data", "encrypt"} {
		t.Run(feat, func(t *testing.T) {
			image := newImageWithOpts(t, 4096, "8M",
				"metadata_csum,64bit,extent,flex_bg,huge_file,dir_nlink,extra_isize,"+feat,
				nil)
			if fs, err := Open(image.path, 0, false); err == nil {
				fs.Close()
				t.Fatalf("expected Open to reject feature %s", feat)
			}
		})
	}
}

func TestNilAndClosedFilesystemSafety(t *testing.T) {
	var nilFS *Filesystem
	if nilFS.NeedsRecovery() {
		t.Fatal("nil NeedsRecovery should be false")
	}
	if err := nilFS.Close(); err != nil {
		t.Fatalf("nil Close: %v", err)
	}
	if _, err := nilFS.Lookup("etc/hostname"); err == nil {
		t.Fatal("nil Lookup should error")
	}
	if _, err := nilFS.Read(Entry{Inode: 2, Regular: true, Size: 1}); err == nil {
		t.Fatal("nil Read should error")
	}
	if err := nilFS.Overwrite(Entry{Inode: 2, Regular: true, Size: 1}, []byte{0}); err == nil {
		t.Fatal("nil Overwrite should error")
	}

	image := newImage(t, 1024)
	fs := open(t, image.path, 0, true)
	entry := lookup(t, fs, "etc/hostname")
	must(t, fs.Close())
	must(t, fs.Close())
	if fs.NeedsRecovery() {
		t.Fatal("closed NeedsRecovery should be false")
	}
	if _, err := fs.Lookup("etc/hostname"); err == nil {
		t.Fatal("closed Lookup should error")
	}
	if _, err := fs.Read(entry); err == nil {
		t.Fatal("closed Read should error")
	}
	if err := fs.Overwrite(entry, make([]byte, entry.Size)); err == nil {
		t.Fatal("closed Overwrite should error")
	}
}
