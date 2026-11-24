// Package lint checks (and with --fix, formats and applies safe ruff lint
// fixes for) every language in the repository with one command: Go (gofmt),
// Rust (rustfmt), Python (ruff), C (clang-format), shell (shfmt and
// shellcheck), and GitHub workflows (actionlint). Go and Rust use gofmt and
// cargo fmt from PATH; the other tools come from mise.toml (see package
// toolpins). A missing tool is skipped with a warning, except in CI (when CI is
// set), where it fails.
package lint

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path"
	"strings"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/toolpins"
)

// rustManifests are the Rust workspaces rustfmt checks. The benchmark and
// the standalone tools are workspaces of their own, which `--all` skips.
var rustManifests = []string{"Cargo.toml", "crates/rc2fx-bench/Cargo.toml", "tools/src_fast_check/Cargo.toml"}

const usage = "usage: go -C tools/xtask run . lint [--fix] [--staged]\n" +
	"--fix formats files and applies safe lint fixes; --staged checks only the files staged for commit."

type options struct {
	fix, staged bool
}

// language is one language's files and the tools that check them.
type language struct {
	name  string
	match func(name string, head []byte) bool
	// whole marks languages whose runner checks whole workspaces rather
	// than individual selected files (rustfmt).
	whole bool
	run   func(o options, files []string) error
}

var languages = []language{
	{name: "Go", match: extension(".go"), run: runGo},
	{name: "Rust", match: extension(".rs"), whole: true, run: runRust},
	{name: "Python", match: extension(".py"), run: runPython},
	{name: "C", match: extension(".c", ".h"), run: runC},
	{name: "shell", match: isShell, run: runShell},
	{name: "GitHub workflows", match: isWorkflow, run: runWorkflows},
}

// Run is `lint`.
func Run(args []string) error {
	var o options
	for _, arg := range args {
		switch arg {
		case "--fix":
			o.fix = true
		case "--staged":
			o.staged = true
		default:
			return task.Error(usage)
		}
	}
	files, err := candidates(o.staged)
	if err != nil {
		return err
	}
	strict := os.Getenv("CI") != ""
	var failed, skipped []string
	for _, lang := range languages {
		var selected []string
		for _, name := range files {
			if lang.match(name, head(name)) {
				selected = append(selected, name)
			}
		}
		if len(selected) == 0 {
			continue
		}
		err := lang.run(o, selected)
		var missing missingTool
		switch {
		case err == nil:
		case asMissing(err, &missing) && !strict:
			fmt.Fprintf(os.Stderr, "lint: skipping %s: %s\n", lang.name, missing.err)
			skipped = append(skipped, lang.name)
		default:
			fmt.Fprintf(os.Stderr, "lint: %s: %s\n", lang.name, err)
			failed = append(failed, lang.name)
		}
	}
	if len(failed) > 0 {
		hint := ""
		if !o.fix {
			hint = "; `just fmt` fixes formatting"
		}
		return task.Errorf("lint failed for %s%s", strings.Join(failed, ", "), hint)
	}
	if len(skipped) > 0 {
		fmt.Fprintf(os.Stderr, "lint: passed, but skipped %s (tools not installed)\n", strings.Join(skipped, ", "))
	}
	return nil
}

// candidates lists regular files in the repository (tracked and non-ignored
// untracked), or only staged added/copied/modified/renamed ones, relative to
// the repository root (the working directory).
func candidates(staged bool) ([]string, error) {
	args := []string{"ls-files", "-z", "--cached", "--others", "--exclude-standard"}
	if staged {
		args = []string{"diff", "--cached", "--name-only", "-z", "--diff-filter=ACMR"}
	}
	output, err := exec.Command("git", args...).Output()
	if err != nil {
		return nil, task.Errorf("git %s: %s", args[0], err)
	}
	var files []string
	for _, name := range strings.Split(string(output), "\x00") {
		if name == "" {
			continue
		}
		if info, err := os.Lstat(name); err == nil && info.Mode().IsRegular() {
			files = append(files, name)
		}
	}
	return files, nil
}

func head(name string) []byte {
	file, err := os.Open(name)
	if err != nil {
		return nil
	}
	defer file.Close()
	buffer := make([]byte, 64)
	n, _ := file.Read(buffer)
	return buffer[:n]
}

func extension(exts ...string) func(string, []byte) bool {
	return func(name string, _ []byte) bool {
		for _, ext := range exts {
			if strings.HasSuffix(name, ext) {
				return true
			}
		}
		return false
	}
}

func isShell(name string, head []byte) bool {
	if strings.HasSuffix(name, ".sh") {
		return true
	}
	line, _, _ := bytes.Cut(head, []byte("\n"))
	for _, shebang := range []string{"#!/bin/sh", "#!/bin/bash", "#!/usr/bin/env sh", "#!/usr/bin/env bash"} {
		if string(bytes.TrimSpace(line)) == shebang {
			return true
		}
	}
	return false
}

func isWorkflow(name string, _ []byte) bool {
	return path.Dir(name) == ".github/workflows" && (strings.HasSuffix(name, ".yml") || strings.HasSuffix(name, ".yaml"))
}

// missingTool marks a tool that is not installed.
type missingTool struct{ err error }

func (m missingTool) Error() string { return m.err.Error() }

func asMissing(err error, target *missingTool) bool {
	m, ok := err.(missingTool)
	if ok {
		*target = m
	}
	return ok
}

func find(tool toolpins.Tool) (string, error) {
	path, err := toolpins.Find(tool)
	if err != nil {
		return "", missingTool{err}
	}
	return path, nil
}

// check runs a checker and fails when it exits non-zero; its output is
// passed through.
func check(program string, args ...string) error {
	return task.Run(exec.Command(program, args...))
}

// listed runs a command that prints one unformatted file per line and fails
// when it prints any.
func listed(what, program string, args ...string) error {
	command := exec.Command(program, args...)
	command.Stderr = os.Stderr
	output, err := command.Output()
	if err != nil {
		return task.Errorf("%s: %s", program, err)
	}
	if list := strings.TrimSpace(string(output)); list != "" {
		return task.Errorf("%s:\n%s", what, list)
	}
	return nil
}

func runGo(o options, files []string) error {
	if o.fix {
		return check("gofmt", append([]string{"-w"}, files...)...)
	}
	return listed("not gofmt-formatted", "gofmt", append([]string{"-l"}, files...)...)
}

func runRust(o options, _ []string) error {
	if _, err := exec.LookPath("cargo"); err != nil {
		return missingTool{task.Error("cargo is not on PATH")}
	}
	for _, manifest := range rustManifests {
		args := []string{"fmt", "--all", "--manifest-path", manifest}
		if !o.fix {
			args = append(args, "--", "--check")
		}
		if err := check("cargo", args...); err != nil {
			return err
		}
	}
	return nil
}

func runPython(o options, files []string) error {
	ruff, err := find(toolpins.Ruff)
	if err != nil {
		return err
	}
	format := []string{"format", "--no-cache"}
	lint := []string{"check", "--no-cache"}
	if o.fix {
		lint = append(lint, "--fix")
	} else {
		format = append(format, "--check")
	}
	// Lint even when formatting fails, to report everything at once.
	return errors.Join(check(ruff, append(format, files...)...), check(ruff, append(lint, files...)...))
}

func runC(o options, files []string) error {
	clangFormat, err := find(toolpins.ClangFormat)
	if err != nil {
		return err
	}
	if o.fix {
		return check(clangFormat, append([]string{"-i"}, files...)...)
	}
	return check(clangFormat, append([]string{"--dry-run", "--Werror"}, files...)...)
}

func runShell(o options, files []string) error {
	shfmt, err := find(toolpins.Shfmt)
	if err != nil {
		return err
	}
	shellcheck, err := find(toolpins.Shellcheck)
	if err != nil {
		return err
	}
	var formatted error
	if o.fix {
		formatted = check(shfmt, append([]string{"-w"}, files...)...)
	} else {
		formatted = listed("not shfmt-formatted", shfmt, append([]string{"-l"}, files...)...)
	}
	return errors.Join(formatted, check(shellcheck, files...))
}

func runWorkflows(_ options, files []string) error {
	actionlint, err := find(toolpins.Actionlint)
	if err != nil {
		return err
	}
	// Pass shellcheck explicitly when found so actionlint checks `run:`
	// scripts even if shellcheck lives in mise rather than on PATH.
	command := exec.Command(actionlint, files...)
	if shellcheck, err := toolpins.Find(toolpins.Shellcheck); err == nil {
		command.Args = append([]string{actionlint, "-shellcheck", shellcheck}, files...)
	}
	return task.Run(command)
}
