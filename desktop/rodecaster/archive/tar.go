// Package archive reads, writes and patches RØDE firmware archives
// (gzip-compressed tar), and holds the private-file helpers that go with
// them.
package archive

// A tar reader and writer that keep every 512-byte header as it is: GNU
// long-name and PAX records can be copied verbatim, which archive/tar
// cannot do.

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
	TypeRegular  = '0'
	TypeOldFile  = 0
	TypeSymlink  = '2'
	TypeDir      = '5'
	TypeLongName = 'L'
	TypeLongLink = 'K'
	TypePaxLocal = 'x'
	TypeSparse   = 'S'
)

type Header [block]byte

func (h *Header) EntryType() byte { return h[156] }

// regular is an ordinary file, '0' or the old NUL type.
func (h *Header) Regular() bool {
	return h.EntryType() == TypeRegular || h.EntryType() == TypeOldFile
}

func (h *Header) ustar() bool {
	return bytes.Equal(h[257:263], []byte("ustar\x00")) && bytes.Equal(h[263:265], []byte("00"))
}

// LinkName is a symlink header's target, as stored.
func (h *Header) LinkName() []byte { return truncateNUL(h[157:257]) }

func truncateNUL(data []byte) []byte {
	if i := bytes.IndexByte(data, 0); i >= 0 {
		return data[:i]
	}
	return data
}

// pathBytes is the header's own name: a ustar prefix joined to the name, or
// the name alone.
func (h *Header) pathBytes() []byte {
	name := truncateNUL(h[0:100])
	if h.ustar() {
		if prefix := truncateNUL(h[345:500]); len(prefix) > 0 {
			return append(append(append([]byte{}, prefix...), '/'), name...)
		}
	}
	return append([]byte{}, name...)
}

// Numeric reads an octal field, or a GNU base-256 one.
func Numeric(field []byte) (uint64, error) {
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

func (h *Header) size() (uint64, error) { return Numeric(h[124:136]) }

func (h *Header) checksumOK() error {
	var sum uint64
	for i, b := range h {
		if i >= 148 && i < 156 {
			b = ' '
		}
		sum += uint64(b)
	}
	want, err := Numeric(h[148:156])
	if err != nil {
		return err
	}
	if sum != want {
		return errors.New("archive header checksum mismatch")
	}
	return nil
}

// Member is one member: its header, its name and a reader of its data.
type Member struct {
	Header Header
	// Path is the member's name; in raw mode the header's own.
	Path string
	Size uint64
	data io.Reader
}

func (e *Member) Read(buf []byte) (int, error) { return e.data.Read(buf) }

func ReadAll(e *Member) ([]byte, error) { return io.ReadAll(e.data) }

// Walk reads a gzip-compressed tar archive, enforcing an expansion
// limit and the gzip trailer checks. raw presents GNU long-name and PAX
// records as their own entries so they can be copied verbatim.
func Walk(path string, limit uint64, raw bool, visit func(*Member) error) error {
	file, err := OpenRegular(path)
	if err != nil {
		return err
	}
	defer file.Close()
	return WalkReader(file, limit, raw, visit)
}

func WalkReader(source io.Reader, limit uint64, raw bool, visit func(*Member) error) error {
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
		return errors.New("archive expansion limit exceeded")
	}
	return nil
}

// readBlock fills one header block; false at a clean end of input.
func readBlock(r io.Reader, h *Header) (bool, error) {
	n, err := io.ReadFull(r, h[:])
	if n == 0 && (err == io.EOF || err == io.ErrUnexpectedEOF) {
		return false, nil
	}
	if err != nil {
		return false, errors.New("failed to read entire block")
	}
	return true, nil
}

func walkTar(r io.Reader, raw bool, visit func(*Member) error) error {
	var longName []byte
	var pax map[string][]byte
	for {
		var h Header
		ok, err := readBlock(r, &h)
		if err != nil {
			return err
		}
		// The end of the archive, or an all-zero block that marks it.
		if !ok || h == (Header{}) {
			return nil
		}
		if err := h.checksumOK(); err != nil {
			return err
		}
		size, err := h.size()
		if err != nil {
			return err
		}
		if h.EntryType() == TypeSparse {
			return errors.New("sparse archive members are not supported")
		}
		padded := (size + block - 1) / block * block
		if !raw {
			switch h.EntryType() {
			case TypeLongName, TypeLongLink, TypePaxLocal:
				if size > MaxSmallMember {
					return errors.New("oversized firmware metadata")
				}
				data := make([]byte, padded)
				if _, err := io.ReadFull(r, data); err != nil {
					return err
				}
				data = data[:size]
				switch h.EntryType() {
				case TypeLongName:
					longName = data
				case TypeLongLink:
					// The link target is not used; the record is read
					// and skipped.
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
			longName, pax = nil, nil
		}
		entry := &Member{
			Header: h,
			Path:   strings.ToValidUTF8(string(path), "�"),
			Size:   size,
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

// Writer writes headers verbatim and data padded to a block.
type Writer struct {
	W io.Writer
}

// Append writes a header as it is, then data padded to a whole block.
func (t *Writer) Append(h *Header, data io.Reader) error {
	if _, err := t.W.Write(h[:]); err != nil {
		return err
	}
	n, err := io.Copy(t.W, data)
	if err != nil {
		return err
	}
	if pad := (block - n%block) % block; pad > 0 {
		if _, err := t.W.Write(make([]byte, pad)); err != nil {
			return err
		}
	}
	return nil
}

// Finish writes the two zero blocks that end an archive.
func (t *Writer) Finish() error {
	_, err := t.W.Write(make([]byte, 2*block))
	return err
}

// Octal writes v right-aligned with leading zeros, leaving the field's last
// byte as it is (NUL in a new header).
func Octal(field []byte, v uint64) {
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

// ArchivePath is the name stored for path: leading `./`
// and empty or `.` components dropped, a trailing slash kept.
func ArchivePath(path string) (string, error) {
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

// gnuHeader is a new root-owned GNU header for one added entry; link is a
// symlink's target, empty for anything else.
func gnuHeader(path string, kind byte, mode uint32, size uint64, mtime uint64, link string) (*Header, error) {
	name, err := ArchivePath(path)
	if err != nil {
		return nil, err
	}
	if len(name) > 100 {
		return nil, fmt.Errorf("path too long for an archive header: %s", name)
	}
	if len(link) > 100 || strings.IndexByte(link, 0) >= 0 {
		return nil, fmt.Errorf("link target does not fit an archive header: %s", name)
	}
	var h Header
	copy(h[0:100], name)
	copy(h[157:257], link)
	Octal(h[100:108], uint64(mode))
	Octal(h[108:116], 0)
	Octal(h[116:124], 0)
	Octal(h[124:136], size)
	Octal(h[136:148], mtime)
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
	Octal(h[148:156], total)
	return &h, nil
}

// paxRecords parses PAX extended-header records ("<len> <key>=<value>\n").
func paxRecords(data []byte) (map[string][]byte, error) {
	records := map[string][]byte{}
	rest := data
	for len(rest) > 0 {
		space := bytes.IndexByte(rest, ' ')
		if space < 0 {
			return nil, errors.New("invalid PAX record")
		}
		length, err := parseUsizeText(string(rest[:space]))
		if err != nil {
			return nil, errors.New("invalid PAX record")
		}
		if length <= space+1 || length > len(rest) || rest[length-1] != '\n' {
			return nil, errors.New("invalid PAX record")
		}
		record := rest[space+1 : length-1]
		eq := bytes.IndexByte(record, '=')
		if eq < 0 {
			return nil, errors.New("invalid PAX record")
		}
		records[strings.ToValidUTF8(string(record[:eq]), "�")] = append([]byte{}, record[eq+1:]...)
		rest = rest[length:]
	}
	return records, nil
}

// parseUsizeText is an optional '+', then digits only, below 2^63.
func parseUsizeText(text string) (int, error) {
	digits := strings.TrimPrefix(text, "+")
	if digits == "" || strings.Trim(digits, "0123456789") != "" {
		return 0, errors.New("invalid digit")
	}
	n, err := strconv.ParseUint(digits, 10, 63)
	return int(n), err
}
