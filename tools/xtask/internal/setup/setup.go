// Package setup handles one-time toolchain setup and the mixer cross-build.
// It uses zig as the C compiler and linker (via cargo-zigbuild) so the build
// runs natively on any host OS and links against the mixer's glibc 2.35
// without a separate sysroot.
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

	"github.com/ibanks42/rc2fx/desktop/common"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/deps"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/elfcheck"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/toolpins"
)

// Target is the mixer's Rust target.
const Target = "aarch64-unknown-linux-gnu"

// ZigTarget: firmware 1.7.6 ships glibc 2.35; zig links against exactly that
// version.
const ZigTarget = "aarch64-unknown-linux-gnu.2.35"

// Run installs everything needed to build, package, and run the desktop
// app and to build the mixer side: the goui-kit and Velopack libraries, the
// Rust target, mise.toml's pinned tools (when mise is installed),
// cargo-zigbuild, and the packaging tools, and checks for zig.
func Run() error {
	if err := deps.FetchGouiKit(""); err != nil {
		return err
	}
	if err := deps.FetchVelopack(nil); err != nil {
		return err
	}
	fmt.Printf("Adding the Rust %s target\n", Target)
	if err := task.Run(exec.Command("rustup", "target", "add", Target)); err != nil {
		return err
	}
	if err := installPinnedTools(task.Root()); err != nil {
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
	if err := packagingTools(task.Root()); err != nil {
		return err
	}
	fmt.Println("Setup complete. Run the app with `just app`, package it with `just package`, and build the mixer side with `just mixer-build`.")
	return nil
}

func succeeds(command *exec.Cmd) bool {
	command.Stdout = nil
	command.Stderr = nil
	return command.Run() == nil
}

// CheckZig requires the zig mise.toml pins (or another on PATH) and says
// which one is used.
func CheckZig() error {
	zig, err := toolpins.Find(toolpins.Zig)
	if err != nil {
		return err
	}
	output, _ := exec.Command(zig, "version").Output()
	fmt.Printf("Using zig %s\n", strings.TrimSpace(strings.ToValidUTF8(string(output), "\uFFFD")))
	return nil
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
		return task.Error("target/mixer is a link to another bundle; refusing to replace it. Use `go -C tools/xtask run . mixer install` to deploy that bundle, or move the link aside to rebuild.")
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
		{"Building librc2fx_native.so", []string{"-p", "rc2fx-native", "--lib"}},
		{"Building rc2fxd", []string{"-p", "rc2fx-device", "--bin", "rc2fxd"}},
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
	sources := payloadSources(release, gui, recorder, []byte(task.CRLFToLF(catalog)), []byte(releaseText))
	if err := checkSources(sources); err != nil {
		return err
	}
	// Each file is read once: checked, written and hashed from the same
	// bytes, in the bundle's SHA256SUMS order.
	var sums strings.Builder
	for _, name := range embed.Pinned {
		source := sources[name]
		file := filepath.Join(out, name)
		data, mode := source.data, fs.FileMode(0o666)
		if source.path != "" {
			info, err := os.Stat(source.path)
			if err == nil {
				data, err = os.ReadFile(source.path)
			}
			if err != nil {
				return task.Errorf("%s: %s", name, task.IOError(err))
			}
			if err := elfcheck.CheckData(file, data, source.shared); err != nil {
				return err
			}
			mode = info.Mode().Perm()
		}
		if err := os.WriteFile(file, data, mode); err != nil {
			return task.Errorf("%s: %s", name, task.IOError(err))
		}
		if source.path != "" {
			// The source's permissions, whatever the umask.
			if err := os.Chmod(file, mode); err != nil {
				return task.Errorf("%s: %s", name, task.IOError(err))
			}
		}
		fmt.Fprintf(&sums, "%s  %s\n", common.SHA256Hex(data), name)
	}
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

// payloadSource is where BuildMixer takes one pinned payload file from: a
// built ELF at path, checked against the mixer ABI baseline (a shared
// library when shared), or data it already holds.
type payloadSource struct {
	path   string
	shared bool
	data   []byte
}

// payloadSources is where BuildMixer takes each file in embed.Pinned from:
// release is the cargo release directory, gui the GUI module and recorder
// the JACK error recorder and its snapshot reader.
func payloadSources(release, gui string, recorder [2]string, profiles, manifest []byte) map[string]payloadSource {
	return map[string]payloadSource{
		"librc2fx_native.so":     {path: filepath.Join(release, "librc2fx_native.so"), shared: true},
		"rc2fxd":                 {path: filepath.Join(release, "rc2fxd")},
		"rc2fx-supervisor":       {path: filepath.Join(release, "rc2fx-supervisor")},
		"rc2fx-firmware":         {path: filepath.Join(release, "rc2fx-firmware")},
		"firmware-profiles.json": {data: profiles},
		GUIModule:                {path: gui, shared: true},
		JackerrModule:            {path: recorder[0], shared: true},
		JackerrSnapshot:          {path: recorder[1]},
		embed.ReleaseManifest:    {data: manifest},
	}
}

// checkSources requires a source for exactly the files in embed.Pinned, and
// a built ELF for exactly those in embed.Binaries, so a payload file added
// on one side stops the build instead of producing a bundle the installer
// refuses.
func checkSources(sources map[string]payloadSource) error {
	if len(sources) != len(embed.Pinned) {
		return task.Errorf("the mixer build makes %d files; the payload pins %d", len(sources), len(embed.Pinned))
	}
	for _, name := range embed.Pinned {
		source, ok := sources[name]
		if !ok {
			return task.Errorf("the mixer build has no source for %s", name)
		}
		if (source.path != "") != embed.IsBinary(name) {
			return task.Errorf("the mixer build and the payload disagree on whether %s is a binary", name)
		}
	}
	return nil
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
		// Names in a directory are unique, so the name alone orders them.
		sort.Slice(files, func(i, j int) bool { return files[i].name < files[j].name })
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
// `mixer/gui/gui_pacing.c` (the three-job rectangle planner, as the
// Pro II profile was measured). The source is passed by its absolute path,
// so the debug info names it.
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
		filepath.Join(root, "mixer", "gui", "gui_pacing.c"),
		"-o", out,
	)
	command.Dir = root
	if err := task.Run(command); err != nil {
		return "", err
	}
	return out, nil
}

// The JACK error recorder and its snapshot reader
// (`mixer/jackerr`).
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
		args = append(args, filepath.Join(root, "mixer", "jackerr", part.source), "-o", outputs[i])
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
