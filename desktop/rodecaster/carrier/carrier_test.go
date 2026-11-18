package carrier

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"io/fs"
	"strings"
	"testing"
)

func TestFiles(t *testing.T) {
	files := Files()
	if len(files) != 6 {
		t.Fatalf("got %d files, want 6", len(files))
	}
	script := Script()
	for i, n := range copyNames {
		f := files[i]
		if f.Path != "etc/init.d/rcfix-"+n || f.Symlink || f.Mode != 0o755 || f.Target != "" {
			t.Errorf("copy %s: %+v", n, f)
		}
		if !bytes.Equal(f.Data, script) {
			t.Errorf("copy %s differs from Script()", n)
		}
		l := files[3+i]
		if l.Path != "etc/rc5.d/S02rcfix-"+n || !l.Symlink || l.Target != "../init.d/rcfix-"+n ||
			l.Mode != fs.ModeSymlink|0o777 || l.Data != nil {
			t.Errorf("link %s: %+v", n, l)
		}
	}
	// Each copy owns its data: changing one must not change another.
	files[0].Data[0] ^= 1
	if bytes.Equal(files[0].Data, files[1].Data) {
		t.Error("copies share their data")
	}
}

func TestScriptEnd(t *testing.T) {
	s := Script()
	if !bytes.HasSuffix(s, []byte("\n")) {
		t.Fatal("script does not end with a newline")
	}
	lines := strings.Split(strings.TrimSuffix(string(s), "\n"), "\n")
	last := lines[len(lines)-1]
	if !strings.HasPrefix(last, eofToken) {
		t.Fatalf("last line %q lacks the end token", last)
	}
	if call := lines[len(lines)-2]; call != `rcfix_main "$@"` {
		t.Fatalf("second-to-last line is %q, want the call to rcfix_main", call)
	}
	body := s[:len(s)-len(last)-1]
	sum := sha256.Sum256(body)
	if got := strings.TrimPrefix(last, eofToken); got != hex.EncodeToString(sum[:]) {
		t.Fatalf("hash line %s does not match the body %x", got, sum)
	}
	if bytes.IndexByte(s, 0) >= 0 {
		t.Fatal("script contains a NUL byte")
	}
	for i, c := range s {
		if c >= 0x80 {
			t.Fatalf("byte %d of the script is not ASCII", i)
		}
	}
}

// The script and the Go side must agree on names and paths.
func TestScriptAgrees(t *testing.T) {
	src := string(scriptSource)
	for _, want := range []string{
		"RCFIX_VERSION=" + ScriptVersion + "\n",
		"MARKER=" + MarkerName + "\n",
		"ACK=" + AckName + "\n",
		"REL_INIT=" + initDir + "\n",
		"REL_RC=" + rcDir + "\n",
		"REL_STATE=" + stateDir + "\n",
		`NAMES="` + strings.Join(copyNames, " ") + `"` + "\n",
		`EOF_TOKEN="` + eofToken + `"` + "\n",
		"/run/rcfix-p2}", "/run/rcfix-img}", "/run/rcfix-ack.img}", "/run/rcfix-ack}",
		usbReportOff,
		debugOff,
		`ln -s "../init.d/rcfix-$n"`,
	} {
		if !strings.Contains(src, want) {
			t.Errorf("script lacks %q", want)
		}
	}
	for _, f := range stateFiles {
		used := false
		for _, dir := range []string{"$STATE_DIR/", "$REL_STATE/"} {
			for _, end := range []string{`"`, " ", "\n"} {
				used = used || strings.Contains(src, dir+f+end)
			}
		}
		if !used {
			t.Errorf("state file %q is not used by the script", f)
		}
	}
}

func TestPaths(t *testing.T) {
	paths := Paths()
	seen := map[string]bool{}
	for _, p := range paths {
		if seen[p] {
			t.Errorf("duplicate path %s", p)
		}
		seen[p] = true
		if strings.HasPrefix(p, "/") {
			t.Errorf("path %s is absolute", p)
		}
	}
	for _, f := range Files() {
		if !seen[f.Path] {
			t.Errorf("Paths lacks %s", f.Path)
		}
	}
	if !seen[stateDir] {
		t.Errorf("Paths lacks %s", stateDir)
	}
}

// The USB report is off unless asked for, and asking changes only its
// default, before the hash.
func TestScriptForUSBReport(t *testing.T) {
	if n := strings.Count(string(scriptSource), usbReportOff); n != 1 {
		t.Fatalf("the source holds the USB report line %d times", n)
	}
	off, on := Script(), ScriptFor(Options{USBReport: true})
	if !bytes.Contains(off, []byte(usbReportOff)) || bytes.Contains(off, []byte(usbReportOn)) {
		t.Error("the default script reports over USB")
	}
	if !bytes.Contains(on, []byte(usbReportOn)) || bytes.Contains(on, []byte(usbReportOff)) {
		t.Error("the USB report script does not report over USB")
	}
	body := func(s []byte) []byte { return s[:bytes.LastIndex(s, []byte(eofToken))] }
	if got := bytes.Replace(body(on), []byte(usbReportOn), []byte(usbReportOff), 1); !bytes.Equal(got, body(off)) {
		t.Error("the two scripts differ in more than the USB report line")
	}
	sum := sha256.Sum256(body(on))
	if !bytes.HasSuffix(on, []byte(eofToken+hex.EncodeToString(sum[:])+"\n")) {
		t.Error("the USB report script's hash is not of its own body")
	}
	for _, f := range FilesFor(Options{USBReport: true}) {
		if !f.Symlink && !bytes.Equal(f.Data, on) {
			t.Errorf("%s is not the USB report script", f.Path)
		}
	}
}

// The debug log is off unless asked for, and asking changes only its
// default, before the hash.
func TestScriptForDebugLog(t *testing.T) {
	if n := strings.Count(string(scriptSource), debugOff); n != 1 {
		t.Fatalf("the source holds the debug line %d times", n)
	}
	off, on := ScriptFor(Options{USBReport: true}), ScriptFor(Options{USBReport: true, DebugLog: true})
	if !bytes.Contains(off, []byte(debugOff)) || bytes.Contains(off, []byte(debugOn)) {
		t.Error("the default script keeps a debug log")
	}
	if !bytes.Contains(on, []byte(debugOn)) || bytes.Contains(on, []byte(debugOff)) {
		t.Error("the debug script does not keep a debug log")
	}
	body := func(s []byte) []byte { return s[:bytes.LastIndex(s, []byte(eofToken))] }
	if got := bytes.Replace(body(on), []byte(debugOn), []byte(debugOff), 1); !bytes.Equal(got, body(off)) {
		t.Error("the two scripts differ in more than the debug line")
	}
	sum := sha256.Sum256(body(on))
	if !bytes.HasSuffix(on, []byte(eofToken+hex.EncodeToString(sum[:])+"\n")) {
		t.Error("the debug script's hash is not of its own body")
	}
	// The debug log lives outside every path the script removes.
	for _, p := range Paths() {
		if strings.HasPrefix(p, "Application/") {
			t.Errorf("Paths lists %s", p)
		}
	}
}
