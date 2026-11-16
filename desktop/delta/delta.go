// Package delta is delta encoding for an in-place mixer payload upload.
//
// The signed envelope (`RC2FX-EMBEDDED-PAYLOAD 1`) is still the authority:
// this package only carries it more cheaply over a slow USB link. Each file
// is sent whole (zstd), unchanged (copy), or as a zstd patch against the copy
// installed on the mixer. Decoding rebuilds the exact envelope and checks
// every file against the manifest inside it; the mixer then verifies the
// rebuilt envelope against the signed target exactly as it did before.
//
// Port of crates/rc2fx-delta. The mixer keeps the Rust decoder, which drives
// the C zstd library with a raw reference prefix (`zstd --patch-from`) and a
// window bound of 2^25; Decode here is its twin for tests. What the encoder
// writes is pinned by fixtures/delta, which the Rust decoder reads too.
package delta

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"math"
	"math/bits"
	"strconv"
	"strings"

	"github.com/klauspost/compress/zstd"
)

//go:generate go run ./internal/genfixtures -out ../../fixtures/delta

const (
	Magic         = "RC2FX-DELTA 1\n"
	envelopeMagic = "RC2FX-EMBEDDED-PAYLOAD 1\n"
	maxLine       = 512
	// The head is the envelope header and its manifest, not any file bytes.
	maxHead    = 64 * 1024
	maxEntries = 256
	// The decoder refuses a frame that needs a larger window than this, so a
	// hostile delta cannot make it allocate wildly.
	windowLogMax = 25
	// A compressed frame may be slightly larger than the bytes it replaces.
	opOverhead = 1 << 20
)

// Entry is one file in the envelope's manifest.
type Entry struct {
	Name   string
	SHA256 string
	Size   uint64
}

// EnvelopeEntries parses an envelope's head: the three header lines and the
// manifest. It returns the head's length and the manifest. The manifest has
// no fixed file count, so it ends when the manifest bytes plus every
// declared size fill the envelope's declared body length.
func EnvelopeEntries(envelope []byte) (int, []Entry, error) {
	if len(envelope) < len(envelopeMagic) {
		return 0, nil, errors.New("the envelope header is truncated")
	}
	if string(envelope[:len(envelopeMagic)]) != envelopeMagic {
		return 0, nil, errors.New("this is not an RC2FX envelope")
	}
	line, cursor, err := lineAt(envelope, len(envelopeMagic))
	if err != nil {
		return 0, nil, err
	}
	text, ok := strings.CutPrefix(line, "length ")
	if !ok {
		return 0, nil, errors.New("the envelope has no length line")
	}
	length, ok := parseU64(text)
	if !ok {
		return 0, nil, errors.New("the envelope's length is not a number")
	}
	line, cursor, err = lineAt(envelope, cursor)
	if err != nil {
		return 0, nil, err
	}
	digest, ok := strings.CutPrefix(line, "sha256 ")
	if !ok {
		return 0, nil, errors.New("the envelope has no digest line")
	}
	if !validHash(digest) {
		return 0, nil, errors.New("the envelope's digest is malformed")
	}
	body := cursor

	var entries []Entry
	var sizes uint64
	for {
		// A payload with no files is still a complete manifest.
		if saturatingAdd(uint64(cursor-body), sizes) == length {
			return cursor, entries, nil
		}
		if cursor > maxHead {
			return 0, nil, errors.New("the envelope's manifest is too long")
		}
		if len(entries) >= maxEntries {
			return 0, nil, errors.New("the envelope has too many files")
		}
		line, cursor, err = lineAt(envelope, cursor)
		if err != nil {
			return 0, nil, err
		}
		parts := strings.SplitN(line, "  ", 3)
		if len(parts) != 3 {
			return 0, nil, errors.New("an envelope manifest line is malformed")
		}
		hash, sizeText, name := parts[0], parts[1], parts[2]
		if !validHash(hash) {
			return 0, nil, fmt.Errorf("%s's checksum in the envelope is malformed", name)
		}
		if !validName(name) {
			return 0, nil, fmt.Errorf("%s is not a payload file name", rustDebug(name))
		}
		for _, entry := range entries {
			if entry.Name == name {
				return 0, nil, fmt.Errorf("the envelope names %s twice", name)
			}
		}
		size, ok := parseU64(sizeText)
		if !ok {
			return 0, nil, fmt.Errorf("%s's size in the envelope is not a number", name)
		}
		var carry uint64
		sizes, carry = bits.Add64(sizes, size, 0)
		if carry != 0 {
			return 0, nil, errors.New("the envelope's sizes overflow")
		}
		expected, carry := bits.Add64(uint64(cursor-body), sizes, 0)
		if carry != 0 {
			return 0, nil, errors.New("the envelope's length overflows")
		}
		if expected > length {
			return 0, nil, errors.New("the envelope's manifest is longer than its length")
		}
		entries = append(entries, Entry{Name: name, SHA256: strings.ToLower(hash), Size: size})
		if expected == length {
			return cursor, entries, nil
		}
	}
}

// Encode encodes the signed envelope as a delta.
//
// installed maps each file the mixer has to its SHA-256. bases maps a file
// name to bytes the app holds; they are used as a patch prefix only when they
// still hash to what the mixer reports, otherwise the app's copy is stale and
// the file is sent whole. level is a zstd level (1-22); the app uses 19.
func Encode(envelope []byte, installed map[string]string, bases map[string][]byte, level int) ([]byte, error) {
	head, entries, err := EnvelopeEntries(envelope)
	if err != nil {
		return nil, err
	}
	sizes, err := sumSizes(entries)
	if err != nil {
		return nil, err
	}
	if total, carry := bits.Add64(uint64(head), sizes, 0); carry != 0 || total != uint64(len(envelope)) {
		return nil, errors.New("the envelope's files do not fill its declared length")
	}
	out := []byte(Magic)
	out = fmt.Appendf(out, "head %d\n", head)
	out = append(out, envelope[:head]...)

	offset := head
	for _, entry := range entries {
		if entry.Size > uint64(math.MaxInt) {
			return nil, fmt.Errorf("%s is too large for this app", entry.Name)
		}
		size := int(entry.Size)
		if offset+size > len(envelope) {
			return nil, fmt.Errorf("%s is truncated in the envelope", entry.Name)
		}
		data := envelope[offset : offset+size]
		offset += size
		current, known := installed[entry.Name]
		if known && strings.EqualFold(current, entry.SHA256) {
			out = append(out, "copy\n"...)
			continue
		}
		base, haveBase := bases[entry.Name]
		if haveBase && known && strings.EqualFold(current, sum(base)) {
			hash := sum(base)
			window := windowLog(uint64(len(base)) + uint64(size))
			compressed, err := compress(data, base, level, window)
			if err != nil {
				return nil, err
			}
			out = fmt.Appendf(out, "patch %s %d\n", hash, len(compressed))
			out = append(out, compressed...)
		} else {
			window := windowLog(uint64(size))
			compressed, err := compress(data, nil, level, window)
			if err != nil {
				return nil, err
			}
			out = fmt.Appendf(out, "zstd %d\n", len(compressed))
			out = append(out, compressed...)
		}
	}
	return out, nil
}

// compress writes one zstd frame the mixer's decoder accepts: no dictionary
// ID (the C decoder refuses a frame naming a dictionary when it only holds a
// raw prefix), a window no larger than 2^window (the C decoder's bound is
// 2^25), the content size in the header, no checksum (the manifest's SHA-256
// covers the bytes), and a real frame even for an empty file (the delta
// refuses a zero compressed length).
//
// With a prefix, the frame is a patch against it, like `zstd --patch-from`:
// the prefix is history before the frame's first byte, so every match into
// it is an offset within the window. The window covers the prefix and the
// new file, because the C streaming decoder keeps the prefix reachable only
// until its own window buffer wraps.
func compress(data, prefix []byte, level, window int) ([]byte, error) {
	options := []zstd.EOption{
		zstd.WithEncoderLevel(zstd.EncoderLevelFromZstd(level)),
		zstd.WithWindowSize(1 << window),
		zstd.WithEncoderConcurrency(1),
		zstd.WithEncoderCRC(false),
		zstd.WithZeroFrames(true),
		// The C encoder's rule (ZSTD_writeFrameHeader): a single segment
		// whenever the window covers the content, so the frame header has
		// the shape the Rust encoder writes.
		zstd.WithSingleSegment(len(data) <= 1<<window),
	}
	// An empty prefix is no prefix, for the C decoder as here.
	if len(prefix) > 0 {
		options = append(options, zstd.WithEncoderDictRaw(0, prefix))
	}
	encoder, err := zstd.NewWriter(nil, options...)
	if err != nil {
		return nil, zstdError(err)
	}
	defer encoder.Close()
	return encoder.EncodeAll(data, make([]byte, 0, len(data)/2+64)), nil
}

// windowLog is the window a patch needs to reach from the start of the base
// to the end of the new file. Plain compression only has to reach its own
// start.
func windowLog(n uint64) int {
	needed := 0
	if n > 1 {
		needed = 64 - bits.LeadingZeros64(n-1)
	}
	return min(max(needed, 20), windowLogMax)
}

func zstdError(err error) error {
	return fmt.Errorf("zstd error: %v", err)
}

func sumSizes(entries []Entry) (uint64, error) {
	var total uint64
	for _, entry := range entries {
		var carry uint64
		total, carry = bits.Add64(total, entry.Size, 0)
		if carry != 0 {
			return 0, errors.New("the envelope's sizes overflow")
		}
	}
	return total, nil
}

func lineAt(b []byte, start int) (string, int, error) {
	if start >= len(b) {
		return "", 0, errors.New("the envelope header is truncated")
	}
	end := min(start+maxLine, len(b))
	for index := start; index < end; index++ {
		if b[index] == '\n' {
			for _, c := range b[start:index] {
				if c >= 0x80 {
					return "", 0, errors.New("the envelope header is not text")
				}
			}
			return string(b[start:index]), index + 1, nil
		}
	}
	return "", 0, errors.New("an envelope header line is too long")
}

// parseU64 parses as Rust's u64 FromStr does: an optional leading '+', then
// one or more ASCII digits, no overflow.
func parseU64(text string) (uint64, bool) {
	digits := strings.TrimPrefix(text, "+")
	if digits == "" {
		return 0, false
	}
	for i := 0; i < len(digits); i++ {
		if digits[i] < '0' || digits[i] > '9' {
			return 0, false
		}
	}
	value, err := strconv.ParseUint(digits, 10, 64)
	return value, err == nil
}

func saturatingAdd(a, b uint64) uint64 {
	if sum, carry := bits.Add64(a, b, 0); carry == 0 {
		return sum
	}
	return math.MaxUint64
}

func validName(name string) bool {
	return name != "" && name != "." && name != ".." && !strings.ContainsAny(name, "/\\\x00")
}

func validHash(text string) bool {
	if len(text) != 64 {
		return false
	}
	for i := 0; i < len(text); i++ {
		c := text[i]
		if !('0' <= c && c <= '9' || 'a' <= c && c <= 'f' || 'A' <= c && c <= 'F') {
			return false
		}
	}
	return true
}

func sum(data []byte) string {
	digest := sha256.Sum256(data)
	return hex.EncodeToString(digest[:])
}

// rustDebug quotes s as Rust's `{:?}` does for ASCII text, which is all an
// envelope header may hold.
func rustDebug(s string) string {
	var b strings.Builder
	b.WriteByte('"')
	for _, r := range s {
		switch r {
		case '"':
			b.WriteString(`\"`)
		case '\\':
			b.WriteString(`\\`)
		case '\t':
			b.WriteString(`\t`)
		case '\r':
			b.WriteString(`\r`)
		case '\n':
			b.WriteString(`\n`)
		case 0:
			b.WriteString(`\0`)
		default:
			if r < 0x20 || r == 0x7f {
				fmt.Fprintf(&b, `\u{%x}`, r)
			} else {
				b.WriteRune(r)
			}
		}
	}
	b.WriteByte('"')
	return b.String()
}
