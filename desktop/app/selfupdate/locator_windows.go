package selfupdate

import (
	"os"
	"path/filepath"
	"strings"

	"golang.org/x/sys/windows"
)

// VelopackLocator is unused on Windows because Velopack's default packages
// path is already per-user.
func VelopackLocator() (LocatorConfig, bool) { return LocatorConfig{}, false }

// PackagesDir mirrors Velopack 1.3.39's Windows locator (not exposed by
// velopack_libc): <root>\packages if the install root is writable, otherwise
// %LocalAppData%\<id>\packages.
func PackagesDir() (string, bool) {
	exe, err := os.Executable()
	if err != nil {
		return "", false
	}
	root := ""
	if parent := filepath.Dir(exe); exists(filepath.Join(parent, "Update.exe")) {
		root = parent
	} else if i := strings.LastIndex(exe, `\current\`); i >= 0 && exists(filepath.Join(exe[:i], "Update.exe")) {
		root = exe[:i]
	}
	if root == "" {
		return "", false
	}
	m, err := readManifest(filepath.Join(root, "current", "sq.version"))
	if err != nil {
		return "", false
	}
	if writable(root) {
		return filepath.Join(root, "packages"), true
	}
	appData, err := windows.KnownFolderPath(windows.FOLDERID_LocalAppData, 0)
	if err != nil {
		// Fall back to root\packages, matching Velopack's behavior.
		return filepath.Join(root, "packages"), true
	}
	return filepath.Join(appData, m.ID, "packages"), true
}

// writable checks write access by creating a self-deleting probe file, matching
// Velopack's is_directory_writable.
func writable(dir string) bool {
	name, err := windows.UTF16PtrFromString(filepath.Join(dir, ".velopack_dir_test"))
	if err != nil {
		return false
	}
	h, err := windows.CreateFile(name, windows.GENERIC_WRITE,
		windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE|windows.FILE_SHARE_DELETE, nil,
		windows.CREATE_ALWAYS, windows.FILE_ATTRIBUTE_NORMAL|windows.FILE_FLAG_DELETE_ON_CLOSE, 0)
	if err != nil {
		return false
	}
	windows.CloseHandle(h)
	return true
}

func EnsurePrivate(dir string) error { return nil }
