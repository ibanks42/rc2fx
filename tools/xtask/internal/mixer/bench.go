package mixer

// Benchmarks DeepFilterNet on the mixer over a recording (crates/rc2fx-bench)
// and fetches the denoised audio, optionally comparing a candidate weights file.

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"math"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/common"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/num"
	"github.com/ibanks42/rc2fx/desktop/usbcontrol"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/elfcheck"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/setup"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

// models is the fixed suppression engine; custom is a DeepFilterNet weights
// file given with --weights.
var models = []string{"deepfilternet3-h128"}

const (
	custom = "deepfilternet3-custom"
	// maxRecording is the longest recording accepted; outputs live in the
	// mixer's RAM.
	maxRecording = 60
)

// BenchBuild is `bench-build`: cross-build the suppression benchmark into
// target/bench.
func BenchBuild() error {
	root := task.Root()
	if err := setup.CheckZig(); err != nil {
		return err
	}
	targetDir := filepath.Join(root, "target", "bench-build")
	command := exec.Command("cargo", "zigbuild", "--locked", "--release", "--target", setup.ZigTarget,
		"--manifest-path", "crates/rc2fx-bench/Cargo.toml")
	command.Dir = root
	command.Env = append(os.Environ(), "CARGO_TARGET_DIR="+targetDir, "RUSTFLAGS="+setup.MixerRustflags())
	if err := task.Run(command); err != nil {
		return err
	}
	out := filepath.Join(root, "target", "bench")
	if err := os.MkdirAll(out, 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	binary := filepath.Join(out, "rc2fx-bench")
	data, err := os.ReadFile(filepath.Join(targetDir, setup.Target, "release", "rc2fx-bench"))
	if err != nil {
		return task.Error(task.IOError(err))
	}
	if err := os.WriteFile(binary, data, 0o755); err != nil {
		return task.Error(task.IOError(err))
	}
	return elfcheck.Check(binary, false)
}

// Bench is `bench WAV [FLAGS]`.
func Bench(input string, flags []string) error {
	return benchIn(filepath.Join(task.Root(), "target", "bench"), Load, input, flags)
}

// parseBenchFlags validates flags forwarded to rc2fx-bench and extracts any
// --weights file path (which benchIn uploads beside the binary).
func parseBenchFlags(flags []string) (passed []string, weightsPath string, err error) {
	for i := 0; i < len(flags); i++ {
		flag := flags[i]
		switch flag {
		case "--unpaced", "--spin":
			passed = append(passed, flag)
		case "--policy", "--cpus", "--priority":
			if i+1 >= len(flags) {
				return nil, "", task.Errorf("%s needs a value", flag)
			}
			i++
			value := flags[i]
			for _, c := range value {
				if !(c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' || c == ',' || c == '-') {
					return nil, "", task.Errorf("bad %s value %s", flag, value)
				}
			}
			passed = append(passed, flag, value)
		case "--pause-us":
			if i+1 >= len(flags) {
				return nil, "", task.Error("--pause-us needs microseconds")
			}
			i++
			value := flags[i]
			if _, ok := common.ParseUint(value, 64); !ok {
				return nil, "", task.Errorf("bad --pause-us value %s", value)
			}
			passed = append(passed, flag, value)
		case "--kernel":
			if i+1 >= len(flags) {
				return nil, "", task.Error("--kernel needs tiled, rows or compare")
			}
			i++
			value := flags[i]
			switch value {
			case "tiled", "rows", "compare":
			default:
				return nil, "", task.Errorf("bad --kernel value %s (expected tiled, rows or compare)", value)
			}
			passed = append(passed, flag, value)
		case "--weights":
			if i+1 >= len(flags) {
				return nil, "", task.Error("--weights needs a file")
			}
			i++
			weightsPath = flags[i]
		default:
			return nil, "", task.Errorf("unknown flag %s", flag)
		}
	}
	return passed, weightsPath, nil
}

// benchIn runs the benchmark using benchDir (normally target/bench) and a
// mixer constructor (overridden in tests).
func benchIn(bench string, mixer func() (*Mixer, error), input string, flags []string) error {
	// Flags pass through to rc2fx-bench except --weights, whose file is
	// uploaded and run alongside the embedded weights.
	passed, weightsPath, err := parseBenchFlags(flags)
	if err != nil {
		return err
	}
	var weights []byte
	haveWeights := false
	if weightsPath != "" {
		data, err := os.ReadFile(weightsPath)
		if err != nil {
			return task.Errorf("%s: %s", weightsPath, task.IOError(err))
		}
		weights, haveWeights = data, true
	}
	recording, err := os.ReadFile(input)
	if err != nil {
		return task.Errorf("%s: %s", input, task.IOError(err))
	}
	rate, seconds, err := wavHeader(recording)
	if err != nil {
		return err
	}
	if rate != 48_000 {
		return task.Errorf("the recording is %d Hz; use 48 kHz", rate)
	}
	if seconds > maxRecording {
		return task.Errorf("the recording is %d s; keep it under %d s", seconds, maxRecording)
	}
	program, err := os.ReadFile(filepath.Join(bench, "rc2fx-bench"))
	if err != nil {
		return task.Error("target/bench/rc2fx-bench is missing; run `just bench-build`")
	}
	var archive bytes.Buffer
	writer := tar.NewWriter(&archive)
	files := []struct {
		name string
		data []byte
		mode int64
	}{
		{"input.wav", recording, 0o600},
		{"rc2fx-bench", program, 0o700},
	}
	if haveWeights {
		files = append(files, struct {
			name string
			data []byte
			mode int64
		}{"weights.bin", weights, 0o600})
	}
	for _, f := range files {
		if err := ustar(writer, f.name, f.data, f.mode); err != nil {
			return task.Error(err.Error())
		}
	}
	if err := writer.Close(); err != nil {
		return task.Error(err.Error())
	}

	m, err := mixer()
	if err != nil {
		return err
	}
	if err := m.Prepare("bench"); err != nil {
		return err
	}
	fmt.Println("Uploading to the mixer")
	if err := m.ssh("set -e; rm -rf /run/rc2fx-bench; mkdir -m 700 /run/rc2fx-bench; tar -xf - -C /run/rc2fx-bench", archive.Bytes()); err != nil {
		return err
	}
	passedFlags := strings.Join(passed, " ")
	run := append([]string{}, models...)
	if haveWeights {
		run = append(run, custom)
	}
	var overruns []*overrun
	var outcome error
	for index, model := range run {
		fmt.Printf("Running %s...\n", model)
		extra := ""
		if model == custom {
			extra = " --weights weights.bin"
		}
		script := fmt.Sprintf("cd /run/rc2fx-bench && ./rc2fx-bench %s input.wav %02d-%s.wav %s%s "+
			"> %s.json 2> %s.log || echo \"  %s failed: $(tail -n 3 %s.log)\"",
			model, index+1, model, passedFlags, extra, model, model, model, model)
		if err := m.ssh(script, nil); err != nil {
			outcome = err
			break
		}
		// The controller keeps only the last ~20 s, so read it right away.
		window, _ := m.capture("cat /run/rc2fx-bench/" + model + ".json")
		overruns = append(overruns, overrunsDuring(window))
	}
	// Bulk uncompressed SSH downloads disturb the live audio graph. Device
	// comparisons found compressed downloads clean, at the cost of transfer
	// time. Uploads did not benefit, so they stay uncompressed.
	results, resultsErr := m.capture("cd /run/rc2fx-bench && tar -czf - *.wav *.json *.log 2>/dev/null; true")
	_ = m.ssh("rm -rf /run/rc2fx-bench", nil)
	if outcome != nil {
		return outcome
	}

	out := filepath.Join(bench, fileStem(input))
	_ = os.RemoveAll(out)
	if err := os.MkdirAll(out, 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	if resultsErr != nil {
		return resultsErr
	}
	if err := unpackResults(results, out); err != nil {
		return err
	}
	if err := os.Rename(filepath.Join(out, "input.wav"), filepath.Join(out, "00-input.wav")); err != nil {
		return task.Error(task.IOError(err))
	}
	return report(out, run, overruns)
}

// fileStem is path's file name without its last extension; a name that
// starts with its only dot keeps it.
func fileStem(path string) string {
	name := filepath.Base(path)
	if name == "." || name == string(filepath.Separator) {
		return ""
	}
	if name == ".." {
		return ".."
	}
	at := strings.LastIndexByte(name, '.')
	if at <= 0 {
		return name
	}
	return name[:at]
}

// unpackResults reads through the gzip footer before touching the output
// directory. A tar reader can stop at its end marker without checking a
// truncated gzip tail.
func unpackResults(data []byte, out string) error {
	reader, err := gzip.NewReader(bytes.NewReader(data))
	if err != nil {
		return task.Errorf("decoding benchmark results: %s", err)
	}
	reader.Multistream(false)
	archive, err := io.ReadAll(reader)
	if err != nil {
		return task.Errorf("decoding benchmark results: %s", err)
	}
	entries := tar.NewReader(bytes.NewReader(archive))
	for {
		header, err := entries.Next()
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			return task.Error(err.Error())
		}
		name := filepath.FromSlash(header.Name)
		if filepath.IsAbs(name) || !filepath.IsLocal(name) {
			// The tar crate skips entries that would land outside the
			// destination.
			continue
		}
		path := filepath.Join(out, name)
		switch header.Typeflag {
		case tar.TypeDir:
			if err := os.MkdirAll(path, 0o777); err != nil {
				return task.Error(task.IOError(err))
			}
		case tar.TypeReg:
			if err := os.MkdirAll(filepath.Dir(path), 0o777); err != nil {
				return task.Error(task.IOError(err))
			}
			data, err := io.ReadAll(entries)
			if err != nil {
				return task.Error(err.Error())
			}
			mode := os.FileMode(header.Mode) & 0o777
			if mode == 0 {
				mode = 0o644
			}
			if err := os.WriteFile(path, data, mode); err != nil {
				return task.Error(task.IOError(err))
			}
		}
	}
}

type overrun struct {
	count   uint64
	seconds float64
}

// overrunsDuring is RC2FX audio-callback overruns while the benchmark's
// timed loop ran, and the seconds observed, from the controller's
// per-second history (mixer clock), read over USB 1. Nil when RC2FX is not
// running or not reachable.
func overrunsDuring(window []byte) *overrun {
	reportValue, err := core.ParseJSONBytes(window, 64<<20, 4_000_000)
	if err != nil {
		return nil
	}
	start, ok1 := getU64(reportValue, "startUnixMs")
	end, ok2 := getU64(reportValue, "endUnixMs")
	if !ok1 || !ok2 {
		return nil
	}
	client, err := usbcontrol.Open()
	if err != nil {
		return nil
	}
	defer client.Close()
	reply, err := client.Request(&usbcontrol.Request{Method: "GET", Path: "api/v1/diagnostics"})
	if err != nil || len(reply.Body) == 0 {
		return nil
	}
	value, err := core.ParseJSONBytes(reply.Body, maxReplyBytes, maxReplyNodes)
	if err != nil {
		return nil
	}
	samplesValue, _ := value.Get("samples")
	samples, ok := samplesValue.AsArray()
	if !ok {
		return nil
	}
	type point struct{ at, overruns uint64 }
	var points []point
	for _, sample := range samples {
		at, ok := path(sample, "observation", "at", "unixTimeMs").AsU64()
		if !ok {
			continue
		}
		diagnostics, _ := path(sample, "telemetry", "nativeDiagnostics").Index(0)
		count, ok := path(diagnostics, "timing", "callbackWall", "overruns").AsU64()
		if !ok {
			continue
		}
		if at >= start && at <= end {
			points = append(points, point{at, count})
		}
	}
	if len(points) == 0 {
		return nil
	}
	first, last := points[0], points[len(points)-1]
	if last.at <= first.at {
		return nil
	}
	count := uint64(0)
	if last.overruns > first.overruns {
		count = last.overruns - first.overruns
	}
	return &overrun{count, float64(last.at-first.at) / 1e3}
}

// path is value[key][key]...: Null where a key is missing.
func path(value core.Value, keys ...string) core.Value {
	for _, key := range keys {
		next, ok := value.Get(key)
		if !ok {
			return core.NullValue()
		}
		value = next
	}
	return value
}

func getU64(value core.Value, key string) (uint64, bool) {
	return path(value, key).AsU64()
}

// wavHeader is the sample rate and whole seconds from a WAV header.
func wavHeader(data []byte) (uint32, uint64, error) {
	if len(data) < 12 || string(data[0:4]) != "RIFF" || string(data[8:12]) != "WAVE" {
		return 0, 0, task.Error("the recording must be a WAV file")
	}
	var rate, byteRate uint32
	var size64 uint64
	at := 12
	for at+8 <= len(data) {
		id := string(data[at : at+4])
		size := int(binary.LittleEndian.Uint32(data[at+4:]))
		end := min(len(data), at+8+size)
		body := data[at+8 : end]
		if id == "fmt " && len(body) >= 12 {
			rate = binary.LittleEndian.Uint32(body[4:])
			byteRate = binary.LittleEndian.Uint32(body[8:])
		} else if id == "data" {
			size64 = uint64(size)
		}
		at += 8 + size + (size & 1)
	}
	if rate == 0 || byteRate == 0 {
		return 0, 0, task.Error("the WAV file has no format chunk")
	}
	return rate, size64 / uint64(byteRate), nil
}

func report(out string, run []string, overruns []*overrun) error {
	fmt.Printf("\n%-26s %8s %9s %9s %9s %9s %6s %5s %8s %6s %9s\n",
		"model", "frame", "cpu mean", "cpu max", "wall p99", "wall max", "load", "late", "startup", "RSS", "mixer ovr")
	for index, model := range run {
		text, _ := os.ReadFile(filepath.Join(out, model+".json"))
		value, err := core.ParseJSONLimited(strings.TrimSpace(string(text)), 64<<20, 4_000_000)
		if err != nil {
			fmt.Printf("%-26s failed; see %s.log\n", model, model)
			continue
		}
		number := func(keys ...string) float64 {
			n, ok := path(value, keys...).AsF64()
			if !ok {
				return math.NaN()
			}
			return n
		}
		over := "-"
		if index < len(overruns) && overruns[index] != nil {
			over = fmt.Sprintf("%d in %.0fs", overruns[index].count, overruns[index].seconds)
		}
		name := model
		if n, ok := path(value, "name").AsString(); ok {
			name = n
		}
		fmt.Printf("%-26s %6.2fms %7.2fms %7.2fms %7.2fms %7.2fms %5.0f%% %5s %6.0fms %4.0fMB %9s\n",
			name,
			number("frameMs"),
			number("cpu", "meanMs"),
			number("cpu", "maxMs"),
			number("wall", "p99Ms"),
			number("wall", "maxMs"),
			number("realtimeFactor")*100,
			num.FormatDecimal(number("lateFrames")),
			number("startupMs"),
			number("peakRssKiB")/1024,
			over,
		)
	}
	fmt.Printf("\nLoad is CPU time per frame as a share of the frame's duration; above 100%% cannot run live.\n"+
		"Mixer ovr counts RC2FX audio-callback overruns while each model ran (- when RC2FX is not running).\n"+
		"Denoised recordings and full results: %s\n", out)
	return nil
}
