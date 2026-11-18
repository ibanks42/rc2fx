package carrier

import (
	"bytes"
	"context"
	"encoding/binary"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"testing"
	"time"
)

// These tests run the real script with bash against a fake mixer: every
// device, mount and U-Boot tool is a stub on PATH backed by files under
// t.TempDir(). The fake disk is a sparse file whose sector 0 holds a real MBR,
// and the fake sfdisk reads and writes that MBR, so a restore really puts the
// table back. Nothing needs root.

const (
	diskSectors = 1202176 // just past the gap partition 1 grows into
	p4Sectors   = 10328064
)

var factoryEntry1 = []byte{0x00, 0x00, 0x01, 0x40, 0x83, 0x03, 0xe0, 0xff, 0x00, 0x20, 0x00, 0x00, 0x62, 0xdc, 0x0f, 0x00}

// mbr is a Pro II's sector 0 with partition 1 at p1 sectors and partition 2
// starting at p2start.
func mbr(p1, p2start uint32) []byte {
	b := make([]byte, 512)
	for i := range 440 {
		b[i] = byte(i*7 + 3)
	}
	copy(b[440:444], []byte{0x24, 0x93, 0x3a, 0xed})
	entries := [4][2]uint32{{8192, p1}, {p2start, 1193984}, {3223552, 1710080}, {4941824, p4Sectors}}
	for i, e := range entries {
		o := 446 + 16*i
		copy(b[o+1:o+4], []byte{0xfe, 0xff, 0xff})
		b[o+4] = 0x83
		copy(b[o+5:o+8], []byte{0xfe, 0xff, 0xff})
		binary.LittleEndian.PutUint32(b[o+8:], e[0])
		binary.LittleEndian.PutUint32(b[o+12:], e[1])
	}
	if p1 == 1039458 && p2start == 2000896 {
		copy(b[446:462], factoryEntry1)
	}
	b[510], b[511] = 0x55, 0xaa
	return b
}

const stubLog = `log() { printf '%s\n' "$(basename "$0") $*" >>"$FAKE/calls"; }
`

var stubs = map[string]string{
	// sfdisk -d DISK prints the table from the fake disk's sector 0;
	// sfdisk ... -N 1 DISK writes entry 1 from "start,size" on stdin, with
	// the CHS bytes a real sfdisk wrote on a mixer.
	"sfdisk": stubLog + `
if [ "$1" = -d ]; then
	[ -f "$FAKE/sfdisk_d_fail" ] && exit 1
	disk=$2
	printf 'label: dos\nlabel-id: 0xed3a9324\ndevice: %s\nunit: sectors\nsector-size: 512\n\n' "$disk"
	for n in 1 2 3 4; do
		o=$((446 + (n - 1) * 16))
		type=$(od -An -tx1 -j $((o + 4)) -N1 "$disk" | tr -d ' \n')
		start=$(od -An -tu4 -j $((o + 8)) -N4 "$disk" | tr -d ' \n')
		size=$(od -An -tu4 -j $((o + 12)) -N4 "$disk" | tr -d ' \n')
		[ "$size" = 0 ] && continue
		printf '%sp%s : start=%12s, size=%12s, type=%s\n' "$disk" "$n" "$start" "$size" "$type"
	done
	exit 0
fi
log "$@"
[ -f "$FAKE/sfdisk_fail" ] && exit 1
for disk; do :; done
IFS=, read -r start size
[ -f "$FAKE/sfdisk_wrong" ] && size=$((size + 1))
le() { printf '\\x%02x\\x%02x\\x%02x\\x%02x' $(($1 & 255)) $((($1 >> 8) & 255)) $((($1 >> 16) & 255)) $((($1 >> 24) & 255)); }
printf "\\x80\\x83\\x03\\x50\\x5f$(le "$start")$(le "$size")" | dd of="$disk" bs=1 seek=449 conv=notrunc 2>/dev/null
[ -f "$FAKE/sfdisk_wrong" ] && printf 'X' | dd of="$disk" bs=1 seek=0 conv=notrunc 2>/dev/null
exit 0
`,
	"resizepart": stubLog + `log "$@"
[ -f "$FAKE/resizepart_fail" ] && exit 1
printf '%s\n' "$3" >"$FAKE/kernel_p1"
`,
	"blockdev": `case $2 in *p1) cat "$FAKE/kernel_p1" ;; *) exit 1 ;; esac
`,
	"fw_printenv": `while IFS= read -r l; do
	case $l in "$1="*) printf '%s\n' "$l"; exit 0 ;; esac
done <"$FAKE/env"
echo "## Error: \"$1\" not defined" >&2
exit 1
`,
	"fw_setenv": stubLog + `log "$@"
name=$1
shift
grep -v "^$name=" "$FAKE/env" >"$FAKE/env.new"
if [ $# -gt 0 ]; then
	v="$*"
	[ -f "$FAKE/fw_wrong" ] && [ "$name" = devplist ] && [ "$v" = 2 ] && v=9
	printf '%s=%s\n' "$name" "$v" >>"$FAKE/env.new"
fi
mv "$FAKE/env.new" "$FAKE/env"
`,
	// mount replaces the mount point with a link to the directory that
	// backs the device, and lists it in the fake /proc/mounts.
	"mount": stubLog + `log "$@"
while [ $# -gt 2 ]; do shift; done
src=$1 dir=$2
case $src in
"$FAKE_P2_DEV") back=$FAKE_P2_DIR; [ -f "$FAKE/mount_p2_fail" ] && exit 32 ;;
"$FAKE_IMAGE") back=$FAKE_IMGFS ;;
"$FAKE_ACK_IMAGE") back=$FAKE_ACKFS ;;
*) exit 32 ;;
esac
rmdir "$dir" && ln -s "$back" "$dir" || exit 32
printf '%s %s ext4 rw 0 0\n' "$src" "$dir" >>"$FAKE/mounts"
`,
	"umount": stubLog + `log "$@"
dir=$1
[ -L "$dir" ] || exit 32
rm "$dir" && mkdir "$dir"
grep -v " $dir " "$FAKE/mounts" >"$FAKE/mounts.new"
mv "$FAKE/mounts.new" "$FAKE/mounts"
`,
	// pgrep reports RØDE's program running once $FAKE/mixer exists, and
	// the first time it does, wipes the image as RØDE's mkfs.vfat would.
	"pgrep": `[ -f "$FAKE/mixer" ] || exit 1
if [ ! -f "$FAKE/mkfs_done" ]; then
	printf 'freshly formatted\n' >"$FAKE_IMAGE"
	: >"$FAKE/mkfs_done"
fi
exit 0
`,
	"reboot": stubLog + `log "$@"
`,
	// mkfs.vfat empties the folder that backs the report disk.
	"mkfs.vfat": stubLog + `log "$@"
[ -f "$FAKE/mkfs_fail" ] && exit 1
rm -rf "$FAKE_ACKFS" && mkdir "$FAKE_ACKFS"
`,
	"sync": "exit 0\n",
}

type rig struct {
	// hostDelay: how long the fake computer waits after seeing the report
	// disk before it acts on it.
	hostDelay                         time.Duration
	t                                 *testing.T
	dir, fake, root, p2, imgfs, ackfs string
	disk, image                       string
	bin                               string
	boot                              int
	sysroot                           string
	extraEnv                          []string
}

func newRig(t *testing.T, sector0 []byte) *rig {
	t.Helper()
	// The script runs on the mixer's Linux and uses its tools' options
	// (stat -c, for one), which macOS's tools do not take.
	if runtime.GOOS != "linux" {
		t.Skip("the script needs Linux's tools")
	}
	bash, err := exec.LookPath("bash")
	if err != nil {
		t.Skip("bash is not installed")
	}
	for _, tool := range []string{"od", "dd", "cmp", "sha256sum", "sed", "tr", "cut", "stat", "grep"} {
		if _, err := exec.LookPath(tool); err != nil {
			t.Skipf("%s is not installed", tool)
		}
	}
	dir := t.TempDir()
	r := &rig{
		t: t, dir: dir,
		fake:  filepath.Join(dir, "fake"),
		root:  filepath.Join(dir, "root"),
		p2:    filepath.Join(dir, "p2"),
		imgfs: filepath.Join(dir, "imgfs"),
		ackfs: filepath.Join(dir, "ackfs"),
		bin:   filepath.Join(dir, "bin"),
	}
	r.sysroot = r.root
	r.disk = filepath.Join(r.fake, "mmcblk1")
	r.image = filepath.Join(r.fake, "usb-update.img")
	for _, d := range []string{r.fake, r.bin, r.imgfs, r.ackfs, filepath.Join(dir, "run")} {
		mkdir(t, d)
	}
	for _, sys := range []string{r.root, r.p2} {
		for _, d := range []string{"etc/init.d", "etc/rc5.d", "var/lib"} {
			mkdir(t, filepath.Join(sys, d))
		}
	}
	for name, body := range stubs {
		write(t, filepath.Join(r.bin, name), "#!"+bash+"\n"+body, 0o755)
	}
	f, err := os.Create(r.disk)
	if err != nil {
		t.Fatal(err)
	}
	if err := f.Truncate(diskSectors * 512); err != nil {
		t.Fatal(err)
	}
	if _, err := f.WriteAt(sector0, 0); err != nil {
		t.Fatal(err)
	}
	f.Close()
	write(t, r.image, "old contents\n", 0o644)
	write(t, filepath.Join(r.fake, "mounts"), "/dev/root / ext4 rw 0 0\n", 0o644)
	write(t, filepath.Join(r.fake, "env"), "bootcmd=run x\ndevplist=1\naltbootcmd=setenv devplist 2; run bootcmd\n", 0o644)
	write(t, filepath.Join(r.fake, "kernel_p1"), "1039458\n", 0o644)
	write(t, filepath.Join(r.fake, "kmsg"), "", 0o644)
	write(t, filepath.Join(r.fake, "calls"), "", 0o644)
	r.setSlot(2)
	r.install(r.root, Files())
	return r
}

func mkdir(t *testing.T, d string) {
	t.Helper()
	if err := os.MkdirAll(d, 0o755); err != nil {
		t.Fatal(err)
	}
}

func write(t *testing.T, path, data string, mode fs.FileMode) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(data), mode); err != nil {
		t.Fatal(err)
	}
}

func (r *rig) install(root string, files []File) {
	for _, f := range files {
		p := filepath.Join(root, filepath.FromSlash(f.Path))
		if f.Symlink {
			if err := os.Symlink(f.Target, p); err != nil {
				r.t.Fatal(err)
			}
			continue
		}
		if err := os.WriteFile(p, f.Data, f.Mode.Perm()); err != nil {
			r.t.Fatal(err)
		}
	}
}

func (r *rig) setSlot(n int) {
	write(r.t, filepath.Join(r.fake, "cmdline"),
		"console=ttyS0 root="+r.disk+"p"+string(rune('0'+n))+" rw rootwait\n", 0o644)
}

func (r *rig) flag(name string) { write(r.t, filepath.Join(r.fake, name), "", 0o644) }

func (r *rig) read(name string) string {
	b, err := os.ReadFile(filepath.Join(r.fake, name))
	if err != nil {
		r.t.Fatal(err)
	}
	return string(b)
}

// bootSystem runs the rc5.d links of the system at r.sysroot one after
// another, as sysvinit does, on a new boot id.
func (r *rig) bootSystem() {
	r.t.Helper()
	r.boot++
	write(r.t, filepath.Join(r.fake, "boot_id"), "boot-"+string(rune('a'+r.boot))+"\n", 0o644)
	env := append(os.Environ(),
		"FAKE="+r.fake,
		"FAKE_P2_DEV="+r.disk+"p2",
		"FAKE_P2_DIR="+r.p2,
		"FAKE_IMAGE="+r.image,
		"FAKE_IMGFS="+r.imgfs,
		"FAKE_ACK_IMAGE="+r.ackImage(),
		"FAKE_ACKFS="+r.ackfs,
		"RCFIX_PATH="+r.bin+string(os.PathListSeparator)+os.Getenv("PATH"),
		"RCFIX_SYSROOT="+r.sysroot,
		"RCFIX_DISK="+r.disk,
		"RCFIX_CMDLINE="+filepath.Join(r.fake, "cmdline"),
		"RCFIX_MOUNTS="+filepath.Join(r.fake, "mounts"),
		"RCFIX_BOOT_ID="+filepath.Join(r.fake, "boot_id"),
		"RCFIX_KMSG="+filepath.Join(r.fake, "kmsg"),
		"RCFIX_IMAGE="+r.image,
		"RCFIX_MNT_P2="+filepath.Join(r.dir, "run", "rcfix-p2"),
		"RCFIX_MNT_IMG="+filepath.Join(r.dir, "run", "rcfix-img"),
		"RCFIX_POLL_SECS=0.02",
		"RCFIX_QUIET_POLLS=2",
		"RCFIX_FALLBACK_POLLS=1000",
		"RCFIX_WAIT_POLLS=40",
		"RCFIX_REBOOT_WAIT=0",
		"RCFIX_FOREGROUND=1",
		"RCFIX_GADGET="+r.gadgetDir(),
		"RCFIX_ACK_IMG="+r.ackImage(),
		"RCFIX_MNT_ACK="+filepath.Join(r.dir, "run", "rcfix-ack"),
		"RCFIX_BOUND_SECS=0.2",
		"RCFIX_ATTACH_TRIES=3",
		"RCFIX_REPORT_SECS=2",
		"RCFIX_ACK_POLL_SECS=0.02",
		"RCFIX_DEBUG_DIR="+r.debugDir(),
		"RCFIX_TRACE_SECS=0.02",
	)
	env = append(env, r.extraEnv...)
	for _, n := range copyNames {
		link := filepath.Join(r.sysroot, "etc/rc5.d/S02rcfix-"+n)
		if _, err := os.Lstat(link); err != nil {
			continue // removed by an earlier copy; rc would just report it
		}
		ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
		cmd := exec.CommandContext(ctx, "bash", link, "start")
		cmd.Env = env
		cmd.Dir = r.dir
		out, err := cmd.CombinedOutput()
		timedOut := ctx.Err() != nil
		cancel()
		if timedOut {
			r.t.Fatalf("copy %s timed out:\n%s", n, out)
		}
		if err != nil {
			r.t.Logf("copy %s: %v\n%s", n, err, out)
		}
	}
}

func (r *rig) marker() Marker {
	r.t.Helper()
	b, err := os.ReadFile(filepath.Join(r.imgfs, MarkerName))
	if err != nil {
		r.t.Fatalf("no marker: %v\nkmsg:\n%s", err, r.read("kmsg"))
	}
	m, err := ParseMarker(b)
	if err != nil {
		r.t.Fatalf("%v:\n%s", err, b)
	}
	return m
}

func (r *rig) noMarker() {
	r.t.Helper()
	if _, err := os.Stat(filepath.Join(r.imgfs, MarkerName)); err == nil {
		r.t.Fatal("a marker was written")
	}
}

// clean fails when anything the script creates is left under root.
func (r *rig) clean(root string) {
	r.t.Helper()
	for _, p := range Paths() {
		if strings.HasPrefix(p, "run/") {
			p = filepath.Join(r.dir, p)
		} else {
			p = filepath.Join(root, p)
		}
		if _, err := os.Lstat(p); err == nil {
			r.t.Errorf("%s was left behind", p)
		}
	}
}

func (r *rig) sector0() []byte {
	f, err := os.Open(r.disk)
	if err != nil {
		r.t.Fatal(err)
	}
	defer f.Close()
	b := make([]byte, 512)
	if _, err := f.ReadAt(b, 0); err != nil {
		r.t.Fatal(err)
	}
	return b
}

func (r *rig) calls() string { return r.read("calls") }

func (r *rig) wroteTable() bool {
	return strings.Contains(r.calls(), "sfdisk --no-reread") || strings.Contains(r.calls(), "resizepart")
}

// envSet is the environment's lines in sorted order: restoring a variable
// moves it to the end of the fake file.
func envSet(s string) string {
	lines := strings.Split(strings.TrimSpace(s), "\n")
	sort.Strings(lines)
	return strings.Join(lines, "\n")
}

func p1Of(b []byte) uint32 { return binary.LittleEndian.Uint32(b[458:]) }

func TestScriptFixFromSlot2(t *testing.T) {
	before := mbr(1039458, 2000896)
	r := newRig(t, before)
	r.flag("mixer")
	r.bootSystem()

	m := r.marker()
	if m.Result != ResultFixed || m.Slot != 2 || m.Hopped || m.P1Sectors != 1193984 || len(m.Table) != 4 {
		t.Fatalf("marker %+v", m)
	}
	if !bytes.Equal(m.Sector0Before, before) {
		t.Fatal("sector0_before is not the sector 0 from before the fix")
	}
	after := r.sector0()
	if p1Of(after) != 1193984 || r.read("kernel_p1") != "1193984\n" {
		t.Fatalf("partition 1 is %d sectors, kernel %q", p1Of(after), r.read("kernel_p1"))
	}
	if !bytes.Equal(after[:446], before[:446]) || !bytes.Equal(after[462:], before[462:]) {
		t.Fatal("bytes outside entry 1 changed")
	}
	if n := strings.Count(r.calls(), "sfdisk --no-reread --no-tell-kernel -N 1"); n != 1 {
		t.Fatalf("sfdisk ran %d times:\n%s", n, r.calls())
	}
	r.clean(r.root)
}

func TestScriptAlreadyFixed(t *testing.T) {
	r := newRig(t, mbr(1193984, 2000896))
	r.flag("mixer")
	r.bootSystem()
	m := r.marker()
	if m.Result != ResultAlreadyFixed || m.Sector0Before != nil || m.P1Sectors != 1193984 {
		t.Fatalf("marker %+v", m)
	}
	if r.wroteTable() {
		t.Fatalf("table written:\n%s", r.calls())
	}
	r.clean(r.root)
}

func TestScriptUnrecognized(t *testing.T) {
	for _, slot := range []int{1, 2} {
		before := mbr(1039458, 2000897)
		r := newRig(t, before)
		r.setSlot(slot)
		r.flag("mixer")
		r.bootSystem()
		m := r.marker()
		if m.Result != ResultUnrecognized || !strings.Contains(m.Reason, "partition 2 start") ||
			len(m.Table) != 4 || m.Slot != slot {
			t.Fatalf("slot %d: marker %+v", slot, m)
		}
		if r.wroteTable() || strings.Contains(r.calls(), "fw_setenv") || r.rebooted() {
			t.Fatalf("slot %d: wrote something:\n%s", slot, r.calls())
		}
		if !bytes.Equal(r.sector0(), before) {
			t.Fatal("sector 0 changed")
		}
		r.clean(r.root)
	}
}

func TestScriptRestoresWrongReadBack(t *testing.T) {
	before := mbr(1039458, 2000896)
	r := newRig(t, before)
	r.flag("sfdisk_wrong")
	r.flag("mixer")
	r.bootSystem()
	m := r.marker()
	if m.Result != ResultFailed || !strings.Contains(m.Reason, "put back") || !bytes.Equal(m.Sector0Before, before) {
		t.Fatalf("marker %+v", m)
	}
	if !bytes.Equal(r.sector0(), before) {
		t.Fatal("sector 0 was not restored")
	}
	if m.P1Sectors != 1039458 || strings.Contains(r.calls(), "resizepart") {
		t.Fatalf("p1 %d, calls:\n%s", m.P1Sectors, r.calls())
	}
	r.clean(r.root)
}

func TestScriptRestoresFailedResizepart(t *testing.T) {
	before := mbr(1039458, 2000896)
	r := newRig(t, before)
	r.flag("resizepart_fail")
	r.flag("mixer")
	r.bootSystem()
	m := r.marker()
	if m.Result != ResultFailed || !strings.Contains(m.Reason, "resizepart failed") {
		t.Fatalf("marker %+v", m)
	}
	if !bytes.Equal(r.sector0(), before) {
		t.Fatal("sector 0 was not restored")
	}
}

func TestScriptRefusesDirtyGap(t *testing.T) {
	before := mbr(1039458, 2000896)
	r := newRig(t, before)
	f, err := os.OpenFile(r.disk, os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := f.WriteAt([]byte{1}, 1100000*512); err != nil {
		t.Fatal(err)
	}
	f.Close()
	r.flag("mixer")
	r.bootSystem()
	m := r.marker()
	if m.Result != ResultFailed || !strings.Contains(m.Reason, "not empty") || r.wroteTable() {
		t.Fatalf("marker %+v, calls:\n%s", m, r.calls())
	}
}

// From slot 1 the script copies itself to slot 2, switches U-Boot to it and
// restarts; on slot 2 it fixes partition 1.
func TestScriptHopThenFix(t *testing.T) {
	before := mbr(1039458, 2000896)
	r := newRig(t, before)
	r.setSlot(1)
	r.bootSystem()

	if r.wroteTable() {
		t.Fatalf("the table was written from slot 1:\n%s", r.calls())
	}
	if !r.rebooted() {
		t.Fatalf("no reboot:\n%s", r.calls())
	}
	env := r.read("env")
	for _, want := range []string{"devplist=2\n", "altbootcmd=setenv devplist 1; run bootcmd\n", "reboot_pwr_button_bypass=1\n"} {
		if !strings.Contains(env, want) {
			t.Errorf("env lacks %q:\n%s", want, env)
		}
	}
	r.clean(r.root)
	r.noMarker()
	for _, f := range Files() {
		p := filepath.Join(r.p2, filepath.FromSlash(f.Path))
		if f.Symlink {
			if target, err := os.Readlink(p); err != nil || target != f.Target {
				t.Fatalf("%s: link %q, %v", f.Path, target, err)
			}
			continue
		}
		b, err := os.ReadFile(p)
		if err != nil || !bytes.Equal(b, f.Data) {
			t.Fatalf("%s on slot 2 is not the script (%v)", f.Path, err)
		}
	}
	if _, err := os.Stat(filepath.Join(r.p2, "var/lib/rcfix/hopped")); err != nil {
		t.Fatal("no hop state on slot 2")
	}
	if strings.Contains(r.read("mounts"), "rcfix-p2") {
		t.Fatal("slot 2 left mounted")
	}

	r.sysroot = r.p2
	r.setSlot(2)
	r.flag("mixer")
	r.bootSystem()
	m := r.marker()
	if m.Result != ResultFixed || !m.Hopped || m.Slot != 2 || m.P1Sectors != 1193984 {
		t.Fatalf("marker %+v", m)
	}
	r.clean(r.p2)
}

func TestScriptHopArrivedOnSlot1(t *testing.T) {
	r := newRig(t, mbr(1039458, 2000896))
	r.setSlot(1)
	write(t, filepath.Join(r.root, "var/lib/rcfix/hopped"), "from=1\n", 0o644)
	r.flag("mixer")
	r.bootSystem()
	m := r.marker()
	if m.Result != ResultFailed || !m.Hopped || m.Slot != 1 {
		t.Fatalf("marker %+v", m)
	}
	if r.rebooted() || strings.Contains(r.calls(), "fw_setenv") || r.wroteTable() {
		t.Fatalf("acted:\n%s", r.calls())
	}
	r.clean(r.root)
}

func TestScriptHopRefused(t *testing.T) {
	cases := map[string]func(r *rig){
		"mount fails": func(r *rig) { r.flag("mount_p2_fail") },
		"no rc5.d":    func(r *rig) { os.RemoveAll(filepath.Join(r.p2, "etc/rc5.d")) },
		"env wrong":   func(r *rig) { r.flag("fw_wrong") },
		"devplist 2": func(r *rig) {
			write(r.t, filepath.Join(r.fake, "env"), "devplist=2\n", 0o644)
		},
	}
	for name, setup := range cases {
		t.Run(name, func(t *testing.T) {
			r := newRig(t, mbr(1039458, 2000896))
			r.setSlot(1)
			setup(r)
			envBefore := envSet(r.read("env"))
			r.flag("mixer")
			r.bootSystem()
			m := r.marker()
			if m.Result != ResultFailed || m.Slot != 1 || m.Hopped {
				t.Fatalf("marker %+v", m)
			}
			if r.rebooted() || r.wroteTable() {
				t.Fatalf("acted:\n%s", r.calls())
			}
			if env := r.read("env"); envSet(env) != envBefore {
				t.Fatalf("env not restored:\n%s", env)
			}
			if _, err := os.Lstat(filepath.Join(r.p2, "var/lib/rcfix")); err == nil {
				t.Fatal("rcfix state left on slot 2")
			}
			if _, err := os.Lstat(filepath.Join(r.p2, "etc/init.d/rcfix-a")); err == nil {
				t.Fatal("script left on slot 2")
			}
			r.clean(r.root)
		})
	}
}

// A copy that lost its tail, whether cut short or zero-filled, does nothing.
func TestScriptTruncatedCopies(t *testing.T) {
	full := Script()
	lastLine := bytes.LastIndexByte(full[:len(full)-1], '\n') + 1
	callLine := bytes.LastIndexByte(full[:lastLine-1], '\n') + 1
	cuts := []int{0, 1, len(full) / 10, len(full) / 3, len(full) / 2, len(full) * 9 / 10,
		callLine, callLine + len("rcfix_mai"), lastLine, lastLine + 10, len(full) - 2, len(full) - 1}
	for _, cut := range cuts {
		for _, zero := range []bool{false, true} {
			data := append([]byte(nil), full[:cut]...)
			if zero {
				data = append(data, make([]byte, len(full)-cut)...)
			}
			r := newRig(t, mbr(1039458, 2000896))
			var files []File
			for _, f := range Files() {
				if !f.Symlink {
					f.Data = data
				}
				files = append(files, f)
			}
			// Replace the copies newRig installed.
			for _, n := range copyNames {
				os.Remove(filepath.Join(r.root, "etc/init.d/rcfix-"+n))
				os.Remove(filepath.Join(r.root, "etc/rc5.d/S02rcfix-"+n))
			}
			r.install(r.root, files)
			r.flag("mixer")
			r.bootSystem()
			if c := r.calls(); c != "" {
				t.Fatalf("cut %d zero %v: commands ran:\n%s", cut, zero, c)
			}
			if _, err := os.Stat(filepath.Join(r.root, "var/lib/rcfix")); err == nil {
				t.Fatalf("cut %d zero %v: state directory created", cut, zero)
			}
			r.noMarker()
			for _, n := range copyNames {
				if _, err := os.Stat(filepath.Join(r.root, "etc/init.d/rcfix-"+n)); err != nil {
					t.Fatalf("cut %d zero %v: copy %s removed", cut, zero, n)
				}
			}
		}
	}
}

// With copy a damaged, copy b does the work, and c finds the lock.
func TestScriptDamagedFirstCopy(t *testing.T) {
	r := newRig(t, mbr(1039458, 2000896))
	full := Script()
	damaged := append(append([]byte(nil), full[:len(full)-30]...), make([]byte, 30)...)
	write(t, filepath.Join(r.root, "etc/init.d/rcfix-a"), string(damaged), 0o755)
	r.flag("mixer")
	r.bootSystem()
	if m := r.marker(); m.Result != ResultFixed {
		t.Fatalf("marker %+v", m)
	}
	if n := strings.Count(r.calls(), "sfdisk --no-reread"); n != 1 {
		t.Fatalf("sfdisk ran %d times", n)
	}
	// Copy a was damaged, not removed by the script's own check, but the
	// cleanup removes every name it installed.
	r.clean(r.root)
}

// Phase B waits for RØDE's program; without it, it gives up for this boot
// and keeps its state, and after five boots it removes itself.
func TestScriptPhaseBGivesUp(t *testing.T) {
	r := newRig(t, mbr(1039458, 2000896))
	for boot := 1; boot <= 5; boot++ {
		r.bootSystem()
		r.noMarker()
		if boot < 5 {
			b, err := os.ReadFile(filepath.Join(r.root, "var/lib/rcfix/boots"))
			if err != nil || strings.TrimSpace(string(b)) != string(rune('0'+boot)) {
				t.Fatalf("boot %d: boots file %q, %v", boot, b, err)
			}
			if _, err := os.Stat(filepath.Join(r.root, "var/lib/rcfix/result")); err != nil {
				t.Fatalf("boot %d: result not kept", boot)
			}
		}
	}
	r.clean(r.root)
	if n := strings.Count(r.calls(), "sfdisk --no-reread"); n != 1 {
		t.Fatalf("phase A ran %d times", n)
	}
}

// The result recorded in phase A survives to a later boot's phase B.
func TestScriptResultSurvivesReboot(t *testing.T) {
	before := mbr(1039458, 2000896)
	r := newRig(t, before)
	r.bootSystem() // RØDE's first boot restarts before its program runs
	r.noMarker()
	r.flag("mixer")
	r.bootSystem()
	m := r.marker()
	if m.Result != ResultFixed || !bytes.Equal(m.Sector0Before, before) {
		t.Fatalf("marker %+v", m)
	}
	r.clean(r.root)
}

func TestScriptMarkerIsWrittenAfterWipe(t *testing.T) {
	r := newRig(t, mbr(1193984, 2000896))
	r.flag("mixer")
	r.bootSystem()
	if b, err := os.ReadFile(r.image); err != nil || string(b) != "freshly formatted\n" {
		t.Fatalf("image %q, %v", b, err)
	}
	r.marker()
	if strings.Contains(r.read("mounts"), "rcfix-img") {
		t.Fatal("image left mounted")
	}
}

// rebooted is whether the reboot command itself was run (fw_setenv
// reboot_pwr_button_bypass is not that).
func (r *rig) rebooted() bool {
	for _, line := range strings.Split(r.calls(), "\n") {
		if line == "reboot" || strings.HasPrefix(line, "reboot ") {
			return true
		}
	}
	return false
}

// ---- The USB report ----

func (r *rig) gadgetDir() string { return filepath.Join(r.fake, "gadget") }
func (r *rig) ackImage() string  { return filepath.Join(r.dir, "run", "rcfix-ack.img") }
func (r *rig) lun() string {
	return filepath.Join(r.gadgetDir(), "functions", "mass_storage.usb0", "lun.0")
}

// gadget makes RØDE's normal-mode gadget as configfs shows it: bound to
// udc ("" for unbound), its disk slot empty and writable.
func (r *rig) gadget(udc string) {
	write(r.t, filepath.Join(r.gadgetDir(), "UDC"), udc+"\n", 0o644)
	write(r.t, filepath.Join(r.lun(), "file"), "\n", 0o644)
	write(r.t, filepath.Join(r.lun(), "ro"), "0\n", 0o644)
}

func (r *rig) lunAttr(name string) string {
	b, err := os.ReadFile(filepath.Join(r.lun(), name))
	if err != nil {
		r.t.Fatal(err)
	}
	return strings.TrimSuffix(string(b), "\n")
}

// hostSeen is what the host saw of the script's disk, one entry per
// attach.
type hostSeen struct {
	attaches int
	markers  []string
	writable []bool
	at       []time.Time
}

// host plays the computer on the other end of the disk slot while the
// script runs. Each time the script attaches its disk, the host reads the
// marker from it and does the next of acts: "ack" writes the app's
// acknowledgement onto the disk and ejects it, "empty" empties the slot
// without one (as something on the mixer does), "take" puts RØDE's image in
// the slot, and "" (or no act left) leaves the disk where it is. It returns
// a function that stops it and gives what it saw.
func (r *rig) host(acts ...string) func() hostSeen {
	stop, done := make(chan struct{}), make(chan struct{})
	var seen hostSeen
	go func() {
		defer close(done)
		attached := false
		for {
			select {
			case <-stop:
				return
			case <-time.After(5 * time.Millisecond):
			}
			b, err := os.ReadFile(filepath.Join(r.lun(), "file"))
			if err != nil || strings.TrimSpace(string(b)) != r.ackImage() {
				attached = false
				continue
			}
			if attached {
				continue
			}
			attached = true
			seen.attaches++
			seen.at = append(seen.at, time.Now())
			ro, _ := os.ReadFile(filepath.Join(r.lun(), "ro"))
			seen.writable = append(seen.writable, strings.TrimSpace(string(ro)) == "0")
			m, _ := os.ReadFile(filepath.Join(r.ackfs, MarkerName))
			seen.markers = append(seen.markers, string(m))
			time.Sleep(r.hostDelay)
			act := ""
			if seen.attaches <= len(acts) {
				act = acts[seen.attaches-1]
			}
			switch act {
			case "ack":
				os.WriteFile(filepath.Join(r.ackfs, AckName), []byte("ack=1\n"), 0o644)
				os.WriteFile(filepath.Join(r.lun(), "file"), []byte("\n"), 0o644)
			case "empty":
				os.WriteFile(filepath.Join(r.lun(), "file"), []byte("\n"), 0o644)
			case "take":
				os.WriteFile(filepath.Join(r.lun(), "file"), []byte("/rode/usb-update.img\n"), 0o644)
			case "ack-take":
				// The app acknowledges, and RØDE's program puts its own image
				// in the slot before the script has seen it empty.
				os.WriteFile(filepath.Join(r.ackfs, AckName), []byte("ack=1\n"), 0o644)
				os.WriteFile(filepath.Join(r.lun(), "file"), []byte("/rode/usb-update.img\n"), 0o644)
			}
		}
	}()
	return func() hostSeen {
		close(stop)
		<-done
		return seen
	}
}

func usbReportRig(t *testing.T) *rig {
	r := newRig(t, mbr(1039458, 2000896))
	r.extraEnv = append(r.extraEnv, "RCFIX_USB_REPORT=1")
	r.gadget("ci_hdrc.0")
	r.flag("mixer")
	return r
}

// slotGivenBack fails unless the slot is empty and writable, as RØDE's
// program leaves it, and the gadget is bound as before.
func (r *rig) slotGivenBack() {
	r.t.Helper()
	if f, ro := r.lunAttr("file"), r.lunAttr("ro"); f != "" || ro != "0" {
		r.t.Errorf("slot left with file %q, ro %q", f, ro)
	}
	if strings.Contains(r.read("mounts"), "rcfix-ack") {
		r.t.Error("the report disk was left mounted")
	}
	if udc := r.read("gadget/UDC"); udc != "ci_hdrc.0\n" {
		r.t.Errorf("the gadget's UDC is %q", udc)
	}
}

func (r *rig) reportMarker(text string) Marker {
	r.t.Helper()
	m, err := ParseMarker([]byte(text))
	if err != nil {
		r.t.Fatalf("the report disk held %q: %v", text, err)
	}
	return m
}

// The app acknowledges the report: the script finishes without touching
// the update drive, and gives the slot back as it found it.
func TestScriptUSBReportAcknowledged(t *testing.T) {
	r := usbReportRig(t)
	stop := r.host("ack")
	r.bootSystem()
	seen := stop()
	if seen.attaches != 1 {
		t.Fatalf("the report disk was attached %d times", seen.attaches)
	}
	m := r.reportMarker(seen.markers[0])
	if m.Result != ResultFixed || m.Sector0Before == nil || m.Script != ScriptVersion {
		t.Fatalf("marker %+v", m)
	}
	if !seen.writable[0] {
		t.Error("the report disk was attached read-only, so the app could not acknowledge it")
	}
	r.noMarker()
	r.slotGivenBack()
	r.clean(r.root)
}

// The slot is emptied without the app's acknowledgement, as happened on a
// mixer just after the gadget was bound: the script offers the disk again,
// once the gadget has been bound for its wait, and finishes when the app
// acknowledges it.
func TestScriptUSBReportEmptiedThenAcknowledged(t *testing.T) {
	r := usbReportRig(t)
	stop := r.host("empty", "ack")
	r.bootSystem()
	seen := stop()
	if seen.attaches != 2 {
		t.Fatalf("the report disk was attached %d times", seen.attaches)
	}
	for _, text := range seen.markers {
		if m := r.reportMarker(text); m.Result != ResultFixed {
			t.Fatalf("marker %+v", m)
		}
	}
	if d := seen.at[1].Sub(seen.at[0]); d < 180*time.Millisecond {
		t.Errorf("attached again %s after it was emptied, before the gadget had been bound for 0.2 s", d)
	}
	r.noMarker()
	r.slotGivenBack()
	r.clean(r.root)
}

// The slot is emptied without an acknowledgement every time: after three
// attaches the script writes the marker to the update drive as before.
func TestScriptUSBReportNeverAcknowledged(t *testing.T) {
	r := usbReportRig(t)
	stop := r.host("empty", "empty", "empty", "ack")
	r.bootSystem()
	if seen := stop(); seen.attaches != 3 {
		t.Fatalf("the report disk was attached %d times, want 3", seen.attaches)
	}
	if m := r.marker(); m.Result != ResultFixed {
		t.Fatalf("marker %+v", m)
	}
	r.slotGivenBack()
	r.clean(r.root)
}

// Nobody touches the report disk: the script detaches it after its limit
// and writes the marker to the update drive as before.
func TestScriptUSBReportTimesOut(t *testing.T) {
	r := usbReportRig(t)
	stop := r.host("")
	r.bootSystem()
	if seen := stop(); seen.attaches != 1 {
		t.Fatalf("the report disk was attached %d times", seen.attaches)
	}
	if m := r.marker(); m.Result != ResultFixed {
		t.Fatalf("marker %+v", m)
	}
	r.slotGivenBack()
	r.clean(r.root)
}

// RØDE's program takes the slot while the report disk is in it: the
// script leaves the slot to it and writes the marker to the update drive.
func TestScriptUSBReportSlotTaken(t *testing.T) {
	r := usbReportRig(t)
	stop := r.host("take")
	r.bootSystem()
	if seen := stop(); seen.attaches != 1 {
		t.Fatalf("the report disk was attached %d times", seen.attaches)
	}
	if m := r.marker(); m.Result != ResultFixed {
		t.Fatalf("marker %+v", m)
	}
	if f := r.lunAttr("file"); f != "/rode/usb-update.img" {
		t.Errorf("the script changed RØDE's slot to %q", f)
	}
	r.clean(r.root)
}

// The app acknowledges and the mixer goes to update mode at once, so
// RØDE's program has the slot before the script looks: the acknowledgement
// still counts, and the update drive is left alone.
func TestScriptUSBReportAcknowledgedThenTaken(t *testing.T) {
	r := usbReportRig(t)
	stop := r.host("ack-take")
	r.bootSystem()
	if seen := stop(); seen.attaches != 1 {
		t.Fatalf("the report disk was attached %d times", seen.attaches)
	}
	r.noMarker()
	if f := r.lunAttr("file"); f != "/rode/usb-update.img" {
		t.Errorf("the script changed RØDE's slot to %q", f)
	}
	r.clean(r.root)
}

// The disk goes into the slot only once the gadget has been bound for the
// whole wait without a break: while the gadget is bound and unbound over
// and over, the wait starts again each time.
func TestScriptUSBReportWaitsForBinding(t *testing.T) {
	r := usbReportRig(t)
	r.extraEnv = append(r.extraEnv, "RCFIX_BOUND_SECS=0.5", "RCFIX_REPORT_SECS=6")
	udc := filepath.Join(r.gadgetDir(), "UDC")
	flapped := make(chan time.Time, 1)
	go func() {
		for i := 0; i < 9; i++ {
			v := "ci_hdrc.0\n"
			if i%2 == 0 {
				v = "\n"
			}
			os.WriteFile(udc, []byte(v), 0o644)
			time.Sleep(100 * time.Millisecond)
		}
		os.WriteFile(udc, []byte("ci_hdrc.0\n"), 0o644)
		flapped <- time.Now()
	}()
	stop := r.host("ack")
	r.bootSystem()
	seen := stop()
	stable := <-flapped
	if seen.attaches != 1 {
		t.Fatalf("the report disk was attached %d times", seen.attaches)
	}
	if d := seen.at[0].Sub(stable); d < 450*time.Millisecond {
		t.Errorf("attached %s after the gadget was last bound, before 0.5 s of binding", d)
	}
	r.noMarker()
	r.slotGivenBack()
	r.clean(r.root)
}

// Without a usable slot, or without being asked, the script never attaches
// a report disk (nor makes one, unless making it is what failed) and works
// as before.
func TestScriptUSBReportNoSlot(t *testing.T) {
	cases := map[string]func(r *rig){
		"no gadget":  func(r *rig) { os.RemoveAll(r.gadgetDir()) },
		"unbound":    func(r *rig) { r.gadget("") },
		"slot busy":  func(r *rig) { write(r.t, filepath.Join(r.lun(), "file"), "/rode/usb-update.img\n", 0o644) },
		"read-only":  func(r *rig) { write(r.t, filepath.Join(r.lun(), "ro"), "1\n", 0o644) },
		"mkfs fails": func(r *rig) { r.flag("mkfs_fail") },
		"not asked":  func(r *rig) { r.extraEnv = nil },
	}
	for name, setup := range cases {
		t.Run(name, func(t *testing.T) {
			r := usbReportRig(t)
			setup(r)
			before := ""
			if _, err := os.Stat(r.lun()); err == nil {
				before = r.lunAttr("file") + "|" + r.lunAttr("ro")
			}
			stop := r.host("ack")
			r.bootSystem()
			if seen := stop(); seen.attaches != 0 {
				t.Errorf("the report disk was attached %d times", seen.attaches)
			}
			if m := r.marker(); m.Result != ResultFixed {
				t.Fatalf("marker %+v", m)
			}
			if _, err := os.Stat(r.lun()); err == nil {
				if after := r.lunAttr("file") + "|" + r.lunAttr("ro"); after != before {
					t.Errorf("slot went from %q to %q", before, after)
				}
			} else if name != "no gadget" {
				t.Fatal(err)
			}
			if name != "mkfs fails" && strings.Contains(r.calls(), "mkfs.vfat") {
				t.Errorf("a report disk was made:\n%s", r.calls())
			}
			r.clean(r.root)
		})
	}
}

func (r *rig) debugDir() string { return filepath.Join(r.dir, "debug") }

// With the debug log asked for, phase B leaves its log and a trace of the
// slot under the debug folder, which the script's cleanup leaves alone.
func TestScriptDebugLog(t *testing.T) {
	r := usbReportRig(t)
	r.extraEnv = append(r.extraEnv, "RCFIX_DEBUG=1")
	// Longer than the trace's 0.2 s sampling, so the attached disk is seen.
	r.hostDelay = 600 * time.Millisecond
	stop := r.host("empty", "ack")
	r.bootSystem()
	stop()
	r.noMarker()
	r.clean(r.root)
	entries, err := os.ReadDir(r.debugDir())
	if err != nil || len(entries) != 1 {
		t.Fatalf("debug folder holds %v (%v)", entries, err)
	}
	if name := entries[0].Name(); name != "boot-b.log" {
		t.Errorf("debug log is named %s, want the boot id", name)
	}
	b, err := os.ReadFile(filepath.Join(r.debugDir(), entries[0].Name()))
	if err != nil {
		t.Fatal(err)
	}
	log := string(b)
	for _, want := range []string{
		"phase A: slot 2",
		"trace file=" + r.ackImage() + " ro=0 udc=ci_hdrc.0",
		"trace file= ro=0 udc=ci_hdrc.0",
		"report disk attached (2 of 3)",
		"emptied without the app's acknowledgement",
		"the app acknowledged the report",
		"phase B ended",
	} {
		if !strings.Contains(log, want) {
			t.Errorf("debug log lacks %q:\n%s", want, log)
		}
	}
}

// Without being asked, the script keeps no debug log.
func TestScriptNoDebugLog(t *testing.T) {
	r := usbReportRig(t)
	stop := r.host("ack")
	r.bootSystem()
	stop()
	if _, err := os.Stat(r.debugDir()); err == nil {
		t.Error("a debug folder was made")
	}
}
