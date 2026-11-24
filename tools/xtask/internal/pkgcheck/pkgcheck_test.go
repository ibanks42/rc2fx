package pkgcheck

import (
	"archive/zip"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"
)

var timeZero = time.Unix(0, 0)

func write(t *testing.T, path string, data []byte) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o777); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatal(err)
	}
}

func envelope(body string, length int, digest string) []byte {
	return []byte(fmt.Sprintf("ELF...%s\nlength %d\nsha256 %s\n%s...rest", payloadMagic, length, digest, body))
}

func TestExpectEmbeddedEnvelope(t *testing.T) {
	sum := sha256.Sum256([]byte("payload"))
	good := hex.EncodeToString(sum[:])
	for _, c := range []struct {
		name string
		data []byte
		ok   bool
	}{
		{"download mode", []byte("x RC2FX-PAYLOAD-MODE:download:1 x"), true},
		{"both modes", []byte("RC2FX-PAYLOAD-MODE:download:1 RC2FX-PAYLOAD-MODE:embedded:1"), false},
		{"embedded", envelope("payload", 7, good), true},
		{"no payload", []byte("nothing"), false},
		{"two payloads", append(envelope("payload", 7, good), payloadMagic...), false},
		{"wrong digest", envelope("payloaD", 7, good), false},
		{"short digest", envelope("payload", 7, good[:63]), false},
		{"upper-case digest", envelope("payload", 7, strings.ToUpper(good)), false},
		{"cut short", envelope("pay", 7, good)[:len(envelope("pay", 7, good))-len("...rest")], false},
	} {
		path := filepath.Join(t.TempDir(), "rc2fx")
		write(t, path, c.data)
		if err := ExpectEmbeddedEnvelope(path); (err == nil) != c.ok {
			t.Errorf("%s: %v", c.name, err)
		}
	}
}

func TestCheckAppImageHeader(t *testing.T) {
	dir := t.TempDir()
	for name, c := range map[string]struct {
		data string
		ok   bool
	}{
		"type2": {"\x7fELF\x02\x01\x01\x00AI\x02rest", true},
		"type1": {"\x7fELF\x02\x01\x01\x00AI\x01rest", false},
		"plain": {"\x7fELF\x02\x01\x01\x00\x00\x00\x00rest", false},
		"short": {"\x7fELF", false},
	} {
		path := filepath.Join(dir, name)
		write(t, path, []byte(c.data))
		if err := checkAppImageHeader(path); (err == nil) != c.ok {
			t.Errorf("%s: %v", name, err)
		}
	}
}

func TestCheckFeed(t *testing.T) {
	dir := t.TempDir()
	appimage := filepath.Join(dir, "rc2fx-linux-x64.AppImage")
	write(t, appimage, []byte("appimage"))
	nupkg := filepath.Join(dir, "io.github.ibanks42.rc2fx-1.2.3-linux-full.nupkg")
	file, err := os.Create(nupkg)
	if err != nil {
		t.Fatal(err)
	}
	archive := zip.NewWriter(file)
	member, _ := archive.Create("lib/app/io.github.ibanks42.rc2fx.AppImage")
	member.Write([]byte("appimage"))
	archive.Close()
	file.Close()
	digest, err := fileSHA256(nupkg)
	if err != nil {
		t.Fatal(err)
	}
	feed := filepath.Join(dir, "releases.linux.json")
	asset := func(version, sha string) string {
		return fmt.Sprintf(`{"Assets":[{"PackageId":"io.github.ibanks42.rc2fx","Version":%q,"Type":"Full","FileName":%q,"SHA256":%q}]}`,
			version, filepath.Base(nupkg), sha)
	}
	write(t, feed, []byte(asset("1.2.3", strings.ToUpper(digest))))
	if err := checkFeed(feed, nupkg, appimage, "1.2.3"); err != nil {
		t.Errorf("matching feed: %v", err)
	}
	write(t, feed, []byte(asset("1.2.4", digest)))
	if err := checkFeed(feed, nupkg, appimage, "1.2.3"); err == nil {
		t.Error("a feed for another version passed")
	}
	write(t, feed, []byte(asset("1.2.3", digest)))
	write(t, appimage, []byte("other"))
	if err := checkFeed(feed, nupkg, appimage, "1.2.3"); err == nil {
		t.Error("a package holding another AppImage passed")
	}
}

func TestSourceDigestIgnoresTimesButNotModes(t *testing.T) {
	dir := t.TempDir()
	write(t, filepath.Join(dir, "a"), []byte("one"))
	files := []string{"a"}
	first, err := sourceDigest(dir, files)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(filepath.Join(dir, "a"), timeZero, timeZero); err != nil {
		t.Fatal(err)
	}
	if again, _ := sourceDigest(dir, files); again != first {
		t.Error("a new modification time changed the digest")
	}
	// Windows has no execute bit to change; package-check runs on Linux.
	if runtime.GOOS == "windows" {
		return
	}
	if err := os.Chmod(filepath.Join(dir, "a"), 0o755); err != nil {
		t.Fatal(err)
	}
	if changed, _ := sourceDigest(dir, files); changed == first {
		t.Error("a new mode left the digest alone")
	}
}

func TestImageTagAndRustVersion(t *testing.T) {
	dir := t.TempDir()
	write(t, filepath.Join(dir, filepath.FromSlash(containerfile)), []byte("FROM x\n"))
	write(t, filepath.Join(dir, "rust-toolchain.toml"), []byte("[toolchain]\nchannel = \"1.100.0\"\n"))
	tag, err := ImageTag(dir)
	sum := sha256.Sum256([]byte("FROM x\n[toolchain]\nchannel = \"1.100.0\"\n"))
	if err != nil || tag != hex.EncodeToString(sum[:])[:12] {
		t.Errorf("tag %q, %v", tag, err)
	}
	if rust, err := RustVersion(dir); err != nil || rust != "1.100.0" {
		t.Errorf("rust %q, %v", rust, err)
	}
}
