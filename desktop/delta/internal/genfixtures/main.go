// Command genfixtures regenerates fixtures/delta for both Go and Rust tests
// (crates/rc2fx-delta/tests/shared_fixtures.rs). Run via `go generate ./delta`
// from desktop/; it wipes and recreates the output directory.
package main

import (
	"bytes"
	"encoding/binary"
	"flag"
	"fmt"
	"log"
	"math"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/delta"
	"github.com/ibanks42/rc2fx/desktop/delta/internal/fixture"
)

func main() {
	out := flag.String("out", "../../fixtures/delta", "output directory")
	flag.Parse()
	if err := os.RemoveAll(*out); err != nil {
		log.Fatal(err)
	}
	g := generator{out: *out}
	g.accept()
	g.reject()
	g.readme()
}

type generator struct{ out string }

type files = map[string][]byte

func (g generator) write(dir string, name string, data []byte) {
	path := filepath.Join(g.out, dir, name)
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		log.Fatal(err)
	}
	if err := os.WriteFile(path, data, 0o644); err != nil {
		log.Fatal(err)
	}
}

func installedFn(installed files) func(string) ([]byte, error) {
	return func(name string) ([]byte, error) {
		if data, ok := installed[name]; ok {
			return bytes.Clone(data), nil
		}
		return nil, fmt.Errorf("no installed %s", name)
	}
}

func hashes(installed files) map[string]string {
	out := map[string]string{}
	for name, data := range installed {
		out[name] = fixture.Sum(data)
	}
	return out
}

func encode(envelope []byte, installed files, level int) []byte {
	d, err := delta.Encode(envelope, hashes(installed), installed, level)
	if err != nil {
		log.Fatal(err)
	}
	return d
}

func decode(d []byte, installed files, max uint64) ([]byte, error) {
	var out bytes.Buffer
	_, err := delta.Decode(bytes.NewReader(d), installedFn(installed), &out, max)
	return out.Bytes(), err
}

func (g generator) acceptCase(name string, envelope []byte, installed files, level int) {
	d := encode(envelope, installed, level)
	got, err := decode(d, installed, uint64(len(envelope)))
	if err != nil || !bytes.Equal(got, envelope) {
		log.Fatalf("%s does not round trip: %v", name, err)
	}
	dir := filepath.Join("accept", name)
	g.write(dir, "delta", d)
	g.write(dir, "envelope", envelope)
	for file, data := range installed {
		g.write(filepath.Join(dir, "installed"), file, data)
	}
}

func (g generator) accept() {
	shared, old, updated, gui := fixture.Sample()
	installed := files{"librc2fx_native.so": shared, "rc2fxd": old}
	all := fixture.Envelope([]fixture.File{{Name: "librc2fx_native.so", Data: shared}, {Name: "rc2fxd", Data: updated}, {Name: "librc2fx_gui.so", Data: gui}})
	for _, level := range []int{1, 3, 7, 19} {
		g.acceptCase("copy-patch-zstd-level-"+strconv.Itoa(level), all, installed, level)
	}
	g.acceptCase("whole", fixture.Envelope([]fixture.File{{Name: "librc2fx_gui.so", Data: gui}}), nil, 19)
	g.acceptCase("no-files", fixture.Envelope(nil), nil, 19)
	empty := fixture.Envelope([]fixture.File{{Name: "empty", Data: nil}, {Name: "empty-patch", Data: nil}, {Name: "one-byte", Data: []byte("x")}, {Name: "tiny-patch", Data: []byte("the previous")}})
	g.acceptCase("empty-and-tiny", empty, files{"empty-patch": old, "tiny-patch": old}, 19)
	// Exercise zstd content-size header boundaries (256 B, 64 KiB) and >128 KiB
	// multi-block frames.
	var sized []fixture.File
	for i, n := range []int{255, 256, 65791, 65792, 131073} {
		sized = append(sized, fixture.File{Name: fmt.Sprintf("sized-%d", n), Data: fixture.Text(uint64(i)+10, n)})
	}
	sized = append(sized, fixture.File{Name: "random", Data: fixture.Random(9, 2000)})
	g.acceptCase("content-sizes", fixture.Envelope(sized), nil, 19)
	// Only store the delta for the large case; both Go and Rust tests rebuild
	// the inputs from fixture.Large().
	envelope, largeInstalled := fixture.Large()
	d := encode(envelope, largeInstalled, 19)
	got, err := decode(d, largeInstalled, uint64(len(envelope)))
	if err != nil || !bytes.Equal(got, envelope) {
		log.Fatalf("large does not round trip: %v", err)
	}
	if !bytes.Contains(d, []byte("patch ")) || len(d) > 64*1024 {
		log.Fatalf("large is not a small patch: %d bytes", len(d))
	}
	g.write(filepath.Join("accept", "large-patch"), "delta", d)
	g.write(filepath.Join("accept", "large-patch"), "generated", []byte("fixture.Large in desktop/delta/internal/fixture\n"))
}

// rejectCase writes a delta that both decoders must reject. An empty want
// accepts any error (for library-specific zstd messages).
func (g generator) rejectCase(name string, d []byte, installed files, max uint64, want string) {
	_, err := decode(d, installed, max)
	if err == nil {
		log.Fatalf("%s was accepted", name)
	}
	if want != "" && err.Error() != want {
		log.Fatalf("%s: got %q, want %q", name, err, want)
	}
	dir := filepath.Join("reject", name)
	g.write(dir, "delta", d)
	for file, data := range installed {
		g.write(filepath.Join(dir, "installed"), file, data)
	}
	if max != math.MaxUint64 {
		g.write(dir, "max", []byte(strconv.FormatUint(max, 10)+"\n"))
	}
	if want != "" {
		g.write(dir, "error", []byte(want+"\n"))
	}
}

// op locates the operation line for the index'th file and its frame.
type op struct {
	line       [2]int // the line, without its newline
	frame      [2]int
	compressed bool
}

func ops(d []byte) []op {
	at := len(delta.Magic)
	end := at + bytes.IndexByte(d[at:], '\n')
	head, _ := strconv.Atoi(strings.TrimPrefix(string(d[at:end]), "head "))
	at = end + 1 + head
	var out []op
	for at < len(d) {
		end := at + bytes.IndexByte(d[at:], '\n')
		line := string(d[at:end])
		o := op{line: [2]int{at, end}}
		at = end + 1
		if line != "copy" {
			fields := strings.Fields(line)
			n, _ := strconv.Atoi(fields[len(fields)-1])
			o.frame = [2]int{at, at + n}
			o.compressed = true
			at += n
		}
		out = append(out, o)
	}
	return out
}

func replaceLine(d []byte, o op, line string) []byte {
	out := bytes.Clone(d[:o.line[0]])
	out = append(out, line...)
	return append(out, d[o.line[1]:]...)
}

func withFrame(d []byte, o op, line string, frame []byte) []byte {
	out := bytes.Clone(d[:o.line[0]])
	out = append(out, line...)
	out = append(out, '\n')
	out = append(out, frame...)
	return append(out, d[o.frame[1]:]...)
}

func (g generator) reject() {
	const all = math.MaxUint64
	shared, old, updated, _ := fixture.Sample()
	installed := files{"librc2fx_native.so": shared, "rc2fxd": old}
	envelope := fixture.Envelope([]fixture.File{{Name: "librc2fx_native.so", Data: shared}, {Name: "rc2fxd", Data: updated}})
	valid := encode(envelope, installed, 19)
	o := ops(valid)
	copyOp, patchOp := o[0], o[1]
	headLine := len(delta.Magic) + bytes.IndexByte(valid[len(delta.Magic):], '\n')
	headStart := headLine + 1
	headLen := copyOp.line[0] - headStart

	bad := bytes.Clone(valid)
	bad[0] = 'X'
	g.rejectCase("bad-magic", bad, installed, all, "this is not an RC2FX delta")
	g.rejectCase("short-magic", valid[:5], installed, all, "the delta header could not be read: failed to fill whole buffer")
	g.rejectCase("empty", nil, installed, all, "the delta header could not be read: failed to fill whole buffer")
	g.rejectCase("truncated", valid[:len(valid)-1], installed, all, "rc2fxd's compressed data could not be read: failed to fill whole buffer")
	g.rejectCase("trailing-byte", append(bytes.Clone(valid), 0), installed, all, "the delta has bytes after its last file")
	g.rejectCase("unknown-operation", replaceLine(valid, copyOp, "nope"), installed, all, "librc2fx_native.so has an unknown delta operation")
	g.rejectCase("over-the-limit", valid, installed, uint64(len(envelope)-1),
		fmt.Sprintf("the envelope would be %d bytes, over the %d-byte limit", len(envelope), len(envelope)-1))
	g.rejectCase("wrong-base", valid, files{"librc2fx_native.so": shared, "rc2fxd": []byte("some other installed rc2fxd\n")}, all,
		"the installed rc2fxd is not the base this patch was made from")
	g.rejectCase("stale-copy", valid, files{"librc2fx_native.so": bytes.Repeat([]byte("x"), len(shared)), "rc2fxd": old}, all,
		"the installed librc2fx_native.so does not match the envelope")
	g.rejectCase("copy-wrong-size", valid, files{"librc2fx_native.so": shared[1:], "rc2fxd": old}, all,
		fmt.Sprintf("the installed librc2fx_native.so is %d bytes; the envelope says %d", len(shared)-1, len(shared)))
	g.rejectCase("copy-not-installed", valid, files{"rc2fxd": old}, all, "")

	// Head and operation lines.
	headless := append([]byte(delta.Magic), valid[headStart:]...)
	g.rejectCase("no-head-line", headless, installed, all, "the delta has no head line")
	g.rejectCase("head-zero", replaceHead(valid, headLine, "head 0"), installed, all, "the delta's head length is outside the supported range")
	g.rejectCase("head-too-long", replaceHead(valid, headLine, "head 65537"), installed, all, "the delta's head length is outside the supported range")
	g.rejectCase("head-not-a-number", replaceHead(valid, headLine, "head 1x"), installed, all, "the delta's head length is not a number")
	g.rejectCase("head-not-head", replaceHead(valid, headLine, "size 10"), installed, all, "the delta has no head line")
	// The head names one byte more than the envelope's manifest.
	longer := bytes.Clone(valid[:len(delta.Magic)])
	longer = fmt.Appendf(longer, "head %d\n", headLen+1)
	longer = append(longer, valid[headStart:headStart+headLen]...)
	longer = append(longer, '\n')
	longer = append(longer, valid[headStart+headLen:]...)
	g.rejectCase("head-after-manifest", longer, installed, all, "the delta's head has bytes after the envelope's manifest")
	g.rejectCase("ends-mid-line", valid[:copyOp.line[0]+2], installed, all, "the delta ends in the middle of a line")
	g.rejectCase("line-too-long", replaceLine(valid, copyOp, strings.Repeat("c", 600)), installed, all, "a delta operation line is too long")
	g.rejectCase("line-not-text", replaceLine(valid, copyOp, "copy\xc3\xa9"), installed, all, "the delta operation is not text")

	patchLine := string(valid[patchOp.line[0]:patchOp.line[1]])
	fields := strings.Fields(patchLine)
	frame := valid[patchOp.frame[0]:patchOp.frame[1]]
	g.rejectCase("patch-base-malformed", replaceLine(valid, patchOp, "patch xyz "+fields[2]), installed, all, "rc2fxd's patch base is malformed")
	g.rejectCase("patch-base-missing", replaceLine(valid, patchOp, "patch"), installed, all, "rc2fxd's patch base is malformed")
	g.rejectCase("length-missing", replaceLine(valid, patchOp, "patch "+fields[1]), installed, all, "rc2fxd's compressed length is missing")
	g.rejectCase("length-not-a-number", replaceLine(valid, patchOp, "patch "+fields[1]+" 12z"), installed, all, "rc2fxd's compressed length is not a number")
	g.rejectCase("length-zero", replaceLine(valid, patchOp, "patch "+fields[1]+" 0"), installed, all, "rc2fxd's compressed length is outside the supported range")
	g.rejectCase("length-over-limit", replaceLine(valid, patchOp, "zstd "+strconv.Itoa(len(envelope)+1<<20+1)), installed, uint64(len(envelope)),
		"rc2fxd's compressed length is outside the supported range")
	g.rejectCase("trailing-fields", replaceLine(valid, patchOp, patchLine+" 1"), installed, all, "rc2fxd's delta operation has trailing fields")
	g.rejectCase("double-space", replaceLine(valid, patchOp, "patch  "+fields[1]+" "+fields[2]), installed, all, "rc2fxd's patch base is malformed")

	// Frames.
	g.rejectCase("frame-trailing-byte", withFrame(valid, patchOp, fmt.Sprintf("patch %s %d", fields[1], len(frame)+1), append(bytes.Clone(frame), 0)), installed, all,
		"rc2fxd's compressed frame has trailing bytes")
	g.rejectCase("frame-two-frames", withFrame(valid, patchOp, fmt.Sprintf("patch %s %d", fields[1], 2*len(frame)), append(bytes.Clone(frame), frame...)), installed, all,
		"rc2fxd's compressed frame has trailing bytes")
	g.rejectCase("frame-truncated", withFrame(valid, patchOp, fmt.Sprintf("patch %s %d", fields[1], len(frame)-1), frame[:len(frame)-1]), installed, all,
		"rc2fxd's compressed frame is truncated")
	// The patch decoded without its base.
	g.rejectCase("patch-as-zstd", replaceLine(valid, patchOp, "zstd "+fields[2]), installed, all, "")
	corrupt := bytes.Clone(frame)
	corrupt[len(corrupt)/2] ^= 0x55
	g.rejectCase("frame-corrupt", withFrame(valid, patchOp, patchLine, corrupt), installed, all, "")

	// Hand-crafted raw-block frames to control decoded output length exactly.
	envelopeOne := fixture.Envelope([]fixture.File{{Name: "file", Data: []byte("0123456789")}})
	head, _, err := delta.EnvelopeEntries(envelopeOne)
	if err != nil {
		log.Fatal(err)
	}
	oneDelta := func(frame []byte) []byte {
		d := []byte(delta.Magic)
		d = fmt.Appendf(d, "head %d\n", head)
		d = append(d, envelopeOne[:head]...)
		d = fmt.Appendf(d, "zstd %d\n", len(frame))
		return append(d, frame...)
	}
	g.acceptCaseRaw("hand-made-raw-frame", oneDelta(rawFrame([]byte("0123456789"), 20, true)), envelopeOne)
	g.rejectCase("decodes-longer", oneDelta(rawFrame([]byte("0123456789A"), 20, true)), nil, all,
		"file decoded to more than the 10 bytes its manifest line says")
	g.rejectCase("decodes-shorter", oneDelta(rawFrame([]byte("012345678"), 20, true)), nil, all,
		"file decoded to 9 bytes; its manifest line says 10")
	g.rejectCase("decodes-different", oneDelta(rawFrame([]byte("0123456788"), 20, true)), nil, all,
		"file does not match its checksum in the envelope")
	g.rejectCase("window-too-large", oneDelta(rawFrame([]byte("0123456789"), 26, false)), nil, all, "")
	g.rejectCase("file-over-the-limit", oneDelta(rawFrame([]byte("0123456789"), 20, true)), nil, uint64(len(envelopeOne)-1),
		fmt.Sprintf("the envelope would be %d bytes, over the %d-byte limit", len(envelopeOne), len(envelopeOne)-1))
	notZstd := bytes.Clone(rawFrame([]byte("0123456789"), 20, true))
	notZstd[0] ^= 1
	g.rejectCase("not-a-zstd-frame", oneDelta(notZstd), nil, all, "")
}

func replaceHead(d []byte, headLine int, line string) []byte {
	out := []byte(delta.Magic)
	out = append(out, line...)
	return append(out, d[headLine:]...)
}

func (g generator) acceptCaseRaw(name string, d, envelope []byte) {
	got, err := decode(d, nil, uint64(len(envelope)))
	if err != nil || !bytes.Equal(got, envelope) {
		log.Fatalf("%s does not decode: %v", name, err)
	}
	dir := filepath.Join("accept", name)
	g.write(dir, "delta", d)
	g.write(dir, "envelope", envelope)
}

// rawFrame builds a minimal single-raw-block zstd frame. When single is false,
// it emits an explicit 2^windowLog window descriptor with no content size.
func rawFrame(data []byte, windowLog int, single bool) []byte {
	frame := binary.LittleEndian.AppendUint32(nil, 0xfd2fb528)
	if single {
		// Single segment, one-byte content size.
		frame = append(frame, 0x20, byte(len(data)))
	} else {
		frame = append(frame, 0x00, byte((windowLog-10)<<3))
	}
	header := uint32(len(data))<<3 | 1 // raw, last
	frame = append(frame, byte(header), byte(header>>8), byte(header>>16))
	return append(frame, data...)
}

func (g generator) readme() {
	g.write(".", "README.md", []byte(`# Delta fixtures

Written by `+"`go generate ./delta`"+` in desktop/ (desktop/delta/internal/genfixtures);
do not edit by hand. Read by desktop/delta (Go) and
crates/rc2fx-delta/tests/shared_fixtures.rs (Rust), so the mixer's Rust
decoder keeps accepting what the desktop's Go encoder writes.

- `+"`accept/<case>/delta`"+` decodes to `+"`accept/<case>/envelope`"+` with a limit of the
  envelope's own length. `+"`installed/<name>`"+` are the files on the mixer; a file
  missing there is not installed.
- `+"`accept/large-patch`"+` stores only the delta; its envelope and installed files are
  rebuilt by both tests from the recipe in desktop/delta/internal/fixture
  (`+"`Large`"+`), which keeps a patch whose matches reach over 1 MiB out of the repo.
- `+"`reject/<case>/delta`"+` must be refused. `+"`max`"+` is the envelope limit (none: no
  limit). `+"`error`"+` is the exact message both decoders give; without it any
  refusal will do (the zstd libraries word their own errors differently).
`))
}
