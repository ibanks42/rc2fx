package delta

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"io"
	"math"
	"strconv"
	"strings"
	"testing"

	"github.com/klauspost/compress/zstd"
)

type testFile struct {
	name string
	data []byte
}

func makeEnvelope(files []testFile) []byte {
	var body []byte
	for _, f := range files {
		body = fmt.Appendf(body, "%s  %d  %s\n", sum(f.data), len(f.data), f.name)
	}
	for _, f := range files {
		body = append(body, f.data...)
	}
	out := []byte("RC2FX-EMBEDDED-PAYLOAD 1\n")
	out = fmt.Appendf(out, "length %d\n", len(body))
	out = fmt.Appendf(out, "sha256 %s\n", sum(body))
	return append(out, body...)
}

func sampleFiles() (shared, old, updated, gui []byte) {
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

func mustEncode(t *testing.T, envelope []byte, installed map[string]string, bases map[string][]byte, level int) []byte {
	t.Helper()
	d, err := Encode(envelope, installed, bases, level)
	if err != nil {
		t.Fatal(err)
	}
	return d
}

// verifyDelta parses a well-formed delta stream produced by Encode and
// decompresses its frames using klauspost/zstd to check Go-side round-trips
// on platforms where Rust isn't run (Windows/macOS). Full wire decoding and
// hostile-input checks live in crates/rc2fx-delta.
func verifyDelta(t *testing.T, d []byte, installed map[string][]byte, wantEnvelope []byte) {
	t.Helper()
	if !bytes.HasPrefix(d, []byte(Magic)) {
		t.Fatal("missing delta magic")
	}
	rest := d[len(Magic):]
	headLine, rest, ok := bytes.Cut(rest, []byte{'\n'})
	if !ok {
		t.Fatal("missing head line")
	}
	headLen, err := strconv.Atoi(strings.TrimPrefix(string(headLine), "head "))
	if err != nil || headLen > len(rest) {
		t.Fatalf("bad head line %q", headLine)
	}
	head := rest[:headLen]
	rest = rest[headLen:]
	_, entries, err := EnvelopeEntries(head)
	if err != nil {
		t.Fatalf("EnvelopeEntries(head): %v", err)
	}
	var rebuilt bytes.Buffer
	rebuilt.Write(head)
	for _, entry := range entries {
		opLine, after, ok := bytes.Cut(rest, []byte{'\n'})
		if !ok {
			t.Fatalf("%s: missing op line", entry.Name)
		}
		rest = after
		fields := strings.Fields(string(opLine))
		switch fields[0] {
		case "copy":
			data, ok := installed[entry.Name]
			if !ok {
				t.Fatalf("%s: copy without installed file", entry.Name)
			}
			rebuilt.Write(data)
		case "zstd", "patch":
			n, err := strconv.Atoi(fields[len(fields)-1])
			if err != nil || n > len(rest) {
				t.Fatalf("%s: bad op %q", entry.Name, opLine)
			}
			frame := rest[:n]
			rest = rest[n:]
			opts := []zstd.DOption{zstd.WithDecoderMaxWindow(1 << windowLogMax)}
			if fields[0] == "patch" {
				base := installed[entry.Name]
				if sum(base) != fields[1] {
					t.Fatalf("%s: patch base hash mismatch", entry.Name)
				}
				if len(base) > 0 {
					opts = append(opts, zstd.WithDecoderDictRaw(0, base))
				}
			}
			dec, err := zstd.NewReader(bytes.NewReader(frame), opts...)
			if err != nil {
				t.Fatalf("%s: zstd.NewReader: %v", entry.Name, err)
			}
			plain, err := io.ReadAll(dec)
			dec.Close()
			if err != nil {
				t.Fatalf("%s: decompress: %v", entry.Name, err)
			}
			rebuilt.Write(plain)
		default:
			t.Fatalf("%s: unknown op %q", entry.Name, opLine)
		}
	}
	if len(rest) != 0 {
		t.Fatalf("%d trailing bytes", len(rest))
	}
	if !bytes.Equal(rebuilt.Bytes(), wantEnvelope) {
		t.Fatal("rebuilt envelope differs")
	}
}

func TestCopyZstdAndPatchRoundTripToTheSameEnvelope(t *testing.T) {
	shared, old, updated, gui := sampleFiles()
	env := makeEnvelope([]testFile{{"librc2fx_native.so", shared}, {"rc2fxd", updated}, {"librc2fx_gui.so", gui}})
	d := mustEncode(t, env, installedFor(shared, old), basesFor(old), 19)
	text := string(d)
	if !strings.Contains(text, "copy\n") {
		t.Error("an unchanged file is copied")
	}
	if !strings.Contains(text, "patch ") {
		t.Error("a changed file with a base is patched")
	}
	if !strings.Contains(text, "zstd ") {
		t.Error("a file without a base is sent whole")
	}
	if len(d) >= len(env) {
		t.Errorf("delta %d bytes, envelope %d", len(d), len(env))
	}
	verifyDelta(t, d, map[string][]byte{"librc2fx_native.so": shared, "rc2fxd": old}, env)
}

func TestAFileTheMixerDoesNotHaveIsCompressedAlone(t *testing.T) {
	_, _, _, gui := sampleFiles()
	env := makeEnvelope([]testFile{{"librc2fx_gui.so", gui}})
	installed := map[string]string{"librc2fx_gui.so": strings.Repeat("0", 64)}
	d := mustEncode(t, env, installed, nil, 19)
	if !strings.Contains(string(d), "zstd ") {
		t.Fatal("not sent whole")
	}
	verifyDelta(t, d, nil, env)
}

func TestTheManifestIsReadWithoutAFixedFileCount(t *testing.T) {
	var files []testFile
	for index := range 20 {
		files = append(files, testFile{fmt.Sprintf("file-%d.bin", index), bytes.Repeat([]byte{byte(index)}, index)})
	}
	env := makeEnvelope(files)
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

func TestMalformedEnvelopesAreRefused(t *testing.T) {
	shared, _, _, _ := sampleFiles()
	env := makeEnvelope([]testFile{{"librc2fx_native.so", shared}})
	if _, err := Encode(env, nil, nil, 19); err != nil {
		t.Fatal(err)
	}
	if _, err := Encode(append(bytes.Clone(env), 0), nil, nil, 19); err == nil {
		t.Error("padded envelope encoded")
	}
	bad := bytes.Clone(env)
	bad[0] = 'X'
	if _, err := Encode(bad, nil, nil, 19); err == nil {
		t.Error("bad magic encoded")
	}
	for _, raw := range []string{
		"",
		"RC2FX-EMBEDDED-PAYLOAD 1\nlength 1\n",
		"RC2FX-EMBEDDED-PAYLOAD 1\nlength 1\nsha256 x\n",
		fmt.Sprintf("length 3\nsha256 %s\n", strings.Repeat("0", 64)),
	} {
		if _, _, err := EnvelopeEntries([]byte(raw)); err == nil {
			t.Errorf("envelope %q accepted", raw)
		}
	}
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

func TestAStaleBaseIsNotUsedEvenWhenTheNameMatches(t *testing.T) {
	_, old, updated, _ := sampleFiles()
	env := makeEnvelope([]testFile{{"rc2fxd", updated}})
	installed := map[string]string{"rc2fxd": sum([]byte("something else"))}
	d := mustEncode(t, env, installed, basesFor(old), 19)
	if !strings.Contains(string(d), "zstd ") {
		t.Fatal("patched against a stale base")
	}
	verifyDelta(t, d, nil, env)
}

// Verify encoded frames match the header flags the mixer's C decoder expects.
func TestFramesSuitTheMixersDecoder(t *testing.T) {
	shared, old, updated, gui := sampleFiles()
	empty := []byte{}
	env := makeEnvelope([]testFile{
		{"librc2fx_native.so", shared},
		{"rc2fxd", updated},
		{"librc2fx_gui.so", gui},
		{"empty", empty},
		{"empty-patch", empty},
	})
	installed := installedFor(shared, old)
	installed["empty-patch"] = sum(old)
	bases := basesFor(old)
	bases["empty-patch"] = bytes.Clone(old)
	for _, level := range []int{1, 3, 7, 19, 22} {
		d := mustEncode(t, env, installed, bases, level)
		verifyDelta(t, d, map[string][]byte{
			"librc2fx_native.so": shared,
			"rc2fxd":             old,
			"empty-patch":        old,
		}, env)
	}
	for _, c := range []struct {
		data, prefix []byte
	}{{gui, nil}, {updated, old}, {empty, nil}, {empty, old}, {[]byte("x"), old}} {
		frame, err := compress(c.data, c.prefix, 19, windowLog(uint64(len(c.prefix)+len(c.data))))
		if err != nil {
			t.Fatal(err)
		}
		if len(frame) < 5 || binary.LittleEndian.Uint32(frame) != 0xfd2fb528 {
			t.Fatalf("bad zstd frame header (%d bytes)", len(frame))
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
	}
}

func TestWindowLog(t *testing.T) {
	for n, want := range map[uint64]int{0: 20, 1: 20, 2: 20, 1 << 20: 20, 1<<20 + 1: 21, 1 << 25: 25, 1<<25 + 1: 25, math.MaxUint64: 25} {
		if got := windowLog(n); got != want {
			t.Errorf("windowLog(%d) = %d, want %d", n, got, want)
		}
	}
}
