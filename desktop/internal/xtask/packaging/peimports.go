package packaging

import (
	"bytes"
	"debug/pe"
	"os"
	"slices"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// shippedDLLs are the libraries the Windows package carries beside rc2fx.exe.
var shippedDLLs = []string{"velopack_libc.dll"}

// checkWindowsImports refuses an rc2fx.exe that needs a DLL a clean Windows
// does not have and the package does not ship. The app is linked by MinGW
// gcc, which can add its own runtime DLLs (libgcc_s, libwinpthread,
// libstdc++). Such an
// executable would pass every other check, be signed, and then not start on
// users' machines.
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

// allowedWindowsImport is whether rc2fx.exe may import the DLL name: the
// DLLs the package ships, or one Windows itself provides. MinGW's and
// MSVC's runtime DLLs are neither (msvcrt.dll and the api-ms-win and UCRT
// sets are part of Windows).
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

// commonControls6 is what a manifest that asks for common controls 6 names.
var commonControls6 = []byte("Microsoft.Windows.Common-Controls")

// checkWindowsManifest refuses an rc2fx.exe that would not start: gpuikit
// imports comctl32 6's TaskDialogIndirect, which Windows provides only to a
// program whose manifest asks for common controls 6. Package gk embeds that
// manifest (resource 1 of type 24); a link that kept MinGW's default one
// instead (no -force:multipleres with lld) has none.
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
