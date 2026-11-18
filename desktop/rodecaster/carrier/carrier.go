// Package carrier holds the one-shot script that grows partition 1 on the
// mixer itself, the files that put it into a firmware archive, and the parser
// for the marker file it leaves on the mixer's USB update drive.
//
// The script is installed as three identical copies, each started by its own
// /etc/rc5.d link before RØDE's audio service. Its last line carries an end
// token and the SHA-256 of every line before it, computed here when the files
// are made; a copy whose tail was lost to the 1 KB-block bug fails that check
// and does nothing. The hash lives in the file itself rather than in a second
// file because a second file could lose its own tail, and because one file
// whose last line must be intact is the simplest thing to check on the mixer
// with sed and sha256sum.
//
// With Options.USBReport the script also offers the marker on a small disk
// of its own in the empty disk slot of RØDE's normal-mode USB gadget, so an
// app that can read that disk gets the result as soon as the mixer is back
// instead of switching it to update mode to read the update drive. The app
// acknowledges it by writing AckName onto that disk and ejecting it; the
// script finishes when its slot is empty and its own disk holds AckName. A
// slot emptied without it (on a mixer something empties the slot just after
// the gadget is bound) gets the disk again, a few times within a limit;
// after that the script detaches its disk and writes the marker to the
// update drive as it always did. Only an app that reads, acknowledges and
// ejects the disk may ask for it.
//
// With Options.DebugLog (development builds of the app only) the script
// also keeps a log of phase B and a trace of the disk slot under
// /Application/update-staging/rcfix-debug on the mixer, which nothing
// removes.
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
	// MarkerName is the file the script writes to the root of the update
	// drive.
	MarkerName = "RCFIX.TXT"
	// AckName is the file the app writes to the root of the script's USB
	// disk to say it has the marker.
	AckName = "RCFIX.ACK"
	// ScriptVersion is what the script writes as script= in the marker.
	ScriptVersion = "4"

	eofToken = "# rcfix-eof sha256="
	initDir  = "etc/init.d"
	rcDir    = "etc/rc5.d"
	stateDir = "var/lib/rcfix"
)

var copyNames = []string{"a", "b", "c"}

// Options change what the installed script does.
type Options struct {
	// USBReport: the script offers the marker on the normal-mode USB disk
	// slot first (see the package comment).
	USBReport bool
	// DebugLog: the script keeps its debug log on the mixer (see the
	// package comment). For development builds only.
	DebugLog bool
}

// usbReportOff and usbReportOn are the script's line that holds the
// USBReport default; debugOff and debugOn the one for DebugLog.
const (
	usbReportOff = "RCFIX_USB_REPORT=${RCFIX_USB_REPORT:-0}\n"
	usbReportOn  = "RCFIX_USB_REPORT=${RCFIX_USB_REPORT:-1}\n"
	debugOff     = "RCFIX_DEBUG=${RCFIX_DEBUG:-0}\n"
	debugOn      = "RCFIX_DEBUG=${RCFIX_DEBUG:-1}\n"
)

// File is one entry to add to a firmware archive, relative to the root of
// the system it is extracted onto.
type File struct {
	Path string
	// Mode holds the permission bits; for a symlink it also has
	// fs.ModeSymlink.
	Mode    fs.FileMode
	Symlink bool
	// Target is the link target (relative), only for a symlink.
	Target string
	// Data is the file's contents, nil for a symlink.
	Data []byte
}

// Script returns the script as it is installed with the default options:
// the source followed by the line holding the end token and the SHA-256 of
// everything before it.
func Script() []byte { return ScriptFor(Options{}) }

// ScriptFor is Script with options o, which are written into the script
// before its hash.
func ScriptFor(o Options) []byte {
	src := append([]byte(nil), scriptSource...)
	if len(src) == 0 || src[len(src)-1] != '\n' {
		src = append(src, '\n')
	}
	if o.USBReport {
		// The line is checked by the tests; a source without it would
		// install a script that never reports over USB, which is safe.
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

// Files returns what must be added to a firmware archive: the three script
// copies and their rc5.d links, with the default options.
func Files() []File { return FilesFor(Options{}) }

// FilesFor is Files with options o.
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

// Paths lists every path, relative to a system root, that the script may
// create on a mixer, including its state directory, its contents and the
// mount points and the report disk's image under /run. When the script has
// finished, none of them exists.
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

// stateFiles are the names the script uses inside its state directory.
var stateFiles = []string{
	"lock", "boots", "log", "result", "result.new", "hopped",
	"hop-attempted", "sector0.bin", "sector0.after", "cmp", "marker",
}
