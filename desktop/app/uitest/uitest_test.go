package main

// Builds the uitest program and runs each mode offscreen with the diag
// library: `go test ./app/uitest` from desktop/, with RC2FX_GPUIKIT_DIAG set
// to the unpacked diag download's lib folder (libgpuikit.so). Skipped
// without it. RC2FX_UITEST_SHOTS keeps the screenshots (default: a test
// temp folder); they are checked by eye once and kept per library hash.

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func TestOffscreenModes(t *testing.T) {
	diag := os.Getenv("RC2FX_GPUIKIT_DIAG")
	if diag == "" {
		t.Skip("RC2FX_GPUIKIT_DIAG is not set (the diag library's lib folder)")
	}
	bin := filepath.Join(t.TempDir(), "uitest")
	build := exec.Command("go", "build", "-tags", "mockmixer", "-o", bin, ".")
	build.Env = append(os.Environ(), "CGO_LDFLAGS=-L"+diag+" -Wl,-rpath,"+diag)
	if out, err := build.CombinedOutput(); err != nil {
		t.Fatalf("build: %v\n%s", err, out)
	}
	shots := os.Getenv("RC2FX_UITEST_SHOTS")
	if shots == "" {
		shots = t.TempDir()
	}
	for _, mode := range []string{"licenses-markup", "licenses-long", "licenses-lazy"} {
		t.Run(mode, func(t *testing.T) {
			run := exec.Command(bin)
			env := []string{"RC2FX_UITEST=" + mode, "RC2FX_UITEST_SHOTS=" + shots, "LD_LIBRARY_PATH=" + diag}
			for _, kv := range os.Environ() {
				if !strings.HasPrefix(kv, "DISPLAY=") && !strings.HasPrefix(kv, "WAYLAND_DISPLAY=") {
					env = append(env, kv)
				}
			}
			run.Env = env
			out, err := run.CombinedOutput()
			if err != nil || !strings.Contains(string(out), "PASS ") {
				t.Fatalf("%v\n%s", err, out)
			}
			t.Logf("%s", out)
		})
	}
}
