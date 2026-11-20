// Package carrier bundles the on-device init script that grows partition 1,
// builds the tar entries to inject into a firmware archive, and parses the
// marker file the script leaves behind.
//
// Three identical copies of the script are installed under /etc/init.d and
// linked from /etc/rc5.d ahead of RØDE's audio service. Each copy ends with a
// trailing EOF token carrying the SHA-256 of everything above it; if the 1 KB
// ext-block bug truncates a copy's tail, its self-check fails and it exits
// harmlessly. Embedding the hash in the script itself avoids relying on a
// separate checksum file (which could also get truncated) and is easy to verify
// on-device with sed and sha256sum.
//
// When Options.USBReport is set, the script first exposes the marker on a tiny
// FAT image in the empty LUN of RØDE's normal-mode USB gadget. The host reads
// the marker, writes AckName, and ejects the disk to tell the script it's done
// (avoiding an update-mode switch). If the LUN is emptied without AckName (which
// happens briefly on real hardware right after gadget bind), the script re-attaches
// a few times before falling back to writing the marker on the update drive.
//
// With Options.DebugLog (dev builds only), the script also preserves a phase B
// log and LUN trace under /Application/update-staging/rcfix-debug.
package carrier

import (
	"bytes"
	"crypto/sha256"
	_ "embed"
	"encoding/hex"
	"io/fs"
)

//go:embed rcfix.sh
var scriptSource []byte

const (
	MarkerName    = "RCFIX.TXT" // marker written to the update drive or report disk
	AckName       = "RCFIX.ACK" // host acknowledgement written to the report disk
	ScriptVersion = "4"         // reported as script= in RCFIX.TXT

	eofToken = "# rcfix-eof sha256="
	initDir  = "etc/init.d"
	rcDir    = "etc/rc5.d"
	stateDir = "var/lib/rcfix"
)

var copyNames = []string{"a", "b", "c"}

// Options configures feature flags baked into the generated carrier script.
type Options struct {
	USBReport bool // offer marker over the normal-mode USB gadget LUN first
	DebugLog  bool // keep phase B debug logs on the mixer (dev builds only)
}

const (
	usbReportOff = "RCFIX_USB_REPORT=${RCFIX_USB_REPORT:-0}\n"
	usbReportOn  = "RCFIX_USB_REPORT=${RCFIX_USB_REPORT:-1}\n"
	debugOff     = "RCFIX_DEBUG=${RCFIX_DEBUG:-0}\n"
	debugOn      = "RCFIX_DEBUG=${RCFIX_DEBUG:-1}\n"
)

// File describes a rootfs-relative entry to inject into a firmware archive.
type File struct {
	Path    string
	Mode    fs.FileMode
	Symlink bool
	Target  string // relative link target when Symlink is true
	Data    []byte // nil for symlinks
}

// Script returns the carrier script with default options and its trailing SHA-256 footer.
func Script() []byte { return ScriptFor(Options{}) }

// ScriptFor returns the carrier script with o baked in before computing the footer hash.
func ScriptFor(o Options) []byte {
	src := append([]byte(nil), scriptSource...)
	if len(src) == 0 || src[len(src)-1] != '\n' {
		src = append(src, '\n')
	}
	if o.USBReport {
		// Tested by TestScriptForUSBReport; a missing line safely leaves reporting off.
		src = bytes.Replace(src, []byte(usbReportOff), []byte(usbReportOn), 1)
	}
	if o.DebugLog {
		src = bytes.Replace(src, []byte(debugOff), []byte(debugOn), 1)
	}
	sum := sha256.Sum256(src)
	out := make([]byte, 0, len(src)+len(eofToken)+65)
	out = append(out, src...)
	out = append(out, eofToken...)
	out = append(out, hex.EncodeToString(sum[:])...)
	out = append(out, '\n')
	return out
}

// Files returns the three init scripts and their rc5.d symlinks with default options.
func Files() []File { return FilesFor(Options{}) }

// FilesFor returns the three init scripts and their rc5.d symlinks with options o.
func FilesFor(o Options) []File {
	script := ScriptFor(o)
	var files []File
	for _, n := range copyNames {
		files = append(files, File{
			Path: initDir + "/rcfix-" + n,
			Mode: 0o755,
			Data: append([]byte(nil), script...),
		})
	}
	for _, n := range copyNames {
		files = append(files, File{
			Path:    rcDir + "/S02rcfix-" + n,
			Mode:    fs.ModeSymlink | 0o777,
			Symlink: true,
			Target:  "../init.d/rcfix-" + n,
		})
	}
	return files
}

// Paths returns every rootfs-relative path the carrier script can create on the
// mixer, including its state dir, mount points, and temp images under /run.
func Paths() []string {
	var paths []string
	for _, n := range copyNames {
		paths = append(paths,
			initDir+"/rcfix-"+n,
			initDir+"/rcfix-"+n+".new",
			rcDir+"/S02rcfix-"+n,
		)
	}
	for _, f := range stateFiles {
		paths = append(paths, stateDir+"/"+f)
	}
	return append(paths, stateDir, "run/rcfix-p2", "run/rcfix-img", "run/rcfix-ack.img", "run/rcfix-ack")
}

var stateFiles = []string{
	"lock", "boots", "log", "result", "result.new", "hopped",
	"hop-attempted", "sector0.bin", "sector0.after", "cmp", "marker",
}
