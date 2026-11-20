package delta

import (
	"bytes"
	"errors"
	"io/fs"
	"math"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/delta/internal/fixture"
)

// Shared with crates/rc2fx-delta/tests/shared_fixtures.rs; regenerate with
// `go generate ./delta`.
const fixtureDir = "../../fixtures/delta"

func caseDirs(t *testing.T, kind string) []string {
	t.Helper()
	entries, err := os.ReadDir(filepath.Join(fixtureDir, kind))
	if err != nil {
		t.Fatal(err)
	}
	var dirs []string
	for _, e := range entries {
		if e.IsDir() {
			dirs = append(dirs, filepath.Join(fixtureDir, kind, e.Name()))
		}
	}
	if len(dirs) == 0 {
		t.Fatalf("no %s fixtures", kind)
	}
	return dirs
}

func readInstalled(t *testing.T, dir string) map[string][]byte {
	t.Helper()
	installed := map[string][]byte{}
	entries, err := os.ReadDir(filepath.Join(dir, "installed"))
	if errors.Is(err, fs.ErrNotExist) {
		return installed
	}
	if err != nil {
		t.Fatal(err)
	}
	for _, e := range entries {
		data, err := os.ReadFile(filepath.Join(dir, "installed", e.Name()))
		if err != nil {
			t.Fatal(err)
		}
		installed[e.Name()] = data
	}
	return installed
}

func lookup(installed map[string][]byte) func(string) ([]byte, error) {
	return func(name string) ([]byte, error) {
		if data, ok := installed[name]; ok {
			return bytes.Clone(data), nil
		}
		return nil, fs.ErrNotExist
	}
}

func mustRead(t *testing.T, path string) []byte {
	t.Helper()
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func TestSharedAcceptFixtures(t *testing.T) {
	for _, dir := range caseDirs(t, "accept") {
		t.Run(filepath.Base(dir), func(t *testing.T) {
			d := mustRead(t, filepath.Join(dir, "delta"))
			var envelope []byte
			var installed map[string][]byte
			if _, err := os.Stat(filepath.Join(dir, "generated")); err == nil {
				envelope, installed = fixture.Large()
			} else {
				envelope = mustRead(t, filepath.Join(dir, "envelope"))
				installed = readInstalled(t, dir)
			}
			var out bytes.Buffer
			written, err := Decode(bytes.NewReader(d), lookup(installed), &out, uint64(len(envelope)))
			if err != nil {
				t.Fatal(err)
			}
			if !bytes.Equal(out.Bytes(), envelope) || written != uint64(len(envelope)) {
				t.Fatal("the envelope was not rebuilt")
			}
			// Also verify the current encoder's output round-trips. Compressed
			// bytes may change after a zstd upgrade (regenerate fixtures then so
			// the Rust tests pick them up).
			hashes := map[string]string{}
			for name, data := range installed {
				hashes[name] = sum(data)
			}
			again, err := Encode(envelope, hashes, installed, 19)
			if err != nil {
				t.Fatal(err)
			}
			out.Reset()
			if _, err := Decode(bytes.NewReader(again), lookup(installed), &out, uint64(len(envelope))); err != nil || !bytes.Equal(out.Bytes(), envelope) {
				t.Fatalf("re-encoded delta: %v", err)
			}
		})
	}
}

func TestSharedRejectFixtures(t *testing.T) {
	for _, dir := range caseDirs(t, "reject") {
		t.Run(filepath.Base(dir), func(t *testing.T) {
			d := mustRead(t, filepath.Join(dir, "delta"))
			limit := uint64(math.MaxUint64)
			if text, err := os.ReadFile(filepath.Join(dir, "max")); err == nil {
				limit, err = strconv.ParseUint(strings.TrimSpace(string(text)), 10, 64)
				if err != nil {
					t.Fatal(err)
				}
			}
			_, err := Decode(bytes.NewReader(d), lookup(readInstalled(t, dir)), &bytes.Buffer{}, limit)
			if err == nil {
				t.Fatal("accepted")
			}
			if want, rerr := os.ReadFile(filepath.Join(dir, "error")); rerr == nil {
				if got, want := err.Error(), strings.TrimSuffix(string(want), "\n"); got != want {
					t.Fatalf("error %q, want %q", got, want)
				}
			}
		})
	}
}
