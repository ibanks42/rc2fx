package deps

import (
	"archive/tar"
	"archive/zip"
	"bytes"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/klauspost/compress/zstd"
)

type entry struct {
	name, body, link string
	mode             int64
	dir              bool
}

func writeTarZst(t *testing.T, path string, entries []entry) {
	t.Helper()
	var buf bytes.Buffer
	encoder, err := zstd.NewWriter(&buf)
	if err != nil {
		t.Fatal(err)
	}
	writer := tar.NewWriter(encoder)
	for _, e := range entries {
		header := &tar.Header{Name: e.name, Mode: e.mode}
		switch {
		case e.dir:
			header.Typeflag = tar.TypeDir
		case e.link != "":
			header.Typeflag, header.Linkname = tar.TypeSymlink, e.link
		default:
			header.Typeflag, header.Size = tar.TypeReg, int64(len(e.body))
		}
		if err := writer.WriteHeader(header); err != nil {
			t.Fatal(err)
		}
		if header.Typeflag == tar.TypeReg {
			if _, err := writer.Write([]byte(e.body)); err != nil {
				t.Fatal(err)
			}
		}
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	if err := encoder.Close(); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, buf.Bytes(), 0o644); err != nil {
		t.Fatal(err)
	}
}

func writeZip(t *testing.T, path string, entries []entry) {
	t.Helper()
	var buf bytes.Buffer
	writer := zip.NewWriter(&buf)
	for _, e := range entries {
		w, err := writer.Create(e.name)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := w.Write([]byte(e.body)); err != nil {
			t.Fatal(err)
		}
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, buf.Bytes(), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestExtractTarZstKeepsFilesModesAndLinks(t *testing.T) {
	dir := t.TempDir()
	archive := filepath.Join(dir, "a.tar.zst")
	writeTarZst(t, archive, []entry{
		{name: "pkg/", dir: true, mode: 0o755},
		{name: "pkg/lib/libx.a", body: "static", mode: 0o644},
		{name: "pkg/build.sh", body: "#!/bin/sh\n", mode: 0o755},
		{name: "pkg/lib/libx.so", link: "libx.a", mode: 0o777},
	})
	out := filepath.Join(dir, "x")
	if err := extractTarZst(archive, out); err != nil {
		t.Fatal(err)
	}
	if data, err := os.ReadFile(filepath.Join(out, "pkg/lib/libx.a")); err != nil || string(data) != "static" {
		t.Fatalf("libx.a = %q, %v", data, err)
	}
	if runtime.GOOS != "windows" {
		info, err := os.Stat(filepath.Join(out, "pkg/build.sh"))
		if err != nil || info.Mode().Perm()&0o100 == 0 {
			t.Fatalf("build.sh lost its execute bit: %v %v", info, err)
		}
		if target, err := os.Readlink(filepath.Join(out, "pkg/lib/libx.so")); err != nil || target != "libx.a" {
			t.Fatalf("libx.so -> %q, %v", target, err)
		}
	}
}

func TestExtractRefusesEntriesOutsideTheDestination(t *testing.T) {
	for _, name := range []string{"../escape", "a/../../escape", "/abs"} {
		dir := t.TempDir()
		archive := filepath.Join(dir, "a.tar.zst")
		writeTarZst(t, archive, []entry{{name: name, body: "x", mode: 0o644}})
		if err := extractTarZst(archive, filepath.Join(dir, "x")); err == nil {
			t.Errorf("tar entry %q was unpacked", name)
		}
		zipped := filepath.Join(dir, "a.zip")
		writeZip(t, zipped, []entry{{name: name, body: "x"}})
		if err := extractZip(zipped, filepath.Join(dir, "x")); err == nil {
			t.Errorf("zip entry %q was unpacked", name)
		}
	}
	dir := t.TempDir()
	archive := filepath.Join(dir, "a.tar.zst")
	writeTarZst(t, archive, []entry{{name: "pkg/link", link: "../../outside", mode: 0o777}})
	if err := extractTarZst(archive, filepath.Join(dir, "x")); err == nil {
		t.Error("a link leaving the archive was unpacked")
	}
}

func TestExtractZip(t *testing.T) {
	dir := t.TempDir()
	archive := filepath.Join(dir, "a.zip")
	writeZip(t, archive, []entry{{name: "pkg/lib/libx.a", body: "static"}})
	out := filepath.Join(dir, "x")
	if err := extractZip(archive, out); err != nil {
		t.Fatal(err)
	}
	if data, err := os.ReadFile(filepath.Join(out, "pkg/lib/libx.a")); err != nil || string(data) != "static" {
		t.Fatalf("libx.a = %q, %v", data, err)
	}
}

func TestGouiKitPlatforms(t *testing.T) {
	for target, want := range map[string]string{
		"linux-amd64": "linux-x86_64 tar.zst", "darwin-universal": "macos-universal tar.zst",
		"darwin-arm64": "macos-universal tar.zst", "windows-amd64": "windows-x86_64 zip",
	} {
		platform, ext, err := gouiKitPlatform(target)
		if err != nil || platform+" "+ext != want {
			t.Errorf("%s: %s %s, %v", target, platform, ext, err)
		}
	}
	if _, _, err := gouiKitPlatform("linux-arm64"); err == nil || !strings.Contains(err.Error(), "linux-arm64") {
		t.Errorf("linux-arm64: %v", err)
	}
}

func TestValidSHA256(t *testing.T) {
	good := strings.Repeat("ab", 32)
	if !validSHA256(good) || validSHA256("") || validSHA256(strings.ToUpper(good)) || validSHA256(good[:62]) {
		t.Error("validSHA256 accepts or refuses the wrong values")
	}
}
