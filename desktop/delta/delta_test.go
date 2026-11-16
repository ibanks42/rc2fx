package delta

import (
	"bytes"
	"errors"
	"fmt"
	"math"
	"strings"
	"testing"
)

type file struct {
	name string
	data []byte
}

func envelope(files []file) []byte {
	var body []byte
	for _, f := range files {
		body = fmt.Appendf(body, "%s  %d  %s\n", sum(f.data), len(f.data), f.name)
	}
	for _, f := range files {
		body = append(body, f.data...)
	}
	out := []byte(envelopeMagic)
	out = fmt.Appendf(out, "length %d\n", len(body))
	out = fmt.Appendf(out, "sha256 %s\n", sum(body))
	return append(out, body...)
}

func sample() (shared, old, updated, gui []byte) {
	shared = bytes.Repeat([]byte("a file the mixer already has\n"), 20)
	old = bytes.Repeat([]byte("the previous rc2fxd build\n"), 200)
	updated = bytes.Clone(old)
	updated[7] ^= 1
	updated = append(updated, bytes.Repeat([]byte("and its new tail\n"), 20)...)
	gui = bytes.Repeat([]byte("compressible gui bytes "), 2000)
	return
}

func installedFor(shared, old []byte) map[string]string {
	return map[string]string{"librc2fx_native.so": sum(shared), "rc2fxd": sum(old)}
}

func basesFor(old []byte) map[string][]byte {
	return map[string][]byte{"rc2fxd": bytes.Clone(old)}
}

func liveFiles(shared, old []byte) func(string) ([]byte, error) {
	return func(name string) ([]byte, error) {
		switch name {
		case "librc2fx_native.so":
			return bytes.Clone(shared), nil
		case "rc2fxd":
			return bytes.Clone(old), nil
		}
		return nil, fmt.Errorf("no installed %s", name)
	}
}

func unreachable(string) ([]byte, error) { panic("the decoder read an installed file") }

func decodeAll(delta []byte, installed func(string) ([]byte, error), max uint64) ([]byte, uint64, error) {
	var restored bytes.Buffer
	written, err := Decode(bytes.NewReader(delta), installed, &restored, max)
	return restored.Bytes(), written, err
}

func mustEncode(t *testing.T, envelope []byte, installed map[string]string, bases map[string][]byte, level int) []byte {
	t.Helper()
	delta, err := Encode(envelope, installed, bases, level)
	if err != nil {
		t.Fatal(err)
	}
	return delta
}

func TestCopyZstdAndPatchRoundTripToTheSameEnvelope(t *testing.T) {
	shared, old, updated, gui := sample()
	env := envelope([]file{{"librc2fx_native.so", shared}, {"rc2fxd", updated}, {"librc2fx_gui.so", gui}})
	delta := mustEncode(t, env, installedFor(shared, old), basesFor(old), 19)
	if !bytes.HasPrefix(delta, []byte(Magic)) {
		t.Fatal("no magic")
	}
	text := string(delta)
	if !strings.Contains(text, "copy\n") {
		t.Error("an unchanged file is copied")
	}
	if !strings.Contains(text, "patch ") {
		t.Error("a changed file with a base is patched")
	}
	if !strings.Contains(text, "zstd ") {
		t.Error("a file without a base is sent whole")
	}
	if len(delta) >= len(env) {
		t.Errorf("delta %d bytes, envelope %d", len(delta), len(env))
	}
	restored, written, err := decodeAll(delta, liveFiles(shared, old), uint64(len(env)))
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(restored, env) || written != uint64(len(env)) {
		t.Fatal("the envelope was not rebuilt")
	}
}

func TestAFileTheMixerDoesNotHaveIsCompressedAlone(t *testing.T) {
	_, _, _, gui := sample()
	env := envelope([]file{{"librc2fx_gui.so", gui}})
	// A reported hash that matches the envelope would make this a copy;
	// a different one means the mixer's file is not the payload file.
	installed := map[string]string{"librc2fx_gui.so": strings.Repeat("0", 64)}
	delta := mustEncode(t, env, installed, nil, 19)
	if !strings.Contains(string(delta), "zstd ") {
		t.Fatal("not sent whole")
	}
	restored, _, err := decodeAll(delta, func(string) ([]byte, error) {
		return nil, errors.New("nothing is installed")
	}, uint64(len(env)))
	if err != nil || !bytes.Equal(restored, env) {
		t.Fatalf("not rebuilt: %v", err)
	}
}

func TestTheManifestIsReadWithoutAFixedFileCount(t *testing.T) {
	var files []file
	for index := range 20 {
		files = append(files, file{fmt.Sprintf("file-%d.bin", index), bytes.Repeat([]byte{byte(index)}, index)})
	}
	env := envelope(files)
	head, entries, err := EnvelopeEntries(env)
	if err != nil {
		t.Fatal(err)
	}
	if len(entries) != 20 {
		t.Fatalf("%d entries", len(entries))
	}
	total := head
	for _, e := range entries {
		total += int(e.Size)
	}
	if total != len(env) {
		t.Fatalf("head and files are %d bytes of %d", total, len(env))
	}
	for i, e := range entries {
		if e.Name != files[i].name || e.Size != uint64(len(files[i].data)) || e.SHA256 != sum(files[i].data) {
			t.Errorf("entry %d: %+v", i, e)
		}
	}
}

func TestMalformedEnvelopesAndDeltasAreRefused(t *testing.T) {
	shared, old, updated, _ := sample()
	env := envelope([]file{{"librc2fx_native.so", shared}, {"rc2fxd", updated}})
	valid := mustEncode(t, env, installedFor(shared, old), basesFor(old), 19)
	live := liveFiles(shared, old)

	badMagic := bytes.Clone(valid)
	badMagic[0] = 'X'
	if _, _, err := decodeAll(badMagic, live, math.MaxUint64); err == nil {
		t.Error("bad magic accepted")
	}
	if _, _, err := decodeAll(valid[:len(valid)-1], live, math.MaxUint64); err == nil {
		t.Error("truncated delta accepted")
	}
	if _, _, err := decodeAll(append(bytes.Clone(valid), 0), live, math.MaxUint64); err == nil {
		t.Error("trailing byte accepted")
	}
	unknown := bytes.Clone(valid)
	at := bytes.Index(unknown, []byte("copy\n"))
	copy(unknown[at:], "nope\n")
	if _, _, err := decodeAll(unknown, live, math.MaxUint64); err == nil {
		t.Error("unknown operation accepted")
	}

	// An envelope that has bytes after its last file. Its head alone
	// parses (decode reads only the head); encoding the whole is refused.
	if _, err := Encode(append(bytes.Clone(env), 0), nil, nil, 3); err == nil {
		t.Error("padded envelope encoded")
	}
	// The mixer's own limit, one byte short.
	_, _, err := decodeAll(valid, unreachable, uint64(len(env))-1)
	if err == nil || !strings.Contains(err.Error(), "over") {
		t.Errorf("limit: %v", err)
	}

	for _, bad := range []string{
		"",
		"RC2FX-EMBEDDED-PAYLOAD 1\nlength 1\n",
		"RC2FX-EMBEDDED-PAYLOAD 1\nlength 1\nsha256 x\n",
		fmt.Sprintf("length 3\nsha256 %s\n", strings.Repeat("0", 64)),
	} {
		if _, _, err := EnvelopeEntries([]byte(bad)); err == nil {
			t.Errorf("envelope %q accepted", bad)
		}
	}
	// A manifest with a path, a duplicate name or a non-numeric size.
	a, b := strings.Repeat("a", 64), strings.Repeat("b", 64)
	for _, line := range []string{
		a + "  1  ../escape\n",
		a + "  1  .\n",
		a + "  x  file\n",
		a + "  1  file\n" + b + "  2  file\n",
	} {
		manifest := envelopeMagic + fmt.Sprintf("length %d\n", len(line)) + "sha256 " + strings.Repeat("0", 64) + "\n" + line
		if _, _, err := EnvelopeEntries([]byte(manifest)); err == nil {
			t.Errorf("manifest %q accepted", line)
		}
	}
}

func TestAPatchAgainstTheWrongBaseIsRefused(t *testing.T) {
	shared, old, updated, _ := sample()
	env := envelope([]file{{"librc2fx_native.so", shared}, {"rc2fxd", updated}})
	delta := mustEncode(t, env, installedFor(shared, old), basesFor(old), 19)
	_, _, err := decodeAll(delta, func(name string) ([]byte, error) {
		if name == "rc2fxd" {
			return []byte("some other installed rc2fxd\n"), nil
		}
		return bytes.Clone(shared), nil
	}, uint64(len(env)))
	if err == nil || !strings.Contains(err.Error(), "base") {
		t.Fatalf("wrong base: %v", err)
	}
}

func TestACopyThatNoLongerMatchesTheEnvelopeIsRefused(t *testing.T) {
	shared, _, _, _ := sample()
	env := envelope([]file{{"librc2fx_native.so", shared}})
	delta := mustEncode(t, env, map[string]string{"librc2fx_native.so": sum(shared)}, nil, 19)
	_, _, err := decodeAll(delta, func(string) ([]byte, error) {
		return bytes.Repeat([]byte{'x'}, len(shared)), nil
	}, uint64(len(env)))
	if err == nil || !strings.Contains(err.Error(), "does not match") {
		t.Fatalf("stale copy: %v", err)
	}
}

func TestAnEnvelopeWithoutFilesOrWithStaleBytesIsNotEncoded(t *testing.T) {
	shared, _, _, _ := sample()
	env := envelope([]file{{"librc2fx_native.so", shared}})
	if _, err := Encode(env, nil, nil, 19); err != nil {
		t.Fatal(err)
	}
	if _, err := Encode(append(bytes.Clone(env), 0), nil, nil, 19); err == nil {
		t.Error("longer envelope encoded")
	}
	bad := bytes.Clone(env)
	bad[0] = 'X'
	if _, err := Encode(bad, nil, nil, 19); err == nil {
		t.Error("bad magic encoded")
	}
}

func TestAStaleBaseIsNotUsedEvenWhenTheNameMatches(t *testing.T) {
	_, old, updated, _ := sample()
	env := envelope([]file{{"rc2fxd", updated}})
	// The app's base is stale relative to what the mixer reports, so it
	// must send the file whole rather than patch against the wrong bytes.
	installed := map[string]string{"rc2fxd": sum([]byte("something else"))}
	delta := mustEncode(t, env, installed, basesFor(old), 19)
	if !strings.Contains(string(delta), "zstd ") {
		t.Fatal("patched against a stale base")
	}
	restored, _, err := decodeAll(delta, unreachable, uint64(len(env)))
	if err != nil || !bytes.Equal(restored, env) {
		t.Fatalf("not rebuilt: %v", err)
	}
}

// Go-only: the frames the encoder writes have the shape the mixer's C
// decoder needs, at every level the app could pass.
func TestFramesSuitTheMixersDecoder(t *testing.T) {
	shared, old, updated, gui := sample()
	empty := []byte{}
	env := envelope([]file{{"librc2fx_native.so", shared}, {"rc2fxd", updated}, {"librc2fx_gui.so", gui}, {"empty", empty}, {"empty-patch", empty}})
	installed := installedFor(shared, old)
	installed["empty-patch"] = sum(old)
	bases := basesFor(old)
	bases["empty-patch"] = bytes.Clone(old)
	for _, level := range []int{1, 3, 7, 19, 22} {
		delta := mustEncode(t, env, installed, bases, level)
		restored, _, err := decodeAll(delta, func(name string) ([]byte, error) {
			if name == "empty-patch" {
				return bytes.Clone(old), nil
			}
			return liveFiles(shared, old)(name)
		}, uint64(len(env)))
		if err != nil || !bytes.Equal(restored, env) {
			t.Fatalf("level %d: %v", level, err)
		}
	}
	for _, c := range []struct {
		data, prefix []byte
	}{{gui, nil}, {updated, old}, {empty, nil}, {empty, old}, {[]byte("x"), old}} {
		frame, err := compress(c.data, c.prefix, 19, windowLog(uint64(len(c.prefix)+len(c.data))))
		if err != nil {
			t.Fatal(err)
		}
		if len(frame) == 0 {
			t.Fatal("an empty frame")
		}
		descriptor := frame[4]
		if descriptor&3 != 0 {
			t.Error("the frame names a dictionary")
		}
		if descriptor&4 != 0 {
			t.Error("the frame carries a checksum")
		}
		if descriptor&0x20 == 0 {
			t.Error("the frame is not a single segment, which the C encoder writes when the window covers the file")
		}
		if n, err := frameLength(frame); err != nil || n != len(frame) {
			t.Errorf("frame length %d of %d: %v", n, len(frame), err)
		}
	}
}

func TestWindowLog(t *testing.T) {
	for n, want := range map[uint64]int{0: 20, 1: 20, 2: 20, 1 << 20: 20, 1<<20 + 1: 21, 1 << 25: 25, 1<<25 + 1: 25, math.MaxUint64: 25} {
		if got := windowLog(n); got != want {
			t.Errorf("windowLog(%d) = %d, want %d", n, got, want)
		}
	}
}
