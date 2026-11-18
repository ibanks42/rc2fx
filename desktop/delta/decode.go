package delta

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"math"
	"strings"

	"github.com/klauspost/compress/zstd"
)

// Decode rebuilds the envelope from delta into out. It is the Go twin of
// the mixer's Rust decoder, for tests; the app itself only encodes, and the
// linker leaves Decode out of the app. It lives outside a _test file because
// internal/genfixtures needs it too: the generator decodes every case before
// writing it, so an accept case that does not round trip, or a reject case
// whose recorded error is wrong, never reaches fixtures/delta. It shares the
// line and number parsers with Encode, which is why it is not a separate
// package.
//
// installed(name) returns the bytes of the file currently on the mixer. It is
// used for copies and as the patch prefix, and must match the manifest (a
// copy) or the patch's base hash. Returns the envelope bytes written.
//
// Messages match the Rust decoder's, except for errors the zstd library
// itself reports, which name the Go library's error instead of a C error
// code, and I/O errors, which carry Go's wording.
func Decode(delta io.Reader, installed func(name string) ([]byte, error), out io.Writer, maxEnvelope uint64) (uint64, error) {
	magic := make([]byte, len(Magic))
	if err := readExact(delta, magic, "the delta header"); err != nil {
		return 0, err
	}
	if string(magic) != Magic {
		return 0, errors.New("this is not an RC2FX delta")
	}
	line, err := opLine(delta)
	if err != nil {
		return 0, err
	}
	text, ok := strings.CutPrefix(line, "head ")
	if !ok {
		return 0, errors.New("the delta has no head line")
	}
	headLen, ok := parseU64(text)
	if !ok {
		return 0, errors.New("the delta's head length is not a number")
	}
	if headLen == 0 || headLen > maxHead {
		return 0, errors.New("the delta's head length is outside the supported range")
	}
	head := make([]byte, headLen)
	if err := readExact(delta, head, "the delta's head"); err != nil {
		return 0, err
	}
	manifestLen, entries, err := EnvelopeEntries(head)
	if err != nil {
		return 0, err
	}
	if manifestLen != len(head) {
		return 0, errors.New("the delta's head has bytes after the envelope's manifest")
	}
	sizes, err := sumSizes(entries)
	if err != nil {
		return 0, err
	}
	envelopeLen := headLen + sizes
	if envelopeLen < headLen {
		return 0, errors.New("the envelope's length overflows")
	}
	if envelopeLen > maxEnvelope {
		return 0, fmt.Errorf("the envelope would be %d bytes, over the %d-byte limit", envelopeLen, maxEnvelope)
	}
	if _, err := out.Write(head); err != nil {
		return 0, fmt.Errorf("cannot write the envelope: %v", err)
	}
	written := headLen
	opLimit := saturatingAdd(maxEnvelope, opOverhead)

	for i := range entries {
		entry := &entries[i]
		line, err := opLine(delta)
		if err != nil {
			return 0, err
		}
		if line == "copy" {
			b, err := installed(entry.Name)
			if err != nil {
				return 0, fmt.Errorf("cannot read the installed %s: %v", entry.Name, err)
			}
			if uint64(len(b)) != entry.Size {
				return 0, fmt.Errorf("the installed %s is %d bytes; the envelope says %d", entry.Name, len(b), entry.Size)
			}
			if !strings.EqualFold(sum(b), entry.SHA256) {
				return 0, fmt.Errorf("the installed %s does not match the envelope", entry.Name)
			}
			if _, err := out.Write(b); err != nil {
				return 0, fmt.Errorf("cannot write %s: %v", entry.Name, err)
			}
			written += uint64(len(b))
			continue
		}
		baseHash, compressedLen, err := parseOp(line, entry.Name, opLimit)
		if err != nil {
			return 0, err
		}
		frame := make([]byte, compressedLen)
		if err := readExact(delta, frame, entry.Name+"'s compressed data"); err != nil {
			return 0, err
		}
		var base []byte
		if baseHash != "" {
			b, err := installed(entry.Name)
			if err != nil {
				return 0, fmt.Errorf("cannot read the installed %s: %v", entry.Name, err)
			}
			if !strings.EqualFold(sum(b), baseHash) {
				return 0, fmt.Errorf("the installed %s is not the base this patch was made from", entry.Name)
			}
			base = b
		}
		if err := decompress(frame, base, entry, out, &written, maxEnvelope); err != nil {
			return 0, err
		}
	}
	end, err := atEnd(delta)
	if err != nil {
		return 0, err
	}
	if !end {
		return 0, errors.New("the delta has bytes after its last file")
	}
	return written, nil
}

// parseOp reads a `zstd <len>` or `patch <hash> <len>` line. The base hash
// is empty for `zstd`.
func parseOp(line, name string, opLimit uint64) (string, int, error) {
	parts := strings.Split(line, " ")
	next := func() (string, bool) {
		if len(parts) == 0 {
			return "", false
		}
		part := parts[0]
		parts = parts[1:]
		return part, true
	}
	var base string
	switch op, _ := next(); op {
	case "zstd":
	case "patch":
		hash, ok := next()
		if !ok || !validHash(hash) {
			return "", 0, fmt.Errorf("%s's patch base is malformed", name)
		}
		base = strings.ToLower(hash)
	default:
		return "", 0, fmt.Errorf("%s has an unknown delta operation", name)
	}
	text, ok := next()
	if !ok {
		return "", 0, fmt.Errorf("%s's compressed length is missing", name)
	}
	n, ok := parseU64(text)
	if !ok {
		return "", 0, fmt.Errorf("%s's compressed length is not a number", name)
	}
	if len(parts) > 0 {
		return "", 0, fmt.Errorf("%s's delta operation has trailing fields", name)
	}
	if n == 0 || n > opLimit {
		return "", 0, fmt.Errorf("%s's compressed length is outside the supported range", name)
	}
	if n > math.MaxInt {
		return "", 0, fmt.Errorf("%s's compressed data is too large", name)
	}
	return base, int(n), nil
}

// decompress decodes one file, checking its exact size and digest as the
// bytes go to out. written is the envelope length so far.
func decompress(data, prefix []byte, entry *Entry, out io.Writer, written *uint64, maxEnvelope uint64) error {
	if entry.Size > maxEnvelope-min(*written, maxEnvelope) {
		return fmt.Errorf("%s would take the envelope over the %d-byte limit", entry.Name, maxEnvelope)
	}
	// The C decoder stops at the end of the first frame; anything after it
	// is trailing data, and a frame that runs out of input is truncated.
	frameLen, err := frameLength(data)
	switch {
	case errors.Is(err, errTruncated):
		return fmt.Errorf("%s's compressed frame is truncated", entry.Name)
	case err != nil:
		return zstdError(err)
	case frameLen != len(data):
		return fmt.Errorf("%s's compressed frame has trailing bytes", entry.Name)
	}
	options := []zstd.DOption{
		zstd.WithDecoderConcurrency(1),
		zstd.WithDecoderMaxWindow(1 << windowLogMax),
		zstd.WithDecoderLowmem(true),
	}
	if len(prefix) > 0 {
		options = append(options, zstd.WithDecoderDictRaw(0, prefix))
	}
	decoder, err := zstd.NewReader(bytes.NewReader(data), options...)
	if err != nil {
		return zstdError(err)
	}
	defer decoder.Close()
	buffer := make([]byte, 64*1024)
	var total uint64
	hasher := sha256.New()
	for {
		count, err := decoder.Read(buffer)
		if count > 0 {
			total += uint64(count)
			if total > entry.Size {
				return fmt.Errorf("%s decoded to more than the %d bytes its manifest line says", entry.Name, entry.Size)
			}
			chunk := buffer[:count]
			hasher.Write(chunk)
			if _, err := out.Write(chunk); err != nil {
				return fmt.Errorf("cannot write %s: %v", entry.Name, err)
			}
		}
		if err == io.EOF {
			break
		}
		if errors.Is(err, io.ErrUnexpectedEOF) {
			return fmt.Errorf("%s's compressed frame is truncated", entry.Name)
		}
		if err != nil {
			return zstdError(err)
		}
	}
	if total != entry.Size {
		return fmt.Errorf("%s decoded to %d bytes; its manifest line says %d", entry.Name, total, entry.Size)
	}
	if !strings.EqualFold(hex.EncodeToString(hasher.Sum(nil)), entry.SHA256) {
		return fmt.Errorf("%s does not match its checksum in the envelope", entry.Name)
	}
	*written += total
	return nil
}

var errTruncated = errors.New("truncated")

// frameLength walks one zstd frame's header and block headers and returns
// its length. A skippable frame is refused: the delta writes none.
func frameLength(data []byte) (int, error) {
	const frameMagic = 0xfd2fb528
	if len(data) < 5 {
		return 0, errTruncated
	}
	if binary.LittleEndian.Uint32(data) != frameMagic {
		return 0, errors.New("unknown frame magic")
	}
	descriptor := data[4]
	if descriptor&0x08 != 0 {
		return 0, errors.New("reserved frame header bit set")
	}
	single := descriptor&0x20 != 0
	at := 5
	if !single {
		at++
	}
	at += [4]int{0, 1, 2, 4}[descriptor&3]
	switch fcs := descriptor >> 6; {
	case fcs == 0 && single:
		at++
	case fcs == 1:
		at += 2
	case fcs == 2:
		at += 4
	case fcs == 3:
		at += 8
	}
	for {
		if at+3 > len(data) {
			return 0, errTruncated
		}
		header := uint32(data[at]) | uint32(data[at+1])<<8 | uint32(data[at+2])<<16
		at += 3
		last := header&1 != 0
		size := int(header >> 3)
		switch (header >> 1) & 3 {
		case 0, 2:
			at += size
		case 1:
			at++
		default:
			return 0, errors.New("reserved block type")
		}
		if at > len(data) {
			return 0, errTruncated
		}
		if last {
			break
		}
	}
	if descriptor&0x04 != 0 {
		at += 4
		if at > len(data) {
			return 0, errTruncated
		}
	}
	return at, nil
}

// opLine reads one `\n`-terminated operation line from the delta stream,
// a byte at a time so nothing after it is consumed.
func opLine(r io.Reader) (string, error) {
	var line []byte
	var b [1]byte
	for {
		n, err := r.Read(b[:])
		if n == 1 {
			switch {
			case b[0] == '\n' && len(line) <= maxLine:
				return string(line), nil
			case len(line) >= maxLine:
				return "", errors.New("a delta operation line is too long")
			case b[0] >= 0x80:
				return "", errors.New("the delta operation is not text")
			}
			line = append(line, b[0])
			continue
		}
		if err == io.EOF {
			return "", errors.New("the delta ends in the middle of a line")
		}
		if err != nil {
			return "", fmt.Errorf("the delta could not be read: %v", err)
		}
	}
}

func readExact(r io.Reader, b []byte, what string) error {
	if _, err := io.ReadFull(r, b); err != nil {
		if err == io.EOF || err == io.ErrUnexpectedEOF {
			// Rust's read_exact words a short read this way.
			return fmt.Errorf("%s could not be read: failed to fill whole buffer", what)
		}
		return fmt.Errorf("%s could not be read: %v", what, err)
	}
	return nil
}

func atEnd(r io.Reader) (bool, error) {
	var b [1]byte
	for {
		n, err := r.Read(b[:])
		if n > 0 {
			return false, nil
		}
		if err == io.EOF {
			return true, nil
		}
		if err != nil {
			return false, fmt.Errorf("the delta could not be read: %v", err)
		}
	}
}
