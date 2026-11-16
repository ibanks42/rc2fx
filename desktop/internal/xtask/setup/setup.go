// Package setup is one-time setup and the mixer cross-build. The mixer build
// uses zig as the C compiler and linker (through cargo-zigbuild), so it runs
// natively on any host OS and links against the mixer's glibc 2.35 without a
// sysroot.
package setup

import (
	"bytes"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/elfcheck"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// Target is the mixer's Rust target.
const Target = "aarch64-unknown-linux-gnu"

// ZigTarget: firmware 1.7.6 ships glibc 2.35; zig links against exactly that
// version.
const ZigTarget = "aarch64-unknown-linux-gnu.2.35"

// Run installs the Rust target and cargo-zigbuild, and checks for zig.
func Run() error {
	fmt.Printf("Adding the Rust %s target\n", Target)
	if err := task.Run(exec.Command("rustup", "target", "add", Target)); err != nil {
		return err
	}
	if !succeeds(exec.Command("cargo-zigbuild", "--version")) {
		fmt.Println("Installing cargo-zigbuild")
		if err := task.Run(exec.Command("cargo", "install", "--locked", "cargo-zigbuild")); err != nil {
			return err
		}
	}
	if err := CheckZig(); err != nil {
		return err
	}
	fmt.Println("Setup complete. Build the mixer side with `just mixer-build`.")
	return nil
}

func succeeds(command *exec.Cmd) bool {
	command.Stdout = nil
	command.Stderr = nil
	return command.Run() == nil
}

// CheckZig requires zig on PATH and says which one is used.
func CheckZig() error {
	output, err := exec.Command("zig", "version").Output()
	if err == nil {
		fmt.Printf("Using zig %s\n", strings.TrimSpace(strings.ToValidUTF8(string(output), "�")))
		return nil
	}
	return task.Error("zig is not on PATH. Install it (0.13 or newer), then rerun setup:\n  " +
		"Windows: scoop install zig   or   winget install zig.zig\n  " +
		"macOS:   brew install zig\n  " +
		"Linux:   snap install zig --classic --beta, your package manager, or pip install ziglang\n  " +
		"https://ziglang.org/download/")
}

// MixerRustflags is the mixer's RUSTFLAGS: code generation for its
// Cortex-A53, exactly as the Pro II profile was measured. They replace any
// RUSTFLAGS of the caller's, so a host setting never reaches the mixer build.
func MixerRustflags() string { return "-C target-cpu=cortex-a53" }

// CheckMixerStaging refuses to replace target/mixer when it is not a plain
// directory of this checkout's builds.
func CheckMixerStaging(path string) error {
	info, err := os.Lstat(path)
	switch {
	case err == nil && info.Mode()&fs.ModeSymlink != 0:
		return task.Error("target/mixer is a link to another bundle; refusing to replace it. Use `./xtask mixer install` to deploy that bundle, or move the link aside to rebuild.")
	case err == nil && info.IsDir():
		return nil
	case err == nil:
		return task.Error("target/mixer is not a directory; refusing to replace it")
	case errors.Is(err, fs.ErrNotExist):
		return nil
	}
	return task.Errorf("cannot inspect mixer staging: %s", task.IOError(err))
}

// DefaultCatalog is config/firmware-profiles.json, the catalog the Rust
// crates compile in.
func DefaultCatalog(root string) (string, error) {
	data, err := os.ReadFile(filepath.Join(root, "config", "firmware-profiles.json"))
	if err != nil {
		return "", task.Errorf("config/firmware-profiles.json: %s", task.IOError(err))
	}
	return string(data), nil
}

// zigbuild is `cargo zigbuild` for the mixer, in its own target directory.
func zigbuild(root, targetDir string, args ...string) *exec.Cmd {
	command := exec.Command("cargo", append([]string{"zigbuild", "--locked", "--release", "--target", ZigTarget}, args...)...)
	command.Dir = root
	command.Env = append(os.Environ(), "CARGO_TARGET_DIR="+targetDir, "RUSTFLAGS="+MixerRustflags())
	return command
}

// BuildMixer cross-builds the mixer binaries and profile checker into
// target/mixer.
func BuildMixer() error {
	root := task.Root()
	catalog, err := DefaultCatalog(root)
	if err != nil {
		return err
	}
	if _, err := firmware.ParseCatalog(catalog); err != nil {
		return task.Error(err.Error())
	}
	if err := CheckMixerStaging(filepath.Join(root, "target", "mixer")); err != nil {
		return err
	}
	if err := CheckZig(); err != nil {
		return err
	}
	targetDir := filepath.Join(root, "target", "mixer-build")
	steps := []struct {
		say  string
		args []string
	}{
		{"Building librc2fx_native.so", []string{"--manifest-path", "crates/rc2fx-native/Cargo.toml", "--lib"}},
		{"Building rc2fxd", []string{"-p", "rc2fx-device", "--features", "native-pipeline", "--bin", "rc2fxd"}},
		{"Building rc2fx-supervisor", []string{"-p", "rc2fx-supervisor", "--bin", "rc2fx-supervisor"}},
		{"Building firmware profile checker", []string{"-p", "rc2fx-firmware", "--bin", "rc2fx-firmware"}},
	}
	for _, step := range steps {
		fmt.Println(step.say)
		if err := task.Run(zigbuild(root, targetDir, step.args...)); err != nil {
			return err
		}
	}
	gui, err := buildGUIModule(root, targetDir)
	if err != nil {
		return err
	}
	recorder, err := BuildJackRecorder(root, targetDir)
	if err != nil {
		return err
	}

	release := filepath.Join(targetDir, Target, "release")
	// Staged beside target/mixer and swapped in only if something changed, so
	// an unchanged build does not make the desktop app embed it again.
	staging := filepath.Join(root, "target", "mixer")
	out := filepath.Join(root, "target", "mixer.new")
	_ = os.RemoveAll(out)
	if err := os.MkdirAll(out, 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	var sums strings.Builder
	for _, binary := range []struct {
		name   string
		shared bool
	}{
		{"librc2fx_native.so", true},
		{"rc2fxd", false},
		{"rc2fx-supervisor", false},
		{"rc2fx-firmware", false},
	} {
		file := filepath.Join(out, binary.name)
		if err := copyFile(filepath.Join(release, binary.name), file); err != nil {
			return task.Errorf("%s: %s", binary.name, task.IOError(err))
		}
		if err := elfcheck.Check(file, binary.shared); err != nil {
			return err
		}
		data, err := os.ReadFile(file)
		if err != nil {
			return task.Error(task.IOError(err))
		}
		fmt.Fprintf(&sums, "%s  %s\n", task.Sha256(data), binary.name)
	}
	profiles := task.CRLFToLF(catalog)
	if err := os.WriteFile(filepath.Join(out, "firmware-profiles.json"), []byte(profiles), 0o666); err != nil {
		return task.Error(task.IOError(err))
	}
	fmt.Fprintf(&sums, "%s  firmware-profiles.json\n", task.Sha256([]byte(profiles)))
	file := filepath.Join(out, GUIModule)
	if err := copyFile(gui, file); err != nil {
		return task.Errorf("%s: %s", GUIModule, task.IOError(err))
	}
	if err := elfcheck.Check(file, true); err != nil {
		return err
	}
	data, err := os.ReadFile(file)
	if err != nil {
		return task.Error(task.IOError(err))
	}
	fmt.Fprintf(&sums, "%s  %s\n", task.Sha256(data), GUIModule)
	for _, part := range []struct {
		source, name string
		shared       bool
	}{
		{recorder[0], JackerrModule, true},
		{recorder[1], JackerrSnapshot, false},
	} {
		file := filepath.Join(out, part.name)
		if err := copyFile(part.source, file); err != nil {
			return task.Errorf("%s: %s", part.name, task.IOError(err))
		}
		if err := elfcheck.Check(file, part.shared); err != nil {
			return err
		}
		data, err := os.ReadFile(file)
		if err != nil {
			return task.Error(task.IOError(err))
		}
		fmt.Fprintf(&sums, "%s  %s\n", task.Sha256(data), part.name)
	}
	// The runtime's release identity, pinned last; the installer refuses a
	// runtime whose manifest differs from config/mixer-release.json.
	manifest, err := os.ReadFile(filepath.Join(root, "config", "mixer-release.json"))
	if err != nil {
		return task.Errorf("config/mixer-release.json: %s", task.IOError(err))
	}
	releaseText := task.CRLFToLF(string(manifest))
	if _, err := core.ParseMixerRelease([]byte(releaseText)); err != nil {
		return task.Error(err.Error())
	}
	if err := os.WriteFile(filepath.Join(out, "mixer-release.json"), []byte(releaseText), 0o666); err != nil {
		return task.Error(task.IOError(err))
	}
	fmt.Fprintf(&sums, "%s  mixer-release.json\n", task.Sha256([]byte(releaseText)))
	if err := os.WriteFile(filepath.Join(out, "SHA256SUMS"), []byte(sums.String()), 0o666); err != nil {
		return task.Error(task.IOError(err))
	}
	if SameFiles(out, staging) {
		if err := os.RemoveAll(out); err != nil {
			return task.Error(task.IOError(err))
		}
		fmt.Printf("Mixer build unchanged in target/mixer/\n%s\n", sums.String())
	} else {
		_ = os.RemoveAll(staging)
		if err := os.Rename(out, staging); err != nil {
			return task.Errorf("target/mixer: %s", task.IOError(err))
		}
		fmt.Printf("Mixer build ready in target/mixer/\n%s\n", sums.String())
	}
	return nil
}

// copyFile is fs::copy: the bytes and the permissions.
func copyFile(from, to string) error {
	data, err := os.ReadFile(from)
	if err != nil {
		return err
	}
	info, err := os.Stat(from)
	if err != nil {
		return err
	}
	if err := os.WriteFile(to, data, info.Mode().Perm()); err != nil {
		return err
	}
	return os.Chmod(to, info.Mode().Perm())
}

// SameFiles reports whether directories a and b hold the same files with the
// same bytes.
func SameFiles(a, b string) bool {
	type entry struct {
		name string
		data []byte
	}
	list := func(dir string) ([]entry, bool) {
		entries, err := os.ReadDir(dir)
		if err != nil {
			return nil, false
		}
		var files []entry
		for _, e := range entries {
			data, err := os.ReadFile(filepath.Join(dir, e.Name()))
			if err != nil {
				return nil, false
			}
			files = append(files, entry{e.Name(), data})
		}
		sort.Slice(files, func(i, j int) bool {
			if files[i].name != files[j].name {
				return files[i].name < files[j].name
			}
			return bytes.Compare(files[i].data, files[j].data) < 0
		})
		return files, true
	}
	left, okLeft := list(a)
	right, okRight := list(b)
	if !okLeft || !okRight || len(left) != len(right) {
		return false
	}
	for i := range left {
		if left[i].name != right[i].name || !bytes.Equal(left[i].data, right[i].data) {
			return false
		}
	}
	return true
}

// GUIModule is the GUI's partial GE2D conversion module (LD_AUDIT; started
// by `rc2fx.sh gui-launch`).
const GUIModule = "librc2fx_gui.so"

// buildGUIModule builds the partial-conversion module from
// `crates/rc2fx-gui/gui_pacing.c` with the three-job rectangle planner, as
// the Pro II profile was measured. The source is passed by its absolute
// path, so the debug info names it.
func buildGUIModule(root, targetDir string) (string, error) {
	fmt.Printf("Building %s\n", GUIModule)
	dir := filepath.Join(targetDir, "gui")
	if err := os.MkdirAll(dir, 0o777); err != nil {
		return "", task.Error(task.IOError(err))
	}
	out := filepath.Join(dir, GUIModule)
	command := exec.Command("zig", "cc",
		"-target", "aarch64-linux-gnu.2.35",
		"-mcpu=cortex_a53",
		"-mno-outline-atomics",
		"-std=c11",
		"-O2",
		"-g",
		"-fPIC",
		"-shared",
		"-fvisibility=hidden",
		"-fno-omit-frame-pointer",
		"-mbranch-protection=standard",
		"-Wall",
		"-Wextra",
		"-Werror",
		"-DRC2FX_PACING_DIRTY",
		"-DRC2FX_PACING_PARTIAL",
		"-DRC2FX_GUI_RECT_PLAN3",
		filepath.Join(root, "crates", "rc2fx-gui", "gui_pacing.c"),
		"-o", out,
	)
	command.Dir = root
	if err := task.Run(command); err != nil {
		return "", err
	}
	return out, nil
}

// The JACK error recorder and its snapshot reader
// (`crates/rc2fx-native/jackerr`).
const (
	JackerrModule   = "librc2fx_jackerr.so"
	JackerrSnapshot = "rc2fx_jackerr_snapshot"
)

// BuildJackRecorder builds the recorder and its snapshot reader into
// targetDir/jackerr and checks both.
func BuildJackRecorder(root, targetDir string) ([2]string, error) {
	directory := filepath.Join(targetDir, "jackerr")
	if err := os.MkdirAll(directory, 0o777); err != nil {
		return [2]string{}, task.Error(task.IOError(err))
	}
	outputs := [2]string{filepath.Join(directory, JackerrModule), filepath.Join(directory, JackerrSnapshot)}
	for i, part := range []struct {
		source string
		shared bool
	}{
		{"rc2fx_jackerr.c", true},
		{"rc2fx_jackerr_snapshot.c", false},
	} {
		args := []string{
			"cc",
			"-target", "aarch64-linux-gnu.2.35",
			"-mcpu=cortex_a53",
			"-mno-outline-atomics",
			"-std=c11",
			"-O2",
			"-g",
			"-Wall",
			"-Wextra",
			"-Werror",
		}
		if part.shared {
			args = append(args, "-shared", "-fPIC", "-fvisibility=hidden")
		} else {
			args = append(args, "-fPIE", "-pie")
		}
		args = append(args, filepath.Join(root, "crates", "rc2fx-native", "jackerr", part.source), "-o", outputs[i])
		command := exec.Command("zig", args...)
		command.Dir = root
		if err := task.Run(command); err != nil {
			return [2]string{}, err
		}
		if err := elfcheck.Check(outputs[i], part.shared); err != nil {
			return [2]string{}, err
		}
	}
	return outputs, nil
}
