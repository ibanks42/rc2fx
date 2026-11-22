package archive

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

type Algorithm int

const (
	SHA256 Algorithm = iota
	MD5
)

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

// HashReader hashes reader and invokes progress with the cumulative byte count
// after each chunk.
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

func FileHash(path string, a Algorithm) (string, error) {
	file, err := OpenRegular(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	return HashReader(file, a, func(uint64) {})
}

// Matches verifies that the file at path has the expected hex digest.
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

// Temp wraps a uniquely named 0600 temp file. Call Cleanup to remove it unless
// Persist or Keep took ownership.
type Temp struct {
	file *os.File
	path string
}

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

func (t *Temp) Path() string { return t.path }

func (t *Temp) Write(data []byte) (int, error) {
	if t.file == nil {
		return 0, os.ErrClosed
	}
	return t.file.Write(data)
}

// Finish syncs and closes the file without deleting it.
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

// Persist syncs, closes, and atomically replaces dest with the temp file.
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

// Keep detaches the path so Cleanup will leave the file on disk.
func (t *Temp) Keep() { t.path = "" }

// Cleanup closes and removes the temp file unless it was persisted or kept.
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

// RegularOnly verifies that file is a regular file, closing it on failure.
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

// OpenRegular opens path for reading without following symlinks and ensures it
// is a regular file. The cache directory belongs to the desktop user, so the
// root helper must not follow symlinks planted there.
func OpenRegular(path string) (*os.File, error) {
	file, err := os.OpenFile(path, os.O_RDONLY|NoFollow, 0)
	if err != nil {
		return nil, err
	}
	return RegularOnly(file)
}

// OpenWorkFile opens a regular cache file and checks that it belongs to the
// user who invoked the privileged helper (unlike files on the USB drive).
func OpenWorkFile(path string) (*os.File, error) {
	file, err := OpenRegular(path)
	if err != nil {
		return nil, err
	}
	return OwnedByInvoker(file)
}

// AtomicFile writes data to path via a synced temp file and rename.
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

// Hashing tees reads or writes into both SHA-256 and MD5 so digests reflect the
// exact bytes processed in flight instead of re-reading from disk later.
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

// Finish returns the hex-encoded SHA-256 and MD5 digests.
func (h *Hashing) Finish() (string, string) {
	return hex.EncodeToString(h.sha.Sum(nil)), hex.EncodeToString(h.md.Sum(nil))
}

// HashBoth computes SHA-256 and MD5 of path in a single pass, reporting
// (read, total) to progress.
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

// CopyWithProgress copies src to a new file at dst, syncs it, and reports
// (written, total) to progress.
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
