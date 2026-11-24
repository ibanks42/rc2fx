//go:build unix

package release

import (
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/desktop/common"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/tufsign"
)

// createKeys generates a new key set in destination, which must not exist and
// must live outside every git checkout.
func createKeys(destination, checkout string) error {
	parent, name, ok := splitDestination(destination)
	if !filepath.IsAbs(destination) || !ok {
		return task.Error("choose an absolute, new directory outside every git checkout")
	}
	parent, err := canonicalize(parent)
	if err != nil {
		return task.Errorf("destination parent must already exist: %s", task.IOError(err))
	}
	checkout, err = canonicalize(checkout)
	if err != nil {
		return task.Error(task.IOError(err))
	}
	if within(parent, checkout) || insideGit(parent) {
		return task.Error("private keys must be outside every git checkout")
	}
	destination = filepath.Join(parent, name)
	// Exclusive create avoids overwriting existing files or following symlinks.
	if err := os.Mkdir(destination, 0o700); err != nil {
		return task.Errorf("use a new key directory (existing paths are never replaced): %s", task.IOError(err))
	}
	writePrivate := func(path string, data []byte) error {
		file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
		if err != nil {
			return task.Error(task.IOError(err))
		}
		defer file.Close()
		if _, err := file.Write(data); err != nil {
			return task.Error(task.IOError(err))
		}
		if err := file.Sync(); err != nil {
			return task.Error(task.IOError(err))
		}
		return nil
	}
	for _, dir := range []string{"offline", "release", "public"} {
		if err := os.Mkdir(filepath.Join(destination, dir), 0o700); err != nil {
			return task.Error(task.IOError(err))
		}
	}
	// Write SETUP.md first so recovery guidance is present even if keygen fails
	// halfway through (we intentionally never auto-delete partial private keys).
	if err := writePrivate(filepath.Join(destination, "SETUP.md"), []byte(instructions)); err != nil {
		return err
	}
	roles := []string{tufsign.RoleRoot, tufsign.RoleTargets, tufsign.RoleSnapshot, tufsign.RoleTimestamp}
	var keys []tufsign.Key
	var rootSigners []tufsign.Signer
	for _, role := range roles {
		document, err := tufsign.GenerateRSA3072()
		if err != nil {
			return task.Error(err.Error())
		}
		folder := "release"
		if role == tufsign.RoleRoot {
			folder = "offline"
		}
		path := filepath.Join(destination, folder, role+".pem")
		if err := writePrivate(path, tufsign.PrivatePEM(document)); err != nil {
			return err
		}
		signer, err := tufsign.LoadKey(path)
		if err != nil {
			return task.Error(err.Error())
		}
		keys = append(keys, signer.Key())
		if role == tufsign.RoleRoot {
			rootSigners = append(rootSigners, signer)
		}
	}
	signed, err := tufsign.NewRoot(time.Now().Add(3*365*86400*time.Second), roles, keys)
	if err != nil {
		return task.Error(err.Error())
	}
	root, err := tufsign.RootFromSigned(signed)
	if err != nil {
		return task.Error(err.Error())
	}
	buffer, err := root.Sign(tufsign.RoleRoot, signed, rootSigners)
	if err != nil {
		return task.Error(err.Error())
	}
	written, err := tufsign.ParseRoot(buffer)
	if err != nil {
		return task.Error(err.Error())
	}
	if err := written.VerifyRole(written.Document); err != nil {
		return task.Error(err.Error())
	}
	if err := writePrivate(filepath.Join(destination, "public", "root.json"), buffer); err != nil {
		return err
	}
	if err := writePrivate(filepath.Join(destination, "public", "root.sha256"), []byte(common.SHA256Hex(buffer)+"  root.json\n")); err != nil {
		return err
	}
	// Sentinel written last to distinguish a finished setup from a partial one.
	return writePrivate(filepath.Join(destination, "COMPLETE"), []byte("Root signatures verified. Follow SETUP.md before use.\n"))
}

// splitDestination splits an absolute path into (parent, name), stripping
// trailing slashes and "." segments and rejecting root or "..".
func splitDestination(path string) (parent, name string, ok bool) {
	for {
		trimmed := strings.TrimRight(path, "/")
		if strings.HasSuffix(trimmed, "/.") {
			path = strings.TrimSuffix(trimmed, ".")
			continue
		}
		path = trimmed
		break
	}
	at := strings.LastIndex(path, "/")
	if at < 0 {
		name = path
		parent = ""
	} else {
		name = path[at+1:]
		parent = path[:at]
		if strings.TrimRight(parent, "/") == "" {
			parent = "/"
		}
	}
	if name == "" || name == "." || name == ".." {
		return "", "", false
	}
	return parent, name, true
}

func canonicalize(path string) (string, error) {
	abs, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	resolved, err := filepath.EvalSymlinks(abs)
	if err != nil {
		return "", err
	}
	if _, err := os.Stat(resolved); err != nil {
		return "", err
	}
	return resolved, nil
}

func within(path, base string) bool {
	rel, err := filepath.Rel(base, path)
	return err == nil && rel != ".." && !strings.HasPrefix(rel, "../") && !filepath.IsAbs(rel)
}

func insideGit(path string) bool {
	for {
		if _, err := os.Stat(filepath.Join(path, ".git")); err == nil {
			return true
		}
		parent := filepath.Dir(path)
		if parent == path {
			return false
		}
		path = parent
	}
}
