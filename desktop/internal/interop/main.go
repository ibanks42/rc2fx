// Command interop is a small pure-Go test bridge invoked by Rust integration
// tests (and cross-package Go tests) to verify live Go <-> Rust wire
// compatibility without committed fixtures.
package main

import (
	"bufio"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"math"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/common"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/delta"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/usbproto"
)

func main() {
	if len(os.Args) < 2 {
		fatalf("usage: interop <delta|eq-gate|usb|update-envelope|core|firmware> ...")
	}
	switch os.Args[1] {
	case "delta":
		runDelta()
	case "eq-gate":
		runEQGate()
	case "usb":
		runUSB()
	case "update-envelope":
		runUpdateEnvelope()
	case "core":
		runCore()
	case "firmware":
		runFirmware()
	default:
		fatalf("unknown interop subcommand %q", os.Args[1])
	}
}

// repoRoot is the checkout this file belongs to.
func repoRoot() string {
	_, file, _, ok := runtime.Caller(0)
	if !ok || !filepath.IsAbs(file) {
		fatalf("cannot locate the repository root")
	}
	// Walk up from desktop/internal/interop/main.go.
	return filepath.Dir(filepath.Dir(filepath.Dir(filepath.Dir(file))))
}

func readU32(r io.Reader) uint32 {
	var b [4]byte
	if _, err := io.ReadFull(r, b[:]); err != nil {
		fatalf("read u32: %v", err)
	}
	return binary.BigEndian.Uint32(b[:])
}

func readBytes(r io.Reader) []byte {
	n := readU32(r)
	buf := make([]byte, n)
	if _, err := io.ReadFull(r, buf); err != nil {
		fatalf("read bytes (%d): %v", n, err)
	}
	return buf
}

func writeU32(w io.Writer, n uint32) {
	var b [4]byte
	binary.BigEndian.PutUint32(b[:], n)
	if _, err := w.Write(b[:]); err != nil {
		fatalf("write u32: %v", err)
	}
}

func writeBytes(w io.Writer, data []byte) {
	writeU32(w, uint32(len(data)))
	if _, err := w.Write(data); err != nil {
		fatalf("write bytes: %v", err)
	}
}

func runDelta() {
	r := bufio.NewReader(os.Stdin)
	w := bufio.NewWriter(os.Stdout)
	count := readU32(r)
	writeU32(w, count)
	for range count {
		name := string(readBytes(r))
		envelope := readBytes(r)
		level := int(int32(readU32(r)))
		instCount := readU32(r)
		installed := make(map[string]string, instCount)
		for range instCount {
			k := string(readBytes(r))
			v := string(readBytes(r))
			installed[k] = v
		}
		baseCount := readU32(r)
		bases := make(map[string][]byte, baseCount)
		for range baseCount {
			k := string(readBytes(r))
			v := readBytes(r)
			bases[k] = v
		}
		d, err := delta.Encode(envelope, installed, bases, level)
		if err != nil {
			fatalf("%s encode: %v", name, err)
		}
		writeBytes(w, []byte(name))
		writeBytes(w, d)
	}
	if err := w.Flush(); err != nil {
		fatalf("flush stdout: %v", err)
	}
}

func f64Hex(v float64) string {
	return fmt.Sprintf("%016x", math.Float64bits(v))
}

func runEQGate() {
	type eqSpec struct {
		BandType core.BandType
		Mode     core.BandMode
		Freq     float64
		Gain     float64
		Q        float64
	}
	eqSpecs := []eqSpec{
		{core.BandTypeOff, core.BandModeRlcBt, 1000, 0, 1},
		{core.BandTypeBell, core.BandModeRlcBt, 1000, 6, 1.41},
		{core.BandTypeBell, core.BandModeRlcBt, 250, -9.5, 4},
		{core.BandTypeBell, core.BandModeApoDr, 3200, 12, 0.1},
		{core.BandTypeBell, core.BandModeApoDr, 120, -18, 10},
		{core.BandTypeBell, core.BandModeApoDr, 1000, 0, 1},
		{core.BandTypeLowPass, core.BandModeRlcBt, 8000, 0, 0.707},
		{core.BandTypeLowPass, core.BandModeApoDr, 18000, 0, 0.5},
		{core.BandTypeHighPass, core.BandModeRlcBt, 80, 0, 0.707},
		{core.BandTypeHighPass, core.BandModeApoDr, 120, 0, 2},
		{core.BandTypeNotch, core.BandModeRlcBt, 60, 0, 8},
		{core.BandTypeNotch, core.BandModeApoDr, 4000, 0, 12},
		{core.BandTypeLowShelf, core.BandModeRlcBt, 105, 6.5, 0.7},
		{core.BandTypeLowShelf, core.BandModeApoDr, 46, 15.33, 0.705},
		{core.BandTypeLowShelf, core.BandModeApoDr, 200, -12, 1.2},
		{core.BandTypeHighShelf, core.BandModeRlcBt, 10000, -3.5, 0.7},
		{core.BandTypeHighShelf, core.BandModeApoDr, 7588, 2.49, 1.437},
		{core.BandTypeHighShelf, core.BandModeApoDr, 20000, 6, 0.707},
	}
	var eqOut []map[string]string
	for _, s := range eqSpecs {
		band := core.Band{
			ID: "b1", Enabled: true, BandType: s.BandType, Mode: s.Mode,
			Frequency: s.Freq, Gain: s.Gain, Q: s.Q,
		}
		coeffs, err := core.DesignCoefficients(band)
		if err != nil {
			fatalf("eq %+v: %v", s, err)
		}
		eqOut = append(eqOut, map[string]string{
			"bandType":  string(s.BandType),
			"mode":      string(s.Mode),
			"frequency": f64Hex(s.Freq),
			"gain":      f64Hex(s.Gain),
			"q":         f64Hex(s.Q),
			"designQ":   f64Hex(core.DesignQ(band)),
			"b0":        f64Hex(coeffs.B0),
			"b1":        f64Hex(coeffs.B1),
			"b2":        f64Hex(coeffs.B2),
			"a1":        f64Hex(coeffs.A1),
			"a2":        f64Hex(coeffs.A2),
		})
	}

	strengths := make([]float64, 0, 26)
	for i := 0; i <= 20; i++ {
		strengths = append(strengths, float64(i)/20.0)
	}
	strengths = append(strengths, 1.0/3.0, 2.0/3.0, 0.123456789, -0.5, 1.5)
	var gateOut []map[string]string
	for _, s := range strengths {
		shape := core.GateShapeForStrength(s)
		gateOut = append(gateOut, map[string]string{
			"strength":       f64Hex(s),
			"threshold":      f64Hex(shape.Threshold),
			"closeThreshold": f64Hex(shape.CloseThreshold),
			"reductionDb":    f64Hex(shape.ReductionDB),
			"attackMs":       f64Hex(shape.AttackMs),
			"holdMs":         f64Hex(shape.HoldMs),
			"releaseMs":      f64Hex(shape.ReleaseMs),
		})
	}
	if err := json.NewEncoder(os.Stdout).Encode(map[string]any{
		"eq":   eqOut,
		"gate": gateOut,
	}); err != nil {
		fatalf("encode eq-gate: %v", err)
	}
}

func runUSB() {
	type msgOut struct {
		Name    string `json:"name"`
		Limit   int    `json:"limit"`
		Packed  string `json:"packed"`
		Decoded string `json:"decoded"`
	}
	type frameOut struct {
		Name   string `json:"name"`
		Report uint8  `json:"report"`
		Raw    string `json:"raw"`
		Kind   uint8  `json:"kind"`
		Sess   uint64 `json:"session"`
		Req    uint32 `json:"request"`
		Offset int    `json:"offset"`
		Total  int    `json:"total"`
		Data   string `json:"data"`
	}
	plain := []byte(`{"ok":true}`)
	compressed := []byte(strings.Repeat(`{"target":"mic-1","gain":0.0},`, 120))
	replyLarge := []byte(strings.Repeat(`{"seq":1,"kind":"xrun","message":"ok"},`, 2000))

	var msgs []msgOut
	for _, c := range []struct {
		name  string
		data  []byte
		limit int
	}{
		{"plain", plain, 64},
		{"plain-empty", nil, 64},
		{"compressed", compressed, 8192},
		{"reply-large", replyLarge, usbproto.MaxReply},
	} {
		packed, err := usbproto.Pack(c.data, c.limit)
		if err != nil {
			fatalf("usb pack %s: %v", c.name, err)
		}
		msgs = append(msgs, msgOut{
			Name:    c.name,
			Limit:   c.limit,
			Packed:  hex.EncodeToString(packed),
			Decoded: hex.EncodeToString(c.data),
		})
	}

	var frames []frameOut
	for _, c := range []struct {
		name   string
		report uint8
		frame  usbproto.Frame
	}{
		{"request", usbproto.OutputID, usbproto.Frame{Kind: usbproto.KindRequest, Session: 0x0123456789abcdef, Request: 12, Offset: 224, Total: 1000, Data: []byte(`{"method":`)}},
		{"reply-full", usbproto.InputID, usbproto.Frame{Kind: usbproto.KindReply, Session: 1, Request: 1, Offset: 0, Total: usbproto.Payload, Data: []byte(strings.Repeat("*", usbproto.Payload))}},
		{"hello", usbproto.OutputID, usbproto.Frame{Kind: usbproto.KindHello, Session: 1, Request: 3, Data: []byte(usbproto.Hello4)}},
		{"max-bounds", usbproto.InputID, usbproto.Frame{Kind: usbproto.KindReply, Session: math.MaxUint64, Request: math.MaxUint32, Offset: math.MaxUint32, Total: math.MaxUint32}},
	} {
		raw := c.frame.Encode(c.report)
		frames = append(frames, frameOut{
			Name:   c.name,
			Report: c.report,
			Raw:    hex.EncodeToString(raw[:]),
			Kind:   uint8(c.frame.Kind),
			Sess:   c.frame.Session,
			Req:    c.frame.Request,
			Offset: c.frame.Offset,
			Total:  c.frame.Total,
			Data:   hex.EncodeToString(c.frame.Data),
		})
	}
	if err := json.NewEncoder(os.Stdout).Encode(map[string]any{
		"messages": msgs,
		"frames":   frames,
	}); err != nil {
		fatalf("encode usb: %v", err)
	}
}

func runUpdateEnvelope() {
	dev := len(os.Args) > 2 && os.Args[2] == "--dev"
	base, err := os.MkdirTemp("", "rc2fx-interop-env-*")
	if err != nil {
		fatalf("mkdtemp: %v", err)
	}
	defer os.RemoveAll(base)

	root := repoRoot()
	runtimeDir := filepath.Join(base, "runtime")
	scriptsDir := filepath.Join(base, "mixer", "scripts")
	configDir := filepath.Join(base, "config")
	for _, d := range []string{runtimeDir, scriptsDir, configDir} {
		if err := os.MkdirAll(d, 0o755); err != nil {
			fatalf("mkdir: %v", err)
		}
	}
	elf := make([]byte, 96)
	copy(elf, []byte{127, 'E', 'L', 'F', 2, 1})
	elf[18] = 0xb7
	for _, name := range embed.Binaries {
		if err := os.WriteFile(filepath.Join(runtimeDir, name), elf, 0o644); err != nil {
			fatalf("write %s: %v", name, err)
		}
	}
	catalog, err := os.ReadFile(filepath.Join(root, "config", "firmware-profiles.json"))
	if err != nil {
		fatalf("read catalog: %v", err)
	}
	releaseJSON := []byte(`{"schemaVersion":1,"version":"1.2.3","protocolVersion":1}`)
	for name, data := range map[string][]byte{
		"firmware-profiles.json": catalog,
		"mixer-release.json":     releaseJSON,
	} {
		if err := os.WriteFile(filepath.Join(runtimeDir, name), data, 0o644); err != nil {
			fatalf("write %s: %v", name, err)
		}
		if err := os.WriteFile(filepath.Join(configDir, name), data, 0o644); err != nil {
			fatalf("write config %s: %v", name, err)
		}
	}
	var sums strings.Builder
	for _, name := range embed.Pinned {
		data, err := os.ReadFile(filepath.Join(runtimeDir, name))
		if err != nil {
			fatalf("read %s: %v", name, err)
		}
		fmt.Fprintf(&sums, "%s  %s\n", common.SHA256Hex(data), name)
	}
	if err := os.WriteFile(filepath.Join(runtimeDir, "SHA256SUMS"), []byte(sums.String()), 0o644); err != nil {
		fatalf("write SHA256SUMS: %v", err)
	}
	for _, name := range embed.Scripts {
		data, err := os.ReadFile(filepath.Join(root, "mixer", "scripts", name))
		if err != nil {
			fatalf("read script %s: %v", name, err)
		}
		if err := os.WriteFile(filepath.Join(scriptsDir, name), data, 0o644); err != nil {
			fatalf("write script %s: %v", name, err)
		}
	}
	env, err := embed.Expected(base, !dev)
	if err != nil {
		fatalf("embed.Expected: %v", err)
	}
	if _, err := os.Stdout.Write(env.AsBytes()); err != nil {
		fatalf("write envelope: %v", err)
	}
}

func decodeCoreCase(kind string, data []byte) (any, error) {
	if kind == "mixerRelease" {
		return core.ParseMixerRelease(data)
	}
	if !utf8.Valid(data) {
		return nil, core.CoreError("not UTF-8")
	}
	text := string(data)
	switch kind {
	case "config":
		return core.ConfigFromJSON(text)
	case "deviceState":
		return core.DeviceStateFromJSON(text)
	case "configUpdate":
		return core.FromJSON[core.ConfigUpdate](text)
	case "presetSummaries":
		return core.FromJSON[[]core.PresetSummary](text)
	case "deviceVersions":
		v, err := core.FromJSON[core.DeviceVersions](text)
		if err != nil {
			return nil, err
		}
		return v, v.Validate()
	case "sshStatus":
		v, err := core.FromJSON[core.SSHStatus](text)
		if err != nil {
			return nil, err
		}
		return v, v.Validate()
	case "meter":
		return core.FromJSON[core.Meter](text)
	case "diagnosticsEvents":
		return core.FromJSON[core.DiagnosticsEvents](text)
	case "stockInventory":
		v, err := core.FromJSON[core.StockInventory](text)
		if err != nil {
			return nil, err
		}
		return v, v.Validate()
	case "json":
		return core.FromJSON[core.Value](text)
	default:
		return nil, fmt.Errorf("unknown core kind %q", kind)
	}
}

func runCore() {
	type inCase struct {
		Name    string `json:"name"`
		Kind    string `json:"kind"`
		DataHex string `json:"dataHex"`
	}
	type outCase struct {
		Name    string `json:"name"`
		OK      bool   `json:"ok"`
		Compact string `json:"compact,omitempty"`
		Pretty  string `json:"pretty,omitempty"`
		Error   string `json:"error,omitempty"`
	}
	raw, err := io.ReadAll(os.Stdin)
	if err != nil {
		fatalf("read core stdin: %v", err)
	}
	var cases []inCase
	if err := json.Unmarshal(raw, &cases); err != nil {
		fatalf("decode core cases: %v", err)
	}
	out := make([]outCase, 0, len(cases))
	for _, c := range cases {
		data, err := hex.DecodeString(c.DataHex)
		if err != nil {
			fatalf("%s hex: %v", c.Name, err)
		}
		v, err := decodeCoreCase(c.Kind, data)
		if err != nil {
			out = append(out, outCase{Name: c.Name, OK: false, Error: err.Error()})
			continue
		}
		compact, err := core.Encode(v)
		if err != nil {
			fatalf("%s encode: %v", c.Name, err)
		}
		pretty, err := core.EncodePretty(v)
		if err != nil {
			fatalf("%s encode pretty: %v", c.Name, err)
		}
		out = append(out, outCase{
			Name:    c.Name,
			OK:      true,
			Compact: string(compact),
			Pretty:  string(pretty),
		})
	}
	if err := json.NewEncoder(os.Stdout).Encode(out); err != nil {
		fatalf("encode core response: %v", err)
	}
}

func runFirmware() {
	type boardIn struct {
		Hostname string `json:"hostname"`
		Extlinux string `json:"extlinux"`
	}
	type catalogIn struct {
		Name string `json:"name"`
		Text string `json:"text"`
	}
	type selectIn struct {
		Name        string `json:"name"`
		Op          string `json:"op"`
		Catalog     string `json:"catalog"`
		ImageHex    string `json:"imageHex"`
		Model       string `json:"model"`
		LauncherHex string `json:"launcherHex"`
	}
	var req struct {
		Boards   []boardIn   `json:"boards"`
		Parse    []string    `json:"parse"`
		Catalogs []catalogIn `json:"catalogs"`
		Select   []selectIn  `json:"select"`
	}
	raw, err := io.ReadAll(os.Stdin)
	if err != nil {
		fatalf("read firmware stdin: %v", err)
	}
	if err := json.Unmarshal(raw, &req); err != nil {
		fatalf("decode firmware req: %v", err)
	}

	type modelOut struct {
		ID             string `json:"id"`
		Hostname       string `json:"hostname"`
		BoardDTB       string `json:"boardDtb"`
		RouteLayout    string `json:"routeLayout"`
		SourceEnum     string `json:"sourceEnum"`
		RuntimeEnabled bool   `json:"runtimeEnabled"`
	}
	var models []modelOut
	for _, m := range firmware.ModelAll {
		models = append(models, modelOut{
			ID:             m.ID(),
			Hostname:       m.Hostname(),
			BoardDTB:       m.BoardDTB(),
			RouteLayout:    m.RouteLayout(),
			SourceEnum:     m.SourceEnum(),
			RuntimeEnabled: m.RuntimeEnabled(),
		})
	}

	type resOut struct {
		Name  string `json:"name,omitempty"`
		OK    bool   `json:"ok"`
		Value string `json:"value,omitempty"`
		Error string `json:"error,omitempty"`
	}
	var boards []resOut
	for _, b := range req.Boards {
		m, err := firmware.ModelFromBoard(b.Hostname, b.Extlinux)
		if err != nil {
			boards = append(boards, resOut{OK: false, Error: err.Error()})
		} else {
			boards = append(boards, resOut{OK: true, Value: m.ID()})
		}
	}
	var parsed []resOut
	for _, id := range req.Parse {
		m, err := firmware.ParseModel(id)
		if err != nil {
			parsed = append(parsed, resOut{OK: false, Error: err.Error()})
		} else {
			parsed = append(parsed, resOut{OK: true, Value: m.ID()})
		}
	}
	var catalogs []resOut
	for _, c := range req.Catalogs {
		_, err := firmware.ParseCatalog(c.Text)
		if err != nil {
			catalogs = append(catalogs, resOut{Name: c.Name, OK: false, Error: err.Error()})
		} else {
			catalogs = append(catalogs, resOut{Name: c.Name, OK: true})
		}
	}
	var selected []resOut
	for _, s := range req.Select {
		cat, err := firmware.ParseCatalog(s.Catalog)
		if err != nil {
			fatalf("select %s catalog: %v", s.Name, err)
		}
		img, err := hex.DecodeString(s.ImageHex)
		if err != nil {
			fatalf("select %s image: %v", s.Name, err)
		}
		launcher, err := hex.DecodeString(s.LauncherHex)
		if err != nil {
			fatalf("select %s launcher: %v", s.Name, err)
		}
		var id string
		switch s.Op {
		case "verifyImage":
			err = cat.Profiles[0].VerifyImage(img)
			if err == nil {
				id = cat.Profiles[0].ID
			}
		case "inspectForModel":
			var m firmware.Model
			m, err = firmware.ParseModel(s.Model)
			if err == nil {
				var p *firmware.Profile
				p, err = cat.InspectForModel(m, img, launcher)
				if err == nil {
					id = p.ID
				}
			}
		case "verifyLauncher":
			err = cat.Profiles[0].VerifyLauncher(launcher)
			if err == nil {
				id = cat.Profiles[0].ID
			}
		default:
			fatalf("unknown select op %q", s.Op)
		}
		if err != nil {
			selected = append(selected, resOut{Name: s.Name, OK: false, Error: err.Error()})
		} else {
			selected = append(selected, resOut{Name: s.Name, OK: true, Value: id})
		}
	}
	if err := json.NewEncoder(os.Stdout).Encode(map[string]any{
		"models":   models,
		"boards":   boards,
		"parse":    parsed,
		"catalogs": catalogs,
		"select":   selected,
	}); err != nil {
		fatalf("encode firmware response: %v", err)
	}
}

func fatalf(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "interop: "+format+"\n", args...)
	os.Exit(1)
}
