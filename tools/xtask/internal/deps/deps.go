// Package deps downloads the prebuilt libraries the desktop app links: the
// goui-kit static library (config/goui-kit.json) and Velopack's C library
// (config/velopack.json). Each download is checked against its pinned
// SHA-256 before anything is unpacked.
package deps

import (
	"archive/tar"
	"archive/zip"
	"compress/gzip"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	slashpath "path"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"github.com/klauspost/compress/zstd"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

// HostTarget is the GOOS-GOARCH target for this machine; macOS is always
// darwin-universal, since the app ships universal.
func HostTarget() string {
	if runtime.GOOS == "darwin" {
		return "darwin-universal"
	}
	return runtime.GOOS + "-" + runtime.GOARCH
}

func thirdParty(root string) string {
	return filepath.Join(root, "desktop", "third_party")
}

func readJSON(path string, into any) error {
	data, err := os.ReadFile(path)
	if err != nil {
		return task.Errorf("%s: %s", path, task.IOError(err))
	}
	if err := json.Unmarshal(data, into); err != nil {
		return task.Errorf("%s: %s", path, err)
	}
	return nil
}

func validSHA256(s string) bool {
	if len(s) != 64 {
		return false
	}
	_, err := hex.DecodeString(s)
	return err == nil && strings.ToLower(s) == s
}

// download saves url to path and fails unless its SHA-256 is want.
// Redirects must stay on HTTPS.
func download(url, path, want string) error {
	client := &http.Client{
		Timeout: 30 * time.Minute,
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			if req.URL.Scheme != "https" {
				return fmt.Errorf("redirect to %s is not HTTPS", req.URL)
			}
			if len(via) >= 10 {
				return errors.New("too many redirects")
			}
			return nil
		},
	}
	fmt.Printf("Downloading %s\n", url)
	var last error
	for attempt := 1; attempt <= 3; attempt++ {
		if attempt > 1 {
			time.Sleep(time.Duration(attempt) * 2 * time.Second)
		}
		got, err := downloadOnce(client, url, path)
		if err != nil {
			last = err
			continue
		}
		if got != want {
			return task.Errorf("%s: SHA-256 is %s, expected %s", url, got, want)
		}
		return nil
	}
	return task.Errorf("%s: %s", url, last)
}

func downloadOnce(client *http.Client, url, path string) (string, error) {
	resp, err := client.Get(url)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("HTTP %s", resp.Status)
	}
	file, err := os.Create(path)
	if err != nil {
		return "", err
	}
	hash := sha256.New()
	_, copyErr := io.Copy(io.MultiWriter(file, hash), resp.Body)
	closeErr := file.Close()
	if copyErr != nil {
		return "", copyErr
	}
	if closeErr != nil {
		return "", closeErr
	}
	return hex.EncodeToString(hash.Sum(nil)), nil
}

// inside joins name onto dir, refusing names that leave dir. Archive names
// use forward slashes, so a backslash or a drive letter is refused on every
// OS; IsLocal also refuses rooted names such as \abs, which IsAbs accepts
// on Windows.
func inside(dir, name string) (string, error) {
	local := filepath.FromSlash(name)
	if strings.ContainsAny(name, `\:`) || !filepath.IsLocal(local) {
		return "", fmt.Errorf("archive entry %q leaves the destination", name)
	}
	return filepath.Join(dir, local), nil
}

func writeFile(path string, mode os.FileMode, r io.Reader) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o777); err != nil {
		return err
	}
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, mode.Perm()|0o600)
	if err != nil {
		return err
	}
	_, copyErr := io.Copy(file, r)
	if err := file.Close(); copyErr == nil {
		copyErr = err
	}
	return copyErr
}

// extractTarZst unpacks a .tar.zst archive into dir.
func extractTarZst(archive, dir string) error {
	file, err := os.Open(archive)
	if err != nil {
		return err
	}
	defer file.Close()
	decoder, err := zstd.NewReader(file)
	if err != nil {
		return err
	}
	defer decoder.Close()
	return extractTar(decoder, dir)
}

// extractTarGz unpacks a .tar.gz archive into dir.
func extractTarGz(archive, dir string) error {
	file, err := os.Open(archive)
	if err != nil {
		return err
	}
	defer file.Close()
	decoder, err := gzip.NewReader(file)
	if err != nil {
		return err
	}
	defer decoder.Close()
	return extractTar(decoder, dir)
}

// extractTar unpacks a tar stream into dir, refusing entries and links that
// would leave it.
func extractTar(r io.Reader, dir string) error {
	reader := tar.NewReader(r)
	for {
		header, err := reader.Next()
		if err == io.EOF {
			return nil
		}
		if err != nil {
			return err
		}
		path, err := inside(dir, header.Name)
		if err != nil {
			return err
		}
		switch header.Typeflag {
		case tar.TypeDir:
			if err := os.MkdirAll(path, 0o777); err != nil {
				return err
			}
		case tar.TypeReg:
			if err := writeFile(path, os.FileMode(header.Mode), reader); err != nil {
				return err
			}
		case tar.TypeSymlink:
			target := slashpath.Join(slashpath.Dir(header.Name), header.Linkname)
			// Join drops a leading slash, so a rooted link is refused here.
			link := header.Linkname
			if strings.HasPrefix(link, "/") || strings.ContainsAny(link, `\:`) {
				return fmt.Errorf("archive entry %q links outside the archive", header.Name)
			}
			if _, err := inside(dir, target); err != nil {
				return fmt.Errorf("archive entry %q links outside the archive", header.Name)
			}
			if err := os.MkdirAll(filepath.Dir(path), 0o777); err != nil {
				return err
			}
			if err := os.Symlink(header.Linkname, path); err != nil {
				return err
			}
		default:
			return fmt.Errorf("archive entry %q has unsupported type %c", header.Name, header.Typeflag)
		}
	}
}

// extractZip unpacks a zip archive into dir.
func extractZip(archive, dir string) error {
	reader, err := zip.OpenReader(archive)
	if err != nil {
		return err
	}
	defer reader.Close()
	for _, entry := range reader.File {
		path, err := inside(dir, entry.Name)
		if err != nil {
			return err
		}
		if entry.FileInfo().IsDir() {
			if err := os.MkdirAll(path, 0o777); err != nil {
				return err
			}
			continue
		}
		if !entry.Mode().IsRegular() {
			return fmt.Errorf("archive entry %q is not a regular file", entry.Name)
		}
		src, err := entry.Open()
		if err != nil {
			return err
		}
		err = writeFile(path, entry.Mode(), src)
		src.Close()
		if err != nil {
			return err
		}
	}
	return nil
}
