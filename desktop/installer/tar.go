package installer

// A tar reader and writer that keep every 512-byte header as it is, as the
// Rust `tar` crate's raw mode does: GNU long-name and PAX records can be
// copied verbatim, which archive/tar cannot do.

import (
	"bytes"
	"compress/gzip"
	"errors"
	"fmt"
	"io"
	"strconv"
	"strings"
)

const block = 512

const (
	typeRegular  = '0'
	typeOldFile  = 0
	typeSymlink  = '2'
	typeDir      = '5'
	typeLongName = 'L'
	typeLongLink = 'K'
	typePaxLocal = 'x'
	typeSparse   = 'S'
)

type tarHeader [block]byte

func (h *tarHeader) entryType() byte { return h[156] }

// regular is an ordinary file, '0' or the old NUL type.
func (h *tarHeader) regular() bool {
	return h.entryType() == typeRegular || h.entryType() == typeOldFile
}

func (h *tarHeader) ustar() bool {
	return bytes.Equal(h[257:263], []byte("ustar\x00")) && bytes.Equal(h[263:265], []byte("00"))
}

func truncateNUL(data []byte) []byte {
	if i := bytes.IndexByte(data, 0); i >= 0 {
		return data[:i]
	}
	return data
}

// pathBytes is the header's own name: a ustar prefix joined to the name, or
// the name alone.
func (h *tarHeader) pathBytes() []byte {
	name := truncateNUL(h[0:100])
	if h.ustar() {
		if prefix := truncateNUL(h[345:500]); len(prefix) > 0 {
			return append(append(append([]byte{}, prefix...), '/'), name...)
		}
	}
	return append([]byte{}, name...)
}

// numeric reads an octal field, or a GNU base-256 one.
func numeric(field []byte) (uint64, error) {
	if len(field) > 0 && field[0]&0x80 != 0 {
		var v uint64
		for i, b := range field {
			if i == 0 {
				b &= 0x7f
			}
			if v>>56 != 0 {
				return 0, errors.New("numeric field too large")
			}
			v = v<<8 | uint64(b)
		}
		return v, nil
	}
	text := strings.Trim(string(truncateNUL(field)), " ")
	v, err := strconv.ParseUint(text, 8, 64)
	if err != nil {
		return 0, fmt.Errorf("numeric field was not a number: %s", text)
	}
	return v, nil
}

func (h *tarHeader) size() (uint64, error) { return numeric(h[124:136]) }

func (h *tarHeader) checksumOK() error {
	var sum uint64
	for i, b := range h {
		if i >= 148 && i < 156 {
			b = ' '
		}
		sum += uint64(b)
	}
	want, err := numeric(h[148:156])
	if err != nil {
		return err
	}
	if sum != want {
		return errors.New("archive header checksum mismatch")
	}
	return nil
}

// tarEntry is one member: its header, its name and a reader of its data.
type tarEntry struct {
	header tarHeader
	// path is the member's name; in raw mode the header's own.
	path string
	size uint64
	data io.Reader
}

func (e *tarEntry) Read(buf []byte) (int, error) { return e.data.Read(buf) }

func readAll(e *tarEntry) ([]byte, error) { return io.ReadAll(e.data) }

// walkArchive reads a gzip-compressed tar archive, enforcing an expansion
// limit and the gzip trailer checks. raw presents GNU long-name and PAX
// records as their own entries so they can be copied verbatim.
func walkArchive(path string, limit uint64, raw bool, visit func(*tarEntry) error) error {
	file, err := openRegular(path)
	if err != nil {
		return err
	}
	defer file.Close()
	return walkReader(file, limit, raw, visit)
}

func walkReader(source io.Reader, limit uint64, raw bool, visit func(*tarEntry) error) error {
	gz, err := gzip.NewReader(source)
	if err != nil {
		return err
	}
	gz.Multistream(true)
	limited := &io.LimitedReader{R: gz, N: int64(limit) + 1}
	if err := walkTar(limited, raw, visit); err != nil {
		return err
	}
	// Read through the trailer, enforcing gzip CRC and the expansion limit.
	if _, err := io.Copy(io.Discard, limited); err != nil {
		return err
	}
	if limited.N == 0 {
		return Error("archive expansion limit exceeded")
	}
	return nil
}

// readBlock fills one header block; false at a clean end of input.
func readBlock(r io.Reader, h *tarHeader) (bool, error) {
	n, err := io.ReadFull(r, h[:])
	if n == 0 && (err == io.EOF || err == io.ErrUnexpectedEOF) {
		return false, nil
	}
	if err != nil {
		return false, errors.New("failed to read entire block")
	}
	return true, nil
}

func walkTar(r io.Reader, raw bool, visit func(*tarEntry) error) error {
	var longName, longLink []byte
	var pax map[string][]byte
	for {
		var h tarHeader
		ok, err := readBlock(r, &h)
		if err != nil {
			return err
		}
		// The end of the archive, or an all-zero block that marks it.
		if !ok || h == (tarHeader{}) {
			return nil
		}
		if err := h.checksumOK(); err != nil {
			return err
		}
		size, err := h.size()
		if err != nil {
			return err
		}
		if h.entryType() == typeSparse {
			return Error("sparse archive members are not supported")
		}
		padded := (size + block - 1) / block * block
		if !raw {
			switch h.entryType() {
			case typeLongName, typeLongLink, typePaxLocal:
				if size > maxSmallMember {
					return Error("oversized firmware metadata")
				}
				data := make([]byte, padded)
				if _, err := io.ReadFull(r, data); err != nil {
					return err
				}
				data = data[:size]
				switch h.entryType() {
				case typeLongName:
					longName = data
				case typeLongLink:
					longLink = data
				default:
					records, err := paxRecords(data)
					if err != nil {
						return err
					}
					pax = records
				}
				continue
			}
		}
		path := h.pathBytes()
		if !raw {
			if longName != nil {
				path = bytes.TrimSuffix(longName, []byte{0})
			} else if p, ok := pax["path"]; ok {
				path = p
			}
			if s, ok := pax["size"]; ok {
				if v, err := strconv.ParseUint(string(s), 10, 64); err == nil {
					size = v
					padded = (size + block - 1) / block * block
				}
			}
			longName, longLink, pax = nil, nil, nil
			_ = longLink
		}
		entry := &tarEntry{
			header: h,
			path:   strings.ToValidUTF8(string(path), "�"),
			size:   size,
			data:   io.LimitReader(r, int64(size)),
		}
		if err := visit(entry); err != nil {
			return err
		}
		// Skip what the visitor left, and the padding.
		if _, err := io.Copy(io.Discard, entry.data); err != nil {
			return err
		}
		if _, err := io.CopyN(io.Discard, r, int64(padded-size)); err != nil {
			return errors.New("failed to read entire block")
		}
	}
}

// tarWriter writes headers verbatim and data padded to a block.
type tarWriter struct {
	w io.Writer
}

// append writes a header as it is, then data padded to a whole block.
func (t *tarWriter) append(h *tarHeader, data io.Reader) error {
	if _, err := t.w.Write(h[:]); err != nil {
		return err
	}
	n, err := io.Copy(t.w, data)
	if err != nil {
		return err
	}
	if pad := (block - n%block) % block; pad > 0 {
		if _, err := t.w.Write(make([]byte, pad)); err != nil {
			return err
		}
	}
	return nil
}

// finish writes the two zero blocks that end an archive.
func (t *tarWriter) finish() error {
	_, err := t.w.Write(make([]byte, 2*block))
	return err
}

// octal writes v right-aligned with leading zeros, leaving the field's last
// byte as it is (NUL in a new header).
func octal(field []byte, v uint64) {
	text := strconv.FormatUint(v, 8)
	for i := len(field) - 2; i >= 0; i-- {
		if len(text) > 0 {
			field[i] = text[len(text)-1]
			text = text[:len(text)-1]
		} else {
			field[i] = '0'
		}
	}
}

// archivePath is the name Rust's tar builder stores for path: leading `./`
// and empty or `.` components dropped, a trailing slash kept.
func archivePath(path string) (string, error) {
	var parts []string
	for _, part := range strings.Split(path, "/") {
		switch part {
		case "", ".":
			continue
		case "..":
			return "", errors.New("paths in archives must not have `..`")
		}
		parts = append(parts, part)
	}
	if strings.HasPrefix(path, "/") {
		return "", errors.New("paths in archives must be relative")
	}
	if len(parts) == 0 {
		return "", errors.New("paths in archives must have at least one component")
	}
	out := strings.Join(parts, "/")
	if strings.HasSuffix(path, "/") {
		out += "/"
	}
	return out, nil
}

// gnuHeader is a new root-owned GNU header for one added entry.
func gnuHeader(path string, kind byte, mode uint32, size uint64, mtime uint64) (*tarHeader, error) {
	name, err := archivePath(path)
	if err != nil {
		return nil, err
	}
	if len(name) > 100 {
		return nil, fmt.Errorf("path too long for an archive header: %s", name)
	}
	var h tarHeader
	copy(h[0:100], name)
	octal(h[100:108], uint64(mode))
	octal(h[108:116], 0)
	octal(h[116:124], 0)
	octal(h[124:136], size)
	octal(h[136:148], mtime)
	h[156] = kind
	copy(h[257:263], "ustar ")
	copy(h[263:265], " \x00")
	for i := 148; i < 156; i++ {
		h[i] = ' '
	}
	var total uint64
	for _, b := range h {
		total += uint64(b)
	}
	for i := 148; i < 156; i++ {
		h[i] = 0
	}
	octal(h[148:156], total)
	return &h, nil
}

// paxRecords parses PAX extended-header records ("<len> <key>=<value>\n").
func paxRecords(data []byte) (map[string][]byte, error) {
	records := map[string][]byte{}
	rest := data
	for len(rest) > 0 {
		space := bytes.IndexByte(rest, ' ')
		if space < 0 {
			return nil, Error("invalid PAX record")
		}
		length, err := parseUsizeText(string(rest[:space]))
		if err != nil {
			return nil, Error("invalid PAX record")
		}
		if length <= space+1 || length > len(rest) || rest[length-1] != '\n' {
			return nil, Error("invalid PAX record")
		}
		record := rest[space+1 : length-1]
		eq := bytes.IndexByte(record, '=')
		if eq < 0 {
			return nil, Error("invalid PAX record")
		}
		records[strings.ToValidUTF8(string(record[:eq]), "�")] = append([]byte{}, record[eq+1:]...)
		rest = rest[length:]
	}
	return records, nil
}

// parseUsizeText is Rust's usize::from_str.
func parseUsizeText(text string) (int, error) {
	digits := strings.TrimPrefix(text, "+")
	if digits == "" || strings.Trim(digits, "0123456789") != "" {
		return 0, errors.New("invalid digit")
	}
	n, err := strconv.ParseUint(digits, 10, 63)
	return int(n), err
}
