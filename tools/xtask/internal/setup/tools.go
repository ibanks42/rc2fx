package setup

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/deps"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/toolpins"
)

// CargoAboutVersion is the cargo-about version pinned by the release workflows.
const CargoAboutVersion = "0.8.4"

// packagingTools installs what `go -C tools/xtask run . package` needs beyond the
// libraries: cargo-about, vpk, and on Linux linuxdeploy, patchelf, mksquashfs,
// and the AppImage runtime notices, at the versions the release workflows use.
func packagingTools(root string) error {
	if err := installCargoAbout(); err != nil {
		return err
	}
	tools := deps.ToolsDir(root)
	if _, err := deps.FetchVPK(tools); err != nil {
		return err
	}
	if runtime.GOOS != "linux" {
		return nil
	}
	for _, tool := range []toolpins.Tool{toolpins.Linuxdeploy, toolpins.Patchelf} {
		if _, err := toolpins.Find(tool); err != nil {
			return err
		}
	}
	if err := installMksquashfs(); err != nil {
		return err
	}
	return prepareNotices(root, tools)
}

// installPinnedTools installs the tools mise.toml pins when mise is
// installed. Without mise it lists the pinned tools that are missing; the
// build fails later on the ones it needs, and lint skips the rest.
func installPinnedTools(root string) error {
	if _, err := exec.LookPath("mise"); err == nil {
		fmt.Println("Installing the tools in mise.toml with mise")
		trust := exec.Command("mise", "trust", "--quiet", filepath.Join(root, "mise.toml"))
		if err := task.Run(trust); err != nil {
			return err
		}
		install := exec.Command("mise", "install", "--yes")
		install.Dir = root
		return task.Run(install)
	}
	tools := []toolpins.Tool{toolpins.Just, toolpins.Zig, toolpins.Ruff, toolpins.Shellcheck, toolpins.Shfmt,
		toolpins.Actionlint, toolpins.ClangFormat}
	if runtime.GOOS == "linux" {
		tools = append(tools, toolpins.Linuxdeploy, toolpins.Patchelf)
	}
	var missing []string
	for _, tool := range tools {
		if _, err := toolpins.Find(tool); err != nil {
			missing = append(missing, "  "+err.Error())
		}
	}
	if len(missing) > 0 {
		fmt.Printf("mise is not installed (https://mise.jdx.dev), and these pinned tools are missing:\n%s\n",
			strings.Join(missing, "\n"))
	}
	return nil
}

func installCargoAbout() error {
	// `cargo about --version` prints "cargo-about 0.8.4".
	output, err := exec.Command("cargo", "about", "--version").Output()
	if err == nil && strings.TrimSpace(string(output)) == "cargo-about "+CargoAboutVersion {
		fmt.Printf("Using cargo-about %s\n", CargoAboutVersion)
		return nil
	}
	fmt.Printf("Installing cargo-about %s\n", CargoAboutVersion)
	return task.Run(exec.Command("cargo", "install", "--locked", "cargo-about", "--version", CargoAboutVersion))
}

// installMksquashfs installs squashfs-tools with the system package manager
// when mksquashfs is missing; vpk builds the AppImage with it.
func installMksquashfs() error {
	if _, err := exec.LookPath("mksquashfs"); err == nil {
		fmt.Println("Using mksquashfs from PATH")
		return nil
	}
	managers := []struct {
		program string
		args    []string
	}{
		{"dnf", []string{"install", "-y", "squashfs-tools"}},
		{"apt-get", []string{"install", "-y", "squashfs-tools"}},
		{"pacman", []string{"-S", "--needed", "--noconfirm", "squashfs-tools"}},
		{"zypper", []string{"install", "-y", "squashfs"}},
	}
	for _, manager := range managers {
		if _, err := exec.LookPath(manager.program); err != nil {
			continue
		}
		args := append([]string{manager.program}, manager.args...)
		if os.Geteuid() != 0 {
			args = append([]string{"sudo"}, args...)
		}
		fmt.Printf("Installing mksquashfs: %s\n", strings.Join(args, " "))
		return task.Run(exec.Command(args[0], args[1:]...))
	}
	return task.Error("mksquashfs is not on PATH and no known package manager was found; install squashfs-tools, then rerun setup")
}

// prepareNotices writes the AppImage runtime's license notices and source
// archive from the vpk package that setup verified, caching the other downloads.
func prepareNotices(root, tools string) error {
	vpkPackage, err := deps.VPKPackage(tools)
	if err != nil {
		return err
	}
	scratch, err := os.MkdirTemp(tools, ".notices.")
	if err != nil {
		return task.Error(task.IOError(err))
	}
	defer os.RemoveAll(scratch)
	fmt.Println("Collecting the AppImage runtime's license notices and sources")
	command := exec.Command("python3", "-I", filepath.Join(root, ".github", "scripts", "prepare-appimage-notices.py"),
		"--root", root,
		"--cache", filepath.Join(tools, "notices-cache"),
		"--vpk-package", vpkPackage)
	command.Env = append(os.Environ(), "TMPDIR="+scratch)
	return task.Run(command)
}
