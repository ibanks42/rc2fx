package mixer

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func compressedResults(t *testing.T) []byte {
	t.Helper()
	var out bytes.Buffer
	zipper := gzip.NewWriter(&out)
	archive := tar.NewWriter(zipper)
	for _, f := range []struct{ name, data string }{
		{"input.wav", "RIFF\x00\xff\x80binary audio"},
		{"model.json", `{"frames":200}`},
		{"model.log", "completed\n"},
	} {
		header := &tar.Header{Typeflag: tar.TypeReg, Name: f.name, Mode: 0o600, Size: int64(len(f.data)),
			ModTime: time.Unix(0, 0), Format: tar.FormatUSTAR}
		if err := archive.WriteHeader(header); err != nil {
			t.Fatal(err)
		}
		if _, err := archive.Write([]byte(f.data)); err != nil {
			t.Fatal(err)
		}
	}
	if err := archive.Close(); err != nil {
		t.Fatal(err)
	}
	if err := zipper.Close(); err != nil {
		t.Fatal(err)
	}
	return out.Bytes()
}

func TestCompressedResultsPreserveAudioAndReports(t *testing.T) {
	out := t.TempDir()
	if err := unpackResults(compressedResults(t), out); err != nil {
		t.Fatal(err)
	}
	for name, want := range map[string]string{
		"input.wav":  "RIFF\x00\xff\x80binary audio",
		"model.json": `{"frames":200}`,
		"model.log":  "completed\n",
	} {
		got, err := os.ReadFile(filepath.Join(out, name))
		if err != nil || string(got) != want {
			t.Errorf("%s: %q %v", name, got, err)
		}
	}
}

func TestIncompleteOrCorruptGzipIsRejectedBeforeUnpacking(t *testing.T) {
	good := compressedResults(t)
	badCRC := append([]byte{}, good...)
	badCRC[len(badCRC)-8] ^= 1
	for _, data := range [][]byte{good[:len(good)-8], badCRC} {
		out := t.TempDir()
		if err := unpackResults(data, out); err == nil {
			t.Fatal("accepted")
		}
		if entries, _ := os.ReadDir(out); len(entries) != 0 {
			t.Fatalf("%d entries written", len(entries))
		}
	}
}
