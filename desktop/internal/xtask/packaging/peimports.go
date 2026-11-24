package packaging

import (
	"bytes"
	"debug/pe"
	"os"
	"slices"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

var shippedDLLs = []string{"velopack_libc.dll"}

// checkWindowsImports ensures rc2fx.exe doesn't depend on MinGW or MSVC
// runtime DLLs (libgcc_s, libwinpthread, libstdc++, etc.) that aren't present
// on a clean Windows install or shipped alongside the app.
func checkWindowsImports(exe string) error {
	file, err := pe.Open(exe)
	if err != nil {
		return task.Errorf("%s: %s", exe, err)
	}
	defer file.Close()
	imports, err := file.ImportedLibraries()
	if err != nil {
		return task.Errorf("%s: cannot read its imports: %s", exe, err)
	}
	var unexpected []string
	for _, name := range imports {
		if !allowedWindowsImport(name) {
			unexpected = append(unexpected, name)
		}
	}
	if len(unexpected) > 0 {
		return task.Errorf("%s imports %s, which a clean Windows does not have and the package does not ship; link it statically or ship it", exe, strings.Join(unexpected, ", "))
	}
	return nil
}

// allowedWindowsImport accepts shipped DLLs and Windows system DLLs (including
// msvcrt.dll, UCRT, and api-ms-win-*), rejecting MinGW/MSVC runtime DLLs.
func allowedWindowsImport(name string) bool {
	lower := strings.ToLower(name)
	if slices.Contains(shippedDLLs, lower) {
		return true
	}
	for _, runtime := range []string{"lib", "vcruntime", "msvcp", "concrt", "vccorlib"} {
		if strings.HasPrefix(lower, runtime) {
			return false
		}
	}
	return strings.HasSuffix(lower, ".dll")
}

var commonControls6 = []byte("Microsoft.Windows.Common-Controls")

// checkWindowsManifest verifies that rc2fx.exe embeds a Common Controls 6
// manifest (provided by package gk's .syso). libgouikit imports
// TaskDialogIndirect from comctl32 v6, so missing this manifest causes startup
// failure on Windows.
func checkWindowsManifest(exe string) error {
	data, err := os.ReadFile(exe)
	if err != nil {
		return task.Errorf("%s: %s", exe, task.IOError(err))
	}
	if bytes.Contains(data, commonControls6) {
		return nil
	}
	return task.Errorf("%s has no application manifest asking for common controls 6 (package gk's), so Windows would not start it", exe)
}
