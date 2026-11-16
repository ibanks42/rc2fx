package installer

// Private local files: journals, keys, downloads and backups.

import (
	"crypto/md5"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"hash"
	"io"
	"io/fs"
	"os"
	"path/filepath"
)

func exists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

func hexString(data []byte) string { return hex.EncodeToString(data) }

func sum(data []byte) string {
	digest := sha256.Sum256(data)
	return hex.EncodeToString(digest[:])
}

type algorithm int

const (
	algorithmSha256 algorithm = iota
	algorithmMd5
)

func (a algorithm) name() string {
	if a == algorithmMd5 {
		return "md5"
	}
	return "sha256"
}

func (a algorithm) hasher() hash.Hash {
	if a == algorithmMd5 {
		return md5.New()
	}
	return sha256.New()
}

// hashReader hashes a reader, reporting the bytes read so far after each
// block to progress.
func hashReader(reader io.Reader, a algorithm, progress func(uint64)) (string, error) {
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

func fileHash(path string, a algorithm) (string, error) {
	file, err := openRegular(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	return hashReader(file, a, func(uint64) {})
}

func matches(path string, a algorithm, want string) error {
	got, err := fileHash(path, a)
	if err != nil {
		return err
	}
	if want == "" || got != want {
		return errorf("checksum mismatch: %s (%s)", filepath.Base(path), a.name())
	}
	return nil
}

// privateDirs creates dir and its missing parents, private to this user.
func privateDirs(dir string) error {
	info, err := os.Stat(dir)
	switch {
	case err == nil && info.IsDir():
		return nil
	case err == nil:
		return errorf("mkdir %s: not a directory", dir)
	case !errors.Is(err, fs.ErrNotExist):
		return err
	}
	if parent := filepath.Dir(dir); parent != dir && parent != "." && parent != "" {
		if err := privateDirs(parent); err != nil {
			return err
		}
	}
	if err := os.Mkdir(dir, 0o700); err != nil {
		if errors.Is(err, fs.ErrExist) {
			return privateDirs(dir)
		}
		return err
	}
	file, err := os.Open(dir)
	if err != nil {
		return err
	}
	defer file.Close()
	return localOwner(file)
}

// temp is a uniquely named private file. Close it with cleanup unless
// persist renamed it.
type temp struct {
	file *os.File
	path string
}

func newTemp(dir, prefix string) (*temp, error) {
	for range 64 {
		nonce := make([]byte, 8)
		if _, err := rand.Read(nonce); err != nil {
			return nil, err
		}
		path := filepath.Join(dir, prefix+hexString(nonce))
		file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
		if errors.Is(err, fs.ErrExist) {
			continue
		}
		if err != nil {
			return nil, err
		}
		t := &temp{file: file, path: path}
		if err := localOwner(file); err != nil {
			t.cleanup()
			return nil, err
		}
		return t, nil
	}
	return nil, errorf("could not create a temporary file in %s", dir)
}

func (t *temp) Write(data []byte) (int, error) {
	if t.file == nil {
		return 0, os.ErrClosed
	}
	return t.file.Write(data)
}

// finish flushes data to disk and closes the handle, keeping the file.
func (t *temp) finish() error {
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

// persist finishes, then renames over dest.
func (t *temp) persist(dest string) error {
	if err := t.finish(); err != nil {
		return err
	}
	if err := replaceLocalFile(t.path, dest); err != nil {
		return err
	}
	t.path = ""
	return nil
}

// keep forgets the file without removing it.
func (t *temp) keep() { t.path = "" }

// cleanup closes and removes the file unless it was persisted or kept.
func (t *temp) cleanup() {
	if t.file != nil {
		t.file.Close()
		t.file = nil
	}
	if t.path != "" {
		os.Remove(t.path)
		t.path = ""
	}
}

func regularOnly(file *os.File) (*os.File, error) {
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

// openRegular opens a regular file for reading. The cache folder is the
// desktop user's, so the root helper must not be led elsewhere by a link
// placed in it.
func openRegular(path string) (*os.File, error) {
	file, err := os.OpenFile(path, os.O_RDONLY|noFollow, 0)
	if err != nil {
		return nil, err
	}
	return regularOnly(file)
}

// openWorkFile is openRegular for a file in the cache folder, which must
// also belong to the user who started a privileged helper. The drive's
// files do not.
func openWorkFile(path string) (*os.File, error) {
	file, err := openRegular(path)
	if err != nil {
		return nil, err
	}
	return ownedByInvoker(file)
}

// openLocalFile creates the file privately if missing, otherwise opens the
// existing one.
func openLocalFile(path string, appendMode bool) (*os.File, error) {
	flags := os.O_WRONLY
	if appendMode {
		flags |= os.O_APPEND
	}
	file, err := os.OpenFile(path, flags|os.O_CREATE|os.O_EXCL, 0o600)
	if err == nil {
		if err := localOwner(file); err != nil {
			file.Close()
			return nil, err
		}
		return file, nil
	}
	if !errors.Is(err, fs.ErrExist) {
		return nil, err
	}
	file, err = os.OpenFile(path, flags|noFollow, 0o600)
	if err != nil {
		return nil, err
	}
	if file, err = regularOnly(file); err != nil {
		return nil, err
	}
	return ownedByInvoker(file)
}

// atomicFile writes a whole file through a synced temporary and an atomic
// rename.
func atomicFile(path string, data []byte) error {
	t, err := newTemp(filepath.Dir(path), ".rc2fx-")
	if err != nil {
		return err
	}
	defer t.cleanup()
	if _, err := t.Write(data); err != nil {
		return err
	}
	return t.persist(path)
}

// exclusiveFile creates a new file exclusively; it refuses to replace an
// existing one.
func exclusiveFile(path string, data []byte) error {
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return err
	}
	defer file.Close()
	if err := localOwner(file); err != nil {
		return err
	}
	if _, err := file.Write(data); err != nil {
		return err
	}
	return file.Sync()
}

// hashing is the SHA-256 and MD5 of the bytes that pass through a reader
// or writer, so a digest describes what this process read or wrote, not
// what a path holds afterwards.
type hashing struct {
	r   io.Reader
	w   io.Writer
	sha hash.Hash
	md  hash.Hash
}

func hashingReader(r io.Reader) *hashing {
	return &hashing{r: r, sha: sha256.New(), md: md5.New()}
}

func hashingWriter(w io.Writer) *hashing {
	return &hashing{w: w, sha: sha256.New(), md: md5.New()}
}

func (h *hashing) Read(buf []byte) (int, error) {
	n, err := h.r.Read(buf)
	h.sha.Write(buf[:n])
	h.md.Write(buf[:n])
	return n, err
}

func (h *hashing) Write(buf []byte) (int, error) {
	n, err := h.w.Write(buf)
	h.sha.Write(buf[:n])
	h.md.Write(buf[:n])
	return n, err
}

// finish is the SHA-256 and MD5 of everything passed through.
func (h *hashing) finish() (string, string) {
	return hex.EncodeToString(h.sha.Sum(nil)), hex.EncodeToString(h.md.Sum(nil))
}

// hashBoth is the SHA-256 and MD5 of a file in one read. progress gets
// (read, total).
func hashBoth(path string, progress func(uint64, uint64)) (string, string, error) {
	file, err := openRegular(path)
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

// copyWithProgress copies a file to a new destination and flushes it.
// progress gets (written, total).
func copyWithProgress(src, dst string, progress func(uint64, uint64)) error {
	input, err := openWorkFile(src)
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

// readLimited reads at most limit bytes of r.
func readLimited(r io.Reader, limit int64) ([]byte, error) {
	return io.ReadAll(io.LimitReader(r, limit))
}
