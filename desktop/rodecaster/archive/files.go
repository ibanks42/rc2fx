package archive

// Private local files: hashing, temporary files, atomic writes and
// regular-file opening.

import (
	"crypto/md5"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"hash"
	"io"
	"io/fs"
	"os"
	"path/filepath"
)

// Sum is the hex SHA-256 of data.
func Sum(data []byte) string {
	digest := sha256.Sum256(data)
	return hex.EncodeToString(digest[:])
}

// Algorithm is a file digest algorithm.
type Algorithm int

const (
	SHA256 Algorithm = iota
	MD5
)

// Name is the algorithm's lowercase name.
func (a Algorithm) Name() string {
	if a == MD5 {
		return "md5"
	}
	return "sha256"
}

func (a Algorithm) hasher() hash.Hash {
	if a == MD5 {
		return md5.New()
	}
	return sha256.New()
}

// HashReader hashes a reader, reporting the bytes read so far after each
// block to progress.
func HashReader(reader io.Reader, a Algorithm, progress func(uint64)) (string, error) {
	h := a.hasher()
	buf := make([]byte, 1<<16)
	var total uint64
	for {
		n, err := reader.Read(buf)
		if n > 0 {
			h.Write(buf[:n])
			total += uint64(n)
			progress(total)
		}
		if err == io.EOF {
			break
		}
		if err != nil {
			return "", err
		}
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

// FileHash is the hex digest of a regular file.
func FileHash(path string, a Algorithm) (string, error) {
	file, err := OpenRegular(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	return HashReader(file, a, func(uint64) {})
}

// Matches is nil when the file at path has digest want.
func Matches(path string, a Algorithm, want string) error {
	got, err := FileHash(path, a)
	if err != nil {
		return err
	}
	if want == "" || got != want {
		return fmt.Errorf("checksum mismatch: %s (%s)", filepath.Base(path), a.Name())
	}
	return nil
}

// Temp is a uniquely named private file. Close it with Cleanup unless
// Persist renamed it.
type Temp struct {
	file *os.File
	path string
}

// NewTemp creates a private file named prefix plus a random suffix in dir.
func NewTemp(dir, prefix string) (*Temp, error) {
	for range 64 {
		nonce := make([]byte, 8)
		if _, err := rand.Read(nonce); err != nil {
			return nil, err
		}
		path := filepath.Join(dir, prefix+hex.EncodeToString(nonce))
		file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
		if errors.Is(err, fs.ErrExist) {
			continue
		}
		if err != nil {
			return nil, err
		}
		t := &Temp{file: file, path: path}
		if err := LocalOwner(file); err != nil {
			t.Cleanup()
			return nil, err
		}
		return t, nil
	}
	return nil, fmt.Errorf("could not create a temporary file in %s", dir)
}

// Path is the file's path, or "" once it was persisted or kept.
func (t *Temp) Path() string { return t.path }

func (t *Temp) Write(data []byte) (int, error) {
	if t.file == nil {
		return 0, os.ErrClosed
	}
	return t.file.Write(data)
}

// Finish flushes data to disk and closes the handle, keeping the file.
func (t *Temp) Finish() error {
	if t.file == nil {
		return nil
	}
	file := t.file
	t.file = nil
	if err := file.Sync(); err != nil {
		file.Close()
		return err
	}
	return file.Close()
}

// Persist finishes, then renames over dest.
func (t *Temp) Persist(dest string) error {
	if err := t.Finish(); err != nil {
		return err
	}
	if err := ReplaceLocalFile(t.path, dest); err != nil {
		return err
	}
	t.path = ""
	return nil
}

// Keep forgets the file without removing it.
func (t *Temp) Keep() { t.path = "" }

// Cleanup closes and removes the file unless it was persisted or kept.
func (t *Temp) Cleanup() {
	if t.file != nil {
		t.file.Close()
		t.file = nil
	}
	if t.path != "" {
		os.Remove(t.path)
		t.path = ""
	}
}

// RegularOnly returns file if it is a regular file, and closes it if not.
func RegularOnly(file *os.File) (*os.File, error) {
	info, err := file.Stat()
	if err != nil {
		file.Close()
		return nil, err
	}
	if !info.Mode().IsRegular() {
		file.Close()
		return nil, errors.New("expected a regular file")
	}
	return file, nil
}

// OpenRegular opens a regular file for reading. The cache folder is the
// desktop user's, so the root helper must not be led elsewhere by a link
// placed in it.
func OpenRegular(path string) (*os.File, error) {
	file, err := os.OpenFile(path, os.O_RDONLY|NoFollow, 0)
	if err != nil {
		return nil, err
	}
	return RegularOnly(file)
}

// OpenWorkFile is OpenRegular for a file in the cache folder, which must
// also belong to the user who started a privileged helper. The drive's
// files do not.
func OpenWorkFile(path string) (*os.File, error) {
	file, err := OpenRegular(path)
	if err != nil {
		return nil, err
	}
	return OwnedByInvoker(file)
}

// AtomicFile writes a whole file through a synced temporary and an atomic
// rename.
func AtomicFile(path string, data []byte) error {
	t, err := NewTemp(filepath.Dir(path), ".rc2fx-")
	if err != nil {
		return err
	}
	defer t.Cleanup()
	if _, err := t.Write(data); err != nil {
		return err
	}
	return t.Persist(path)
}

// Hashing is the SHA-256 and MD5 of the bytes that pass through a reader
// or writer, so a digest describes what this process read or wrote, not
// what a path holds afterwards.
type Hashing struct {
	r   io.Reader
	w   io.Writer
	sha hash.Hash
	md  hash.Hash
}

func HashingReader(r io.Reader) *Hashing {
	return &Hashing{r: r, sha: sha256.New(), md: md5.New()}
}

func HashingWriter(w io.Writer) *Hashing {
	return &Hashing{w: w, sha: sha256.New(), md: md5.New()}
}

func (h *Hashing) Read(buf []byte) (int, error) {
	n, err := h.r.Read(buf)
	h.sha.Write(buf[:n])
	h.md.Write(buf[:n])
	return n, err
}

func (h *Hashing) Write(buf []byte) (int, error) {
	n, err := h.w.Write(buf)
	h.sha.Write(buf[:n])
	h.md.Write(buf[:n])
	return n, err
}

// Finish is the SHA-256 and MD5 of everything passed through.
func (h *Hashing) Finish() (string, string) {
	return hex.EncodeToString(h.sha.Sum(nil)), hex.EncodeToString(h.md.Sum(nil))
}

// HashBoth is the SHA-256 and MD5 of a file in one read. progress gets
// (read, total).
func HashBoth(path string, progress func(uint64, uint64)) (string, string, error) {
	file, err := OpenRegular(path)
	if err != nil {
		return "", "", err
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return "", "", err
	}
	total := uint64(info.Size())
	sha, md := sha256.New(), md5.New()
	buf := make([]byte, 1<<20)
	var done uint64
	progress(0, total)
	for {
		n, err := file.Read(buf)
		if n > 0 {
			sha.Write(buf[:n])
			md.Write(buf[:n])
			done += uint64(n)
			progress(done, total)
		}
		if err == io.EOF {
			break
		}
		if err != nil {
			return "", "", err
		}
	}
	return hex.EncodeToString(sha.Sum(nil)), hex.EncodeToString(md.Sum(nil)), nil
}

// CopyWithProgress copies a file to a new destination and flushes it.
// progress gets (written, total).
func CopyWithProgress(src, dst string, progress func(uint64, uint64)) error {
	input, err := OpenWorkFile(src)
	if err != nil {
		return err
	}
	defer input.Close()
	info, err := input.Stat()
	if err != nil {
		return err
	}
	total := uint64(info.Size())
	out, err := os.OpenFile(dst, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return err
	}
	defer out.Close()
	buf := make([]byte, 1<<20)
	var done uint64
	progress(0, total)
	for {
		n, err := input.Read(buf)
		if n > 0 {
			if _, err := out.Write(buf[:n]); err != nil {
				return err
			}
			done += uint64(n)
			progress(done, total)
		}
		if err == io.EOF {
			break
		}
		if err != nil {
			return err
		}
	}
	return out.Sync()
}
