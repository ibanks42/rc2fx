// Package delta implements delta encoding for in-place mixer payload uploads.
//
// The signed RC2FX-EMBEDDED-PAYLOAD 1 envelope remains the source of truth;
// delta encoding just reduces transfer size over USB. Each file is sent whole
// (zstd), skipped (copy), or patched against the mixer's installed copy via
// zstd raw prefix matching. On the mixer, decoding reconstructs the exact
// envelope bytes and verifies every file against the manifest before normal
// signature verification runs.
//
// Port of crates/rc2fx-delta. The mixer runs the Rust decoder (libzstd with
// --patch-from semantics and a 2^25 window cap); live Go-to-Rust wire
// compatibility is verified directly by crates/rc2fx-delta/tests/shared_fixtures.rs.
package delta

import (
	"errors"
	"fmt"
	"math"
	"math/bits"
	"strings"

	"github.com/klauspost/compress/zstd"

	"github.com/ibanks42/rc2fx/desktop/internal/common"
)

const (
	Magic         = "RC2FX-DELTA 1\n"
	envelopeMagic = "RC2FX-EMBEDDED-PAYLOAD 1\n"
	maxLine       = 512
	maxHead       = 64 * 1024 // envelope header + manifest only
	maxEntries    = 256
	// Cap decoder window at 32 MiB to bound memory allocation on untrusted input.
	windowLogMax = 25
	// Allow compressed frames to slightly exceed the uncompressed envelope size.
	opOverhead = 1 << 20
)

// Entry describes one file in an envelope manifest.
type Entry struct {
	Name   string
	SHA256 string
	Size   uint64
}

// EnvelopeEntries parses an envelope's header and manifest, returning the byte
// length of the head and its entries. Because the manifest has no explicit
// entry count, parsing stops once the manifest bytes plus summed file sizes
// reach the declared body length.
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
	length, ok := common.ParseUint(text, 64)
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
		// An empty payload is still valid.
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
			return 0, nil, fmt.Errorf("%q is not a payload file name", name)
		}
		for _, entry := range entries {
			if entry.Name == name {
				return 0, nil, fmt.Errorf("the envelope names %s twice", name)
			}
		}
		size, ok := common.ParseUint(sizeText, 64)
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

// Encode builds a delta stream for envelope.
//
// installed maps installed filenames on the mixer to their SHA-256 hex digests.
// bases holds local copies the app can patch against; a base is only used if
// its SHA-256 matches installed[name], otherwise the file is sent whole. level
// is a zstd compression level (1-22; the app uses 19).
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
		if haveBase && known && strings.EqualFold(current, common.SHA256Hex(base)) {
			hash := common.SHA256Hex(base)
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

// compress emits a single zstd frame compatible with the mixer's C decoder: no
// dictionary ID (libzstd rejects a dict ID when given a raw prefix), window
// bounded to 2^window (<= 2^25), no content checksum (covered by the manifest
// SHA-256), and a valid frame even for empty inputs.
//
// When prefix is non-empty, it acts as raw history before the frame (matching
// `zstd --patch-from`). The window must span both prefix and data so the C
// streaming decoder doesn't evict the prefix before finishing.
func compress(data, prefix []byte, level, window int) ([]byte, error) {
	options := []zstd.EOption{
		zstd.WithEncoderLevel(zstd.EncoderLevelFromZstd(level)),
		zstd.WithWindowSize(1 << window),
		zstd.WithEncoderConcurrency(1),
		zstd.WithEncoderCRC(false),
		zstd.WithZeroFrames(true),
		// Match libzstd's ZSTD_writeFrameHeader rule for single-segment frames.
		zstd.WithSingleSegment(len(data) <= 1<<window),
	}
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

// windowLog returns the zstd window log needed to span n bytes (clamped to
// [20, windowLogMax]).
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
	return len(text) == 64 && common.IsHex(text)
}
