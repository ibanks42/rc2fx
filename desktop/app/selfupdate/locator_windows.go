package selfupdate

import (
	"os"
	"path/filepath"
	"strings"

	"golang.org/x/sys/windows"
)

// VelopackLocator is only given on Linux; elsewhere Velopack's own choice is
// already private to the user.
func VelopackLocator() (LocatorConfig, bool) { return LocatorConfig{}, false }

// PackagesDir is where Velopack keeps this copy's packages:
// auto_locate_app_manifest(FromCurrentExe).get_packages_dir() of velopack
// 1.3.39 on Windows, which the C library does not offer. The install root
// is the folder with Update.exe beside or above `current\`; its `packages`
// folder when the root is writable, else %LocalAppData%\<id>\packages.
// ok is false when Velopack did not install this copy.
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
		// Velopack keeps root\packages then, and says updates may not work.
		return filepath.Join(root, "packages"), true
	}
	return filepath.Join(appData, m.ID, "packages"), true
}

// writable is velopack's misc::is_directory_writable: a test file that
// deletes itself on close.
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

// EnsurePrivate is a Linux check; elsewhere the folder is the user's own.
func EnsurePrivate(dir string) error { return nil }
