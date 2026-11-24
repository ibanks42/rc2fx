package deps

import (
	"archive/zip"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"slices"
	"strings"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

type velopackConfig struct {
	Version    string `json:"version"`
	LibcSHA256 string `json:"libcSha256"`
}

// velopackSources maps each target to the file in velopack_libc's zip that
// it links. Windows links the MSVC DLL through its C exports only, so the
// two C runtimes stay apart; macOS links each architecture's static library
// rather than the universal dylib, whose install name points at Velopack's
// build machine.
var velopackSources = map[string]string{
	"linux-amd64":   "lib-static/velopack_libc_linux_x64_gnu.a",
	"linux-arm64":   "lib-static/velopack_libc_linux_arm64_gnu.a",
	"darwin-arm64":  "lib-static/velopack_libc_osx_arm64_gnu.a",
	"darwin-amd64":  "lib-static/velopack_libc_osx_x64_gnu.a",
	"windows-amd64": "lib/velopack_libc_win_x64_msvc.dll",
}

var velopackExport = regexp.MustCompile(`\bvpkc_[a-z_]+\(`)

// VelopackDir is where velopack_libc is staged: include/Velopack.h, one
// folder per GOOS-GOARCH, and VERSION.
func VelopackDir(root string) string {
	return filepath.Join(thirdParty(root), "velopack")
}

// FetchVelopack downloads velopack_libc at the version pinned in
// config/velopack.json and stages it for targets (defaulting to this
// machine when empty), replacing whatever was staged before.
// darwin-universal expands to both macOS architectures.
func FetchVelopack(targets []string) error {
	root := task.Root()
	if len(targets) == 0 {
		targets = []string{HostTarget()}
	}
	var expanded []string
	for _, target := range targets {
		if target == "darwin-universal" {
			expanded = append(expanded, "darwin-arm64", "darwin-amd64")
		} else {
			expanded = append(expanded, target)
		}
	}
	for _, target := range expanded {
		if _, ok := velopackSources[target]; !ok {
			return task.Errorf("no velopack_libc prebuilt for %s", target)
		}
	}
	configPath := filepath.Join(root, "config", "velopack.json")
	var config velopackConfig
	if err := readJSON(configPath, &config); err != nil {
		return err
	}
	if strings.TrimSpace(config.Version) == "" || !validSHA256(config.LibcSHA256) {
		return task.Errorf("%s has no version or libcSha256", configPath)
	}

	if err := os.MkdirAll(thirdParty(root), 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	stage, err := os.MkdirTemp(thirdParty(root), ".fetch-velopack.")
	if err != nil {
		return task.Error(task.IOError(err))
	}
	defer os.RemoveAll(stage)
	archive := filepath.Join(stage, "velopack_libc.zip")
	url := fmt.Sprintf("https://github.com/velopack/velopack/releases/download/%s/velopack_libc_%s.zip", config.Version, config.Version)
	if err := download(url, archive, config.LibcSHA256); err != nil {
		return err
	}
	reader, err := zip.OpenReader(archive)
	if err != nil {
		return task.Errorf("%s: %s", url, err)
	}
	defer reader.Close()

	out := filepath.Join(stage, "velopack")
	header := filepath.Join(out, "include", "Velopack.h")
	if err := copyMember(&reader.Reader, "include/Velopack.h", header); err != nil {
		return err
	}
	for _, target := range expanded {
		dir := filepath.Join(out, target)
		if !strings.HasPrefix(target, "windows-") {
			if err := copyMember(&reader.Reader, velopackSources[target], filepath.Join(dir, "libvelopack_libc.a")); err != nil {
				return err
			}
			continue
		}
		// Named to match the LIBRARY name in the zip's import library.
		if err := copyMember(&reader.Reader, velopackSources[target], filepath.Join(dir, "velopack_libc.dll")); err != nil {
			return err
		}
		if err := writeDef(header, filepath.Join(dir, "velopack_libc.def")); err != nil {
			return err
		}
		if err := importLibrary(dir); err != nil {
			return err
		}
	}
	if err := os.WriteFile(filepath.Join(out, "VERSION"), []byte(config.Version+"\n"), 0o644); err != nil {
		return task.Error(task.IOError(err))
	}
	dest := VelopackDir(root)
	if err := os.RemoveAll(dest); err != nil {
		return task.Errorf("%s: %s", dest, task.IOError(err))
	}
	if err := os.Rename(out, dest); err != nil {
		return task.Errorf("%s: %s", dest, task.IOError(err))
	}
	fmt.Printf("velopack_libc %s is ready in %s for %s\n", config.Version, dest, strings.Join(expanded, " "))
	return nil
}

func copyMember(reader *zip.Reader, name, to string) error {
	member, err := reader.Open(name)
	if err != nil {
		return task.Errorf("velopack_libc zip: %s: %s", name, err)
	}
	defer member.Close()
	if err := writeFile(to, 0o644, member); err != nil {
		return task.Errorf("%s: %s", to, task.IOError(err))
	}
	return nil
}

// writeDef lists every vpkc_ function Velopack.h declares as an export of
// velopack_libc.dll, for MinGW's import library.
func writeDef(header, def string) error {
	file, err := os.Open(header)
	if err != nil {
		return task.Error(task.IOError(err))
	}
	text, err := io.ReadAll(file)
	file.Close()
	if err != nil {
		return task.Error(task.IOError(err))
	}
	var names []string
	for _, match := range velopackExport.FindAllString(string(text), -1) {
		names = append(names, strings.TrimSuffix(match, "("))
	}
	slices.Sort(names)
	names = slices.Compact(names)
	var b strings.Builder
	b.WriteString("LIBRARY velopack_libc.dll\nEXPORTS\n")
	for _, name := range names {
		b.WriteString("    " + name + "\n")
	}
	if err := os.WriteFile(def, []byte(b.String()), 0o644); err != nil {
		return task.Error(task.IOError(err))
	}
	return nil
}

// importLibrary builds libvelopack_libc.dll.a from the .def with the first
// dlltool on PATH, and skips it when there is none.
func importLibrary(dir string) error {
	for _, tool := range []string{"x86_64-w64-mingw32-dlltool", "dlltool", "llvm-dlltool"} {
		path, err := exec.LookPath(tool)
		if err != nil {
			continue
		}
		return task.Run(exec.Command(path, "-m", "i386:x86-64",
			"-d", filepath.Join(dir, "velopack_libc.def"),
			"-l", filepath.Join(dir, "libvelopack_libc.dll.a")))
	}
	return nil
}
