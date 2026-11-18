package archive

// The local patch of a RØDE firmware archive: a verbatim copy with entries
// added.

import (
	"bytes"
	"compress/gzip"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"
)

// EntryKind is a directory, a file or a symbolic link.
type EntryKind int

const (
	EntryDir EntryKind = iota
	EntryFile
	EntryLink
)

// Entry is a file, directory or symbolic link to add to the firmware image.
type Entry struct {
	Kind EntryKind
	Path string
	Mode uint32
	Data []byte
	// Target is a link's target, as stored; only for EntryLink.
	Target string
}

// Normalized is a member name without a leading `./` or a trailing slash.
func Normalized(name string) string {
	name = strings.TrimPrefix(name, "./")
	return strings.TrimSuffix(name, "/")
}

const (
	// MaxMixerMember is the largest mixer executable read into memory.
	MaxMixerMember = 64 << 20
	// MaxSmallMember is the largest launcher, configuration file or tar
	// metadata record.
	MaxSmallMember = 1 << 20
)

// RØDE's members that identify the firmware: the board's hostname, and the
// mixer executable and its launcher, whose hashes Patch returns.
const (
	hostnameMember = "etc/hostname"
	mixerMember    = "usr/bin/rc_audio_mixer"
	launcherMember = "usr/bin/audio-start.sh"
)

// Patched is what Patch built.
type Patched struct {
	// Mixer and Launcher are the SHA-256 of the mixer executable and its
	// launcher.
	Mixer, Launcher string
	// ArchiveSHA and ArchiveMD5 are the SHA-256 and MD5 of the bytes
	// written. They come from the write itself, so another process changing
	// the cache folder cannot alter them.
	ArchiveSHA, ArchiveMD5 string
}

// Copied is one member Patch copied, as PatchOptions.Member sees it.
type Copied struct {
	// Name is the member's normalized name.
	Name   string
	Header Header
	Size   uint64
	// Data is the member's data when Patch read it whole (etc/hostname and
	// the PatchOptions.Read members), else nil.
	Data []byte
	// SHA256 is the hex SHA-256 of the member's data.
	SHA256 string
}

// PatchOptions is what a caller adds to Patch.
type PatchOptions struct {
	Model model.Model
	// Extra is added after the source's members, under `opt`.
	Extra []Entry
	// SourceSHA, when set, is the SHA-256 the bytes read from the source
	// must have.
	SourceSHA *string
	// Read names members, besides RØDE's own, that must each appear once as
	// a regular member and are read whole (at most MaxSmallMember) and handed
	// to Member with their data.
	Read []string
	// Member sees every member except tar metadata records, in order. An
	// error stops the patch.
	Member func(Copied) error
	// Finish runs when there are extras, after they are written; add
	// appends one more entry.
	Finish func(add func(Entry) error) error
	// Verify runs once the archive is written; false means the source is not
	// the expected rootfs.
	Verify func() bool
}

// Patch writes a copy of src to dst with o.Extra added. Every record,
// including long-name and PAX records, is copied verbatim. With
// o.SourceSHA, the bytes actually read from src must have that SHA-256: a
// check of the path before this call says nothing about what is read here.
func Patch(src, dst string, o PatchOptions) (*Patched, error) {
	extra := o.Extra
	important := append([]string{hostnameMember, mixerMember, launcherMember}, o.Read...)
	t, err := NewTemp(filepath.Dir(dst), ".firmware-")
	if err != nil {
		return nil, err
	}
	defer t.Cleanup()
	written := HashingWriter(t)
	gz, err := gzip.NewWriterLevel(written, gzip.DefaultCompression)
	if err != nil {
		return nil, err
	}
	builder := &Writer{W: gz}
	srcFile, err := OpenWorkFile(src)
	if err != nil {
		return nil, err
	}
	defer srcFile.Close()
	source := HashingReader(srcFile)
	seen := map[string]bool{}
	hashes := map[string]string{}
	hostOK := false
	// Names supplied by preceding GNU long-name or PAX records.
	var longName, paxName *string
	paxSize, hasOpt := false, false
	member := func(c Copied) error {
		if o.Member == nil {
			return nil
		}
		return o.Member(c)
	}
	err = WalkReader(source, 4<<30, true, func(entry *Member) error {
		header := entry.Header
		kind := header.EntryType()
		if kind == TypeLongName || kind == TypePaxLocal {
			if entry.Size > MaxSmallMember {
				return errors.New("oversized firmware metadata")
			}
			data, err := ReadAll(entry)
			if err != nil {
				return err
			}
			if kind == TypeLongName {
				name := strings.ToValidUTF8(string(truncateNUL(data)), "�")
				longName = &name
			} else {
				records, err := paxRecords(data)
				if err != nil {
					return err
				}
				_, paxSize = records["size"]
				paxName = nil
				if p, ok := records["path"]; ok {
					name := strings.ToValidUTF8(string(p), "�")
					paxName = &name
				}
			}
			return builder.Append(&header, bytes.NewReader(data))
		}
		var name string
		switch {
		case paxName != nil:
			name, paxName = *paxName, nil
		case longName != nil:
			name, longName = *longName, nil
		default:
			name = entry.Path
		}
		sizedByPax := paxSize
		paxSize = false
		n := Normalized(name)
		if slices.ContainsFunc(extra, func(e Entry) bool { return e.Path == n }) {
			return fmt.Errorf("the firmware already contains %s", n)
		}
		if n == "opt" {
			hasOpt = true
		}
		if slices.Contains(important, n) {
			if seen[n] || !header.Regular() || sizedByPax {
				return fmt.Errorf("duplicate or nonregular required member %s", n)
			}
			seen[n] = true
		}
		if n == hostnameMember || slices.Contains(o.Read, n) {
			if entry.Size > MaxSmallMember {
				return errors.New("oversized firmware metadata")
			}
			data, err := ReadAll(entry)
			if err != nil {
				return err
			}
			if n == hostnameMember {
				hostOK = string(TrimASCII(data)) == o.Model.Hostname()
			}
			if err := member(Copied{Name: n, Header: header, Size: entry.Size, Data: data, SHA256: Sum(data)}); err != nil {
				return err
			}
			return builder.Append(&header, bytes.NewReader(data))
		}
		if n == mixerMember || n == launcherMember {
			limit := uint64(MaxSmallMember)
			if n == mixerMember {
				limit = MaxMixerMember
			}
			if entry.Size > limit {
				return fmt.Errorf("oversized firmware member %s", n)
			}
			data, err := ReadAll(entry)
			if err != nil {
				return err
			}
			digest := Sum(data)
			if err := member(Copied{Name: n, Header: header, Size: entry.Size, SHA256: digest}); err != nil {
				return err
			}
			hashes[n] = digest
			return builder.Append(&header, bytes.NewReader(data))
		}
		hashed := HashingReader(entry)
		if err := builder.Append(&header, hashed); err != nil {
			return err
		}
		digest, _ := hashed.Finish()
		return member(Copied{Name: n, Header: header, Size: entry.Size, SHA256: digest})
	})
	if err != nil {
		return nil, err
	}
	if len(extra) > 0 && !hasOpt {
		if err := appendEntry(builder, Entry{Kind: EntryDir, Path: "opt", Mode: 0o755}); err != nil {
			return nil, err
		}
	}
	for _, entry := range extra {
		if err := appendEntry(builder, entry); err != nil {
			return nil, err
		}
	}
	if len(extra) > 0 && o.Finish != nil {
		if err := o.Finish(func(e Entry) error { return appendEntry(builder, e) }); err != nil {
			return nil, err
		}
	}
	if _, err := io.Copy(io.Discard, source); err != nil {
		return nil, err
	}
	if o.SourceSHA != nil {
		if got, _ := source.Finish(); got != *o.SourceSHA {
			return nil, errors.New("the downloaded firmware changed while it was being patched")
		}
	}
	if err := builder.Finish(); err != nil {
		return nil, err
	}
	if err := gz.Close(); err != nil {
		return nil, err
	}
	archiveSha, archiveMd5 := written.Finish()
	if err := t.Finish(); err != nil {
		return nil, err
	}
	if !hostOK || len(hashes) != 2 || (o.Verify != nil && !o.Verify()) {
		return nil, fmt.Errorf("firmware is not the expected %s rootfs", o.Model.Label())
	}
	info, err := os.Stat(t.Path())
	if err != nil {
		return nil, err
	}
	if info.Size() > 275<<20 {
		return nil, errors.New("patched image exceeds update volume limit")
	}
	if err := t.Persist(dst); err != nil {
		return nil, err
	}
	return &Patched{
		Mixer:      hashes[mixerMember],
		Launcher:   hashes[launcherMember],
		ArchiveSHA: archiveSha,
		ArchiveMD5: archiveMd5,
	}, nil
}

// TrimASCII trims ASCII whitespace from both ends.
func TrimASCII(data []byte) []byte {
	return bytes.Trim(data, " \t\n\r\f")
}

// AppendEntry appends one root-owned entry in the image's `./path` style.
func AppendEntry(builder *Writer, entry Entry) error {
	return appendEntry(builder, entry)
}

// appendEntry appends one root-owned entry in the image's `./path` style.
// A link is written as RØDE's own are: a symlink member with no data.
func appendEntry(builder *Writer, entry Entry) error {
	path := "./" + entry.Path
	kind, size, link := byte(TypeRegular), uint64(len(entry.Data)), ""
	switch entry.Kind {
	case EntryDir:
		path += "/"
		kind, size = TypeDir, 0
	case EntryLink:
		if entry.Target == "" || len(entry.Data) > 0 {
			return fmt.Errorf("the link %s needs a target and no data", entry.Path)
		}
		kind, size, link = TypeSymlink, 0, entry.Target
	}
	header, err := gnuHeader(path, kind, entry.Mode, size, nowUnix(), link)
	if err != nil {
		return err
	}
	data := io.Reader(bytes.NewReader(entry.Data))
	if entry.Kind == EntryDir || entry.Kind == EntryLink {
		data = bytes.NewReader(nil)
	}
	return builder.Append(header, data)
}

// nowUnix is the current Unix time, or 0 before 1970.
func nowUnix() uint64 {
	now := time.Now().Unix()
	if now < 0 {
		return 0
	}
	return uint64(now)
}
