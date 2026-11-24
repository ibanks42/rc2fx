// Package toolpins finds the command-line tools pinned in mise.toml. mise
// installs exactly those versions (`mise install`), but it is optional: a
// tool on PATH from anywhere else is used too, with a warning when its
// version differs from the pin.
package toolpins

import (
	"bufio"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"sync"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

// Tool is a command pinned in mise.toml.
type Tool struct {
	// Key is the tool's name in mise.toml's [tools] table.
	Key string
	// Command is the program it installs.
	Command string
	// VersionArgs print the version; none skips the version check.
	VersionArgs []string
	// Install says how to get it without mise.
	Install string
}

var (
	Just        = Tool{"just", "just", []string{"--version"}, "https://just.systems"}
	Zig         = Tool{"zig", "zig", []string{"version"}, "https://ziglang.org/download/"}
	Ruff        = Tool{"ruff", "ruff", []string{"--version"}, "https://docs.astral.sh/ruff/installation/"}
	Shellcheck  = Tool{"shellcheck", "shellcheck", []string{"--version"}, "apt/dnf/brew install shellcheck, or winget install koalaman.shellcheck"}
	Shfmt       = Tool{"shfmt", "shfmt", []string{"--version"}, "brew install shfmt, or go install mvdan.cc/sh/v3/cmd/shfmt@latest"}
	Actionlint  = Tool{"actionlint", "actionlint", []string{"--version"}, "brew install actionlint, or go install github.com/rhysd/actionlint/cmd/actionlint@latest"}
	ClangFormat = Tool{"conda:clang-format", "clang-format", []string{"--version"}, "apt/dnf install clang-format, brew install clang-format, or winget install LLVM.LLVM"}
	Patchelf    = Tool{"github:NixOS/patchelf", "patchelf", []string{"--version"}, "apt/dnf install patchelf"}
	// linuxdeploy is an AppImage whose version output names no release.
	Linuxdeploy = Tool{"github:linuxdeploy/linuxdeploy", "linuxdeploy", nil, "https://github.com/linuxdeploy/linuxdeploy/releases"}
)

var (
	pinsOnce sync.Once
	pins     map[string]string
	pinsErr  error
	warned   sync.Map
)

var (
	tableLine = regexp.MustCompile(`^\s*\[([^\]]+)\]\s*$`)
	pinLine   = regexp.MustCompile(`^\s*("[^"]+"|[A-Za-z0-9_.:/-]+)\s*=\s*(?:"([^"]+)"|\{.*\bversion\s*=\s*"([^"]+)".*\})\s*$`)
)

// Pins reads and caches tool versions from the [tools] table of mise.toml.
func Pins() (map[string]string, error) {
	pinsOnce.Do(func() {
		path := filepath.Join(task.Root(), "mise.toml")
		file, err := os.Open(path)
		if err != nil {
			pinsErr = task.Errorf("%s: %s", path, task.IOError(err))
			return
		}
		defer file.Close()
		pins = map[string]string{}
		table := ""
		scanner := bufio.NewScanner(file)
		for scanner.Scan() {
			line := scanner.Text()
			if strings.HasPrefix(strings.TrimSpace(line), "#") {
				continue
			}
			if m := tableLine.FindStringSubmatch(line); m != nil {
				table = strings.TrimSpace(m[1])
				continue
			}
			if table != "tools" {
				continue
			}
			if m := pinLine.FindStringSubmatch(line); m != nil {
				version := m[2]
				if version == "" {
					version = m[3]
				}
				pins[strings.Trim(m[1], `"`)] = version
			}
		}
		if err := scanner.Err(); err != nil {
			pinsErr = task.Errorf("%s: %s", path, err)
		}
	})
	return pins, pinsErr
}

// Pin is the version mise.toml pins for tool.
func Pin(tool Tool) (string, error) {
	pins, err := Pins()
	if err != nil {
		return "", err
	}
	version, ok := pins[tool.Key]
	if !ok {
		return "", task.Errorf("mise.toml pins no %s", tool.Key)
	}
	return version, nil
}

// Find returns the path of tool's command: from PATH, or from mise when mise
// has it installed but is not activated in this shell (prepending its
// directory to PATH so child processes find it too). It warns once per command
// when the version output does not contain the pin.
func Find(tool Tool) (string, error) {
	pin, err := Pin(tool)
	if err != nil {
		return "", err
	}
	path, err := exec.LookPath(tool.Command)
	if err != nil {
		// Found through mise, so prepend its directory to PATH for
		// subprocesses too (cargo-zigbuild runs zig, linuxdeploy runs patchelf).
		if path = miseWhich(tool.Command); path != "" {
			os.Setenv("PATH", filepath.Dir(path)+string(os.PathListSeparator)+os.Getenv("PATH"))
		}
	}
	if path == "" {
		return "", task.Errorf("%s %s is not installed. Run `mise install` in the repository, or install it yourself: %s",
			tool.Command, pin, tool.Install)
	}
	if len(tool.VersionArgs) > 0 {
		output, _ := exec.Command(path, tool.VersionArgs...).CombinedOutput()
		if !strings.Contains(string(output), pin) {
			if _, seen := warned.LoadOrStore(tool.Command, true); !seen {
				fmt.Fprintf(os.Stderr, "warning: %s is not version %s (mise.toml); its output may differ from CI's. Run `mise install` to get the pinned version.\n",
					path, pin)
			}
		}
	}
	return path, nil
}

// miseWhich returns the path mise installed command at, or "" if mise is not on
// PATH or has not installed command.
func miseWhich(command string) string {
	if _, err := exec.LookPath("mise"); err != nil {
		return ""
	}
	which := exec.Command("mise", "which", command)
	which.Dir = task.Root()
	output, err := which.Output()
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(output))
}
