package mixer

import (
	"bufio"
	"bytes"
	"encoding/json"
	"strconv"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/internal/common"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
	"github.com/ibanks42/rc2fx/desktop/usbcontrol"
	"github.com/ibanks42/rc2fx/desktop/usbproto"
)

func rc2fxSh(t *testing.T) string {
	t.Helper()
	text, err := script(rc2fxShPath)
	if err != nil {
		t.Fatal(err)
	}
	return text
}

func TestEveryFlagChangeIsSyncedForAPowerCut(t *testing.T) {
	for name, s := range map[string]string{"enable": enableScript, "disable": disableScript} {
		if !strings.Contains(s, "\nsync\n") {
			t.Errorf("%s must sync its flag change", name)
		}
	}
}

// The upload's modes, as they were when each file had its own line: the
// shims run by RØDE's scripts are world-executable, the other executables
// and scripts root's alone, libraries and data not executable.
func TestEveryUploadedFileKeepsItsMode(t *testing.T) {
	want := map[string]int64{
		"librc2fx_native.so":     0o600,
		"rc2fxd":                 0o700,
		"rc2fx-supervisor":       0o700,
		"rc2fx-firmware":         0o700,
		"firmware-profiles.json": 0o600,
		"librc2fx_gui.so":        0o600,
		"librc2fx_jackerr.so":    0o600,
		"rc2fx_jackerr_snapshot": 0o700,
		"mixer-release.json":     0o600,
		"SHA256SUMS":             0o600,
		"rc2fx.sh":               0o700,
		"rc2fx-shim.sh":          0o755,
		"rc2fx-gui-shim.sh":      0o755,
		"install.sh":             0o700,
	}
	if len(want) != len(embed.PayloadNames) {
		t.Fatalf("the payload has %d files; this test knows %d", len(embed.PayloadNames), len(want))
	}
	for _, name := range embed.PayloadNames {
		mode, ok := want[name]
		if !ok {
			t.Fatalf("no expected mode for %s", name)
		}
		if got := uploadMode(name); got != mode {
			t.Errorf("%s: mode %o, want %o", name, got, mode)
		}
	}
}

func TestMissingLauncherIsNotReportedAsSuccess(t *testing.T) {
	s := onMixer("status")
	if !strings.Contains(s, "/run/rc2fx-dev/rc2fx.sh /opt/rc2fx/rc2fx.sh") || !strings.HasSuffix(s, "exit 1\n") {
		t.Fatal(s)
	}
}

type countingOutput struct {
	bytes.Buffer
	flushes int
}

func (o *countingOutput) Flush() error {
	o.flushes++
	return nil
}

func TestAPISessionPreservesOneRequesterAndFlushesEachReply(t *testing.T) {
	var output countingOutput
	calls := 0
	input := "{\"method\":\"GET\",\"path\":\"api/v1/state\"}\n{\"method\":\"GET\",\"path\":\"api/v1/state\"}\n"
	err := apiStream(bufio.NewReader(strings.NewReader(input)), &output, func(request usbcontrol.Request) (*usbcontrol.Reply, error) {
		calls++
		if request.Path != "api/v1/state" {
			t.Errorf("path %q", request.Path)
		}
		return &usbcontrol.Reply{Status: 200, Body: json.RawMessage(`{"call":` + strconv.Itoa(calls) + `}`)}, nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if calls != 2 || output.flushes != 2 {
		t.Fatalf("calls %d flushes %d", calls, output.flushes)
	}
	if n := len(common.Lines(output.String())); n != 2 {
		t.Fatalf("%d lines", n)
	}
}

func TestAPISessionDoesNotReplayTransportFailures(t *testing.T) {
	calls := 0
	err := apiStream(bufio.NewReader(strings.NewReader("{\"method\":\"PUT\",\"path\":\"api/v1/config\"}\n")), &countingOutput{},
		func(usbcontrol.Request) (*usbcontrol.Reply, error) {
			calls++
			return nil, task.Error("outcome unknown")
		})
	if err == nil || err.Error() != "outcome unknown" || calls != 1 {
		t.Fatalf("%v, %d calls", err, calls)
	}
}

// stock is RØDE's Pro II 1.7.6/1.7.7 HID report descriptor.
var stock = []byte{
	0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x75, 0x08, 0x15, 0x00, 0x25, 0xff, 0x09, 0x02,
	0x85, 0x01, 0x95, 0x3f, 0x91, 0x02, 0x09, 0x03, 0x85, 0x02, 0x95, 0x3f, 0x81, 0x02, 0x09,
	0x04, 0x85, 0x03, 0x95, 0xff, 0x91, 0x02, 0x09, 0x05, 0x85, 0x04, 0x95, 0xff, 0x81, 0x02,
	0xc0,
}

func prepareUSB(t *testing.T) string {
	t.Helper()
	text := rc2fxSh(t)
	start := strings.Index(text, "prepare_usb() {")
	if start < 0 {
		t.Fatal("prepare_usb in rc2fx.sh")
	}
	body := text[start:]
	end := strings.Index(body, "\n}\n")
	if end < 0 {
		t.Fatal("end of prepare_usb")
	}
	return body[:end]
}

// staged is the descriptor prepare_usb stages from the stock one: its `head
// -c` prefix followed by its `printf` bytes.
func staged(t *testing.T) []byte {
	body := prepareUSB(t)
	parts := strings.SplitN(body, "head -c ", 3)
	if len(parts) < 2 {
		t.Fatal("head -c")
	}
	head, err := strconv.Atoi(strings.Fields(parts[1])[0])
	if err != nil {
		t.Fatal(err)
	}
	var printf string
	for _, line := range common.Lines(body) {
		if strings.HasPrefix(strings.TrimLeft(line, " \t"), `printf '\x`) {
			printf = line
			break
		}
	}
	if printf == "" {
		t.Fatal("descriptor printf")
	}
	escapes := strings.Split(printf, "'")[1]
	out := append([]byte{}, stock[:head]...)
	for _, h := range strings.Split(escapes, `\x`)[1:] {
		b, err := strconv.ParseUint(h, 16, 8)
		if err != nil {
			t.Fatal(err)
		}
		out = append(out, byte(b))
	}
	return out
}

// items is the descriptor's short-item prefixes, in order.
func items(t *testing.T, descriptor []byte) []byte {
	i := 0
	var prefixes []byte
	for i < len(descriptor) {
		prefix := descriptor[i]
		prefixes = append(prefixes, prefix)
		i += 1 + []int{0, 1, 2, 4}[prefix&3]
	}
	if i != len(descriptor) {
		t.Fatal("descriptor ends mid-item")
	}
	return prefixes
}

func TestStagedDescriptorKeepsRodesItemsInOneCollection(t *testing.T) {
	descriptor := staged(t)
	if len(descriptor) != 62 {
		t.Fatalf("%d bytes", len(descriptor))
	}
	if !bytes.Equal(descriptor[:len(stock)-1], stock[:len(stock)-1]) {
		t.Fatal("the stock items changed")
	}
	prefixes := items(t, descriptor)
	// One application collection, closed once at the very end: Windows
	// lists RØDE's HID interface as a single device.
	opens, closes := 0, 0
	for _, p := range prefixes {
		if p&0xfc == 0xa0 {
			opens++
		}
		if p&0xfc == 0xc0 {
			closes++
		}
	}
	if opens != 1 || closes != 1 || prefixes[len(prefixes)-1] != 0xc0 {
		t.Fatalf("%x", prefixes)
	}
	if !usbproto.DeclaresControl(descriptor) || usbproto.DeclaresControl(stock) {
		t.Fatal("control reports")
	}
}

func TestStagingOnlyAcceptsTheStockDescriptorItWasBuiltFrom(t *testing.T) {
	body := prepareUSB(t)
	if !strings.Contains(body, "sha256sum -c") || !strings.Contains(body, common.SHA256Hex(stock)) ||
		!strings.Contains(body, `> "$run/usb-report.bin"`) {
		t.Fatal(body)
	}
}

func TestParseBenchFlagsForwardsAllBenchOptions(t *testing.T) {
	passed, weights, err := parseBenchFlags([]string{
		"--unpaced", "--spin",
		"--policy", "production",
		"--cpus", "2,3",
		"--priority", "80",
		"--pause-us", "500",
		"--kernel", "compare",
		"--weights", "custom.bin",
	})
	if err != nil {
		t.Fatal(err)
	}
	want := []string{"--unpaced", "--spin", "--policy", "production", "--cpus", "2,3", "--priority", "80", "--pause-us", "500", "--kernel", "compare"}
	if strings.Join(passed, " ") != strings.Join(want, " ") {
		t.Fatalf("passed = %q, want %q", passed, want)
	}
	if weights != "custom.bin" {
		t.Fatalf("weights = %q", weights)
	}
	for _, bad := range [][]string{
		{"--pause-us"},
		{"--pause-us", "-5"},
		{"--pause-us", "abc"},
		{"--kernel"},
		{"--kernel", "invalid"},
		{"--unknown"},
	} {
		if _, _, err := parseBenchFlags(bad); err == nil {
			t.Errorf("accepted invalid bench flags %v", bad)
		}
	}
}
