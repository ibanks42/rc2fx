package archive

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

	"github.com/ibanks42/rc2fx/desktop/internal/common"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"
)

type EntryKind int

const (
	EntryDir EntryKind = iota
	EntryFile
	EntryLink
)

// Entry describes a file, directory, or symlink to append to a firmware image.
type Entry struct {
	Kind   EntryKind
	Path   string
	Mode   uint32
	Data   []byte
	Target string // symlink target; only used with EntryLink
}

// Normalized strips a leading "./" and trailing "/" from a tar member path.
func Normalized(name string) string {
	name = strings.TrimPrefix(name, "./")
	return strings.TrimSuffix(name, "/")
}

const (
	MaxMixerMember = 64 << 20
	MaxSmallMember = 1 << 20
)

// Key members used to verify the firmware rootfs and record its hashes.
const (
	hostnameMember = "etc/hostname"
	mixerMember    = "usr/bin/rc_audio_mixer"
	launcherMember = "usr/bin/audio-start.sh"
)

// Patched holds the digests recorded while building a patched firmware archive.
type Patched struct {
	Mixer, Launcher string
	// Computed directly from the output stream as it is written so concurrent
	// changes in the cache directory can't skew the digests.
	ArchiveSHA, ArchiveMD5 string
}

// Copied describes a tar member observed during Patch.
type Copied struct {
	Name   string // normalized member path
	Header Header
	Size   uint64
	Data   []byte // populated only for etc/hostname and PatchOptions.Read members
	SHA256 string
}

type PatchOptions struct {
	Model     model.Model
	Extra     []Entry // appended after the original members, under opt/
	SourceSHA *string // if set, the bytes read from src must match this SHA-256
	// Read lists extra members to buffer in full (up to MaxSmallMember) and
	// pass to Member with Data populated. Each must appear once as a regular file.
	Read []string
	// Member is called for every non-metadata tar entry in order.
	Member func(Copied) error
	// Finish runs after Extra entries are written, allowing one last round of
	// generated entries to be appended.
	Finish func(add func(Entry) error) error
	// Verify runs once the archive is written; returning false rejects the image.
	Verify func() bool
}

// Patch copies src to dst with o.Extra appended, preserving all tar headers
// (including GNU long-name and PAX records) verbatim. When o.SourceSHA is set,
// the hash is verified against the exact bytes streamed from src.
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
			if err := member(Copied{Name: n, Header: header, Size: entry.Size, Data: data, SHA256: common.SHA256Hex(data)}); err != nil {
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
			digest := common.SHA256Hex(data)
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

func TrimASCII(data []byte) []byte {
	return bytes.Trim(data, " \t\n\r\f")
}

// AppendEntry writes one root-owned entry using RØDE's "./path" convention.
func AppendEntry(builder *Writer, entry Entry) error {
	return appendEntry(builder, entry)
}

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
	header, err := gnuHeader(path, kind, entry.Mode, size, common.NowUnix(), link)
	if err != nil {
		return err
	}
	data := io.Reader(bytes.NewReader(entry.Data))
	if entry.Kind == EntryDir || entry.Kind == EntryLink {
		data = bytes.NewReader(nil)
	}
	return builder.Append(header, data)
}
