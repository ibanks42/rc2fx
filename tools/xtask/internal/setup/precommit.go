package setup

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

// rustManifests are the Rust workspaces rustfmt checks. The benchmark and
// the standalone tools are workspaces of their own, which `--all` skips.
var rustManifests = []string{"Cargo.toml", "crates/rc2fx-bench/Cargo.toml", "tools/src_fast_check/Cargo.toml"}

// PreCommit implements `pre-commit`, invoked by the git hook that `just setup`
// enables. It refuses a commit when Rust code fails rustfmt or staged Go files
// fail gofmt, matching CI. Clippy, go vet, and tests take minutes, so they
// stay with `just test`.
func PreCommit() error {
	root := task.Root()
	// rustfmt checks the working tree, so unstaged edits count too.
	rust := exec.Command("git", "-C", root, "diff", "--cached", "--quiet", "--", "*.rs")
	if err := rust.Run(); err != nil {
		if _, ok := err.(*exec.ExitError); !ok {
			return task.Errorf("git diff: %s", err)
		}
		for _, manifest := range rustManifests {
			check := exec.Command("cargo", "fmt", "--all", "--manifest-path", manifest, "--", "--check")
			check.Dir = root
			if err := check.Run(); err != nil {
				return task.Errorf("pre-commit: Rust code is not formatted. Run `cargo fmt --all --manifest-path %s`, stage the changes and commit again.", manifest)
			}
		}
	}

	// gofmt checks the staged Go files as they are in the working tree.
	staged, err := exec.Command("git", "-C", root, "diff", "--cached", "--name-only", "-z", "--diff-filter=ACMR", "--", "*.go").Output()
	if err != nil {
		return task.Errorf("git diff: %s", err)
	}
	var files []string
	for _, name := range strings.Split(string(staged), "\x00") {
		if name != "" {
			files = append(files, filepath.Join(root, filepath.FromSlash(name)))
		}
	}
	if len(files) == 0 {
		return nil
	}
	gofmt := exec.Command("gofmt", append([]string{"-l"}, files...)...)
	gofmt.Stderr = os.Stderr
	unformatted, err := gofmt.Output()
	if err != nil {
		return task.Errorf("gofmt: %s", err)
	}
	if list := strings.TrimSpace(string(unformatted)); list != "" {
		return task.Error(fmt.Sprintf("pre-commit: Go code is not formatted. Run `gofmt -w` on these, stage the changes and commit again:\n%s", list))
	}
	return nil
}
