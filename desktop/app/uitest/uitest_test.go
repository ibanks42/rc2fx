package main

// Runs offscreen UI tests via `go test ./app/uitest` from desktop/.
// Requires RC2FX_GOUIKIT_DIAG pointing to a diagnostic build of libgouikit
// (built via `make lib` in the bindings repo); skipped otherwise.
// Set RC2FX_UITEST_SHOTS to keep rendered screenshots instead of using a temp dir.

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

func TestOffscreenModes(t *testing.T) {
	diag := os.Getenv("RC2FX_GOUIKIT_DIAG")
	if diag == "" {
		t.Skip("RC2FX_GOUIKIT_DIAG is not set (the diag library's lib folder)")
	}
	bin := filepath.Join(t.TempDir(), "uitest")
	build := exec.Command("go", "build", "-tags", "mockmixer,gkstatic", "-o", bin, ".")
	build.Env = append(os.Environ(), "CGO_LDFLAGS=-L"+diag)
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
