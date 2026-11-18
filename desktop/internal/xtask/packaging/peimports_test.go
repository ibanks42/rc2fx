package packaging

import (
	"os"
	"path/filepath"
	"testing"
)

func TestTheWindowsAppMayImportOnlySystemAndShippedDLLs(t *testing.T) {
	for _, name := range []string{
		"KERNEL32.dll", "USER32.dll", "msvcrt.dll", "api-ms-win-crt-runtime-l1-1-0.dll",
		"ucrtbase.dll", "ADVAPI32.dll", "WS2_32.dll", "velopack_libc.dll", "Velopack_libc.DLL",
	} {
		if !allowedWindowsImport(name) {
			t.Errorf("%s refused", name)
		}
	}
	for _, name := range []string{
		"libwinpthread-1.dll", "libgcc_s_seh-1.dll", "libstdc++-6.dll", "LIBGCC_S_SEH-1.DLL",
		"vcruntime140.dll", "msvcp140.dll", "something.so",
	} {
		if allowedWindowsImport(name) {
			t.Errorf("%s allowed", name)
		}
	}
}

func TestTheWindowsAppMustCarryACommonControls6Manifest(t *testing.T) {
	dir := t.TempDir()
	exe := filepath.Join(dir, "rc2fx.exe")
	manifest := `<dependentAssembly><assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"/></dependentAssembly>`
	write := func(path, text string) {
		if err := os.WriteFile(path, []byte(text), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	write(exe, "MZ no manifest here")
	if err := checkWindowsManifest(exe); err == nil {
		t.Fatal("a program without a manifest was accepted")
	}
	// Only package gk's embedded manifest counts: nothing puts one beside.
	write(exe+".manifest", manifest)
	if err := checkWindowsManifest(exe); err == nil {
		t.Fatal("a manifest beside the program was accepted")
	}
	write(exe, "MZ ... "+manifest+" ...")
	if err := checkWindowsManifest(exe); err != nil {
		t.Fatalf("embedded: %v", err)
	}
}
