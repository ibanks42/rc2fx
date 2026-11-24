package licenses

import (
	"bytes"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/deps"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

const (
	// Covered by RC2FX's own license.
	mainModule = "github.com/ibanks42/rc2fx/desktop"
	// Covered by libgouikit's THIRD_PARTY.md.
	gouikitModule = "github.com/ibanks42/goui-kit"
)

// GoApp collects notices for the Go desktop app for the given target and build
// tags: the Go stdlib, linked Go modules, and libgouikit under
// desktop/third_party/goui-kit.
func GoApp(root, goos, goarch string, tags []string) ([]Section, error) {
	desktop := filepath.Join(root, "desktop")
	goEnv := func(args ...string) *exec.Cmd {
		command := exec.Command("go", args...)
		command.Dir = desktop
		command.Env = append(os.Environ(), "GOOS="+goos, "GOARCH="+goarch, "CGO_ENABLED=1")
		return command
	}
	output, err := goEnv("env", "GOROOT").Output()
	if err != nil {
		return nil, task.Errorf("could not run go env GOROOT: %s", commandError(err))
	}
	goroot := strings.TrimSpace(string(output))
	std, err := os.ReadFile(filepath.Join(goroot, "LICENSE"))
	if err != nil {
		return nil, task.Errorf("%s: %s", filepath.Join(goroot, "LICENSE"), task.IOError(err))
	}
	sections := []Section{{"Go standard library (BSD-3-Clause)", string(std)}}

	args := []string{"list", "-deps", "-f", "{{with .Module}}{{.Path}}\t{{.Version}}\t{{.Dir}}{{end}}"}
	if len(tags) > 0 {
		args = append(args, "-tags", strings.Join(tags, ","))
	}
	args = append(args, "./cmd/rc2fx")
	command := goEnv(args...)
	var stderr bytes.Buffer
	command.Stderr = &stderr
	output, err = command.Output()
	if err != nil {
		return nil, task.Errorf("go list failed for cmd/rc2fx: %s%s", commandError(err), strings.TrimSpace(stderr.String()))
	}
	groups := Groups{}
	seen := map[string]bool{}
	for _, line := range strings.Split(string(output), "\n") {
		if line == "" || seen[line] {
			continue
		}
		seen[line] = true
		parts := strings.SplitN(line, "\t", 3)
		if len(parts) != 3 {
			continue
		}
		path, version, dir := parts[0], parts[1], parts[2]
		if path == mainModule || path == gouikitModule {
			continue
		}
		user := path
		if version != "" {
			user += " " + version
		}
		files, err := licenseFiles(dir)
		if err != nil {
			return nil, err
		}
		if len(files) == 0 {
			return nil, task.Errorf("Go module %s has no license file in %s", user, dir)
		}
		for _, file := range files {
			text, err := os.ReadFile(filepath.Join(dir, file))
			if err != nil {
				return nil, task.Errorf("%s: %s", filepath.Join(dir, file), task.IOError(err))
			}
			if !utf8.Valid(text) {
				return nil, task.Errorf("%s: stream did not contain valid UTF-8", filepath.Join(dir, file))
			}
			groups.add("Go modules", string(text), user)
		}
	}
	sections = append(sections, groups.Sections("Dependencies: ")...)

	gouikit, err := GouiKit(deps.GouiKitDir(root))
	if err != nil {
		return nil, err
	}
	return append(sections, gouikit...), nil
}

func licenseFiles(dir string) ([]string, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, task.Errorf("%s: %s", dir, task.IOError(err))
	}
	var out []string
	for _, entry := range entries {
		upper := strings.ToUpper(entry.Name())
		if !entry.Type().IsRegular() {
			continue
		}
		for _, prefix := range []string{"LICENSE", "LICENCE", "COPYING", "NOTICE", "PATENTS"} {
			if strings.HasPrefix(upper, prefix) {
				out = append(out, entry.Name())
				break
			}
		}
	}
	sort.Strings(out)
	return out, nil
}

// GouiKit reads THIRD_PARTY.md and all license texts under licences/texts/
// from an unpacked goui-kit download, deduplicating identical texts.
func GouiKit(dir string) ([]Section, error) {
	listing, err := os.ReadFile(filepath.Join(dir, "THIRD_PARTY.md"))
	if err != nil {
		return nil, task.Errorf("%s: %s; link desktop/third_party/goui-kit to the goui-kit download (desktop/README.md)",
			filepath.Join(dir, "THIRD_PARTY.md"), task.IOError(err))
	}
	if !utf8.Valid(listing) {
		return nil, task.Errorf("%s: stream did not contain valid UTF-8", filepath.Join(dir, "THIRD_PARTY.md"))
	}
	sections := []Section{{"GPUI-kit library (libgouikit): third-party software", string(listing)}}
	texts := filepath.Join(dir, "licences", "texts")
	crates, err := os.ReadDir(texts)
	if err != nil {
		return nil, task.Errorf("%s: %s", texts, task.IOError(err))
	}
	groups := Groups{}
	for _, crate := range crates {
		if !crate.IsDir() {
			continue
		}
		files, err := os.ReadDir(filepath.Join(texts, crate.Name()))
		if err != nil {
			return nil, task.Errorf("%s: %s", filepath.Join(texts, crate.Name()), task.IOError(err))
		}
		for _, file := range files {
			if !file.Type().IsRegular() {
				continue
			}
			path := filepath.Join(texts, crate.Name(), file.Name())
			text, err := os.ReadFile(path)
			if err != nil {
				return nil, task.Errorf("%s: %s", path, task.IOError(err))
			}
			if !utf8.Valid(text) {
				return nil, task.Errorf("%s: stream did not contain valid UTF-8", path)
			}
			if strings.TrimSpace(string(text)) == "" {
				continue
			}
			groups.add("libgouikit license texts", string(text), crate.Name()+" ("+file.Name()+")")
		}
	}
	if len(groups) == 0 {
		return nil, task.Errorf("%s holds no license texts", texts)
	}
	return append(sections, groups.Sections("GPUI-kit library: ")...), nil
}

func commandError(err error) string {
	if exit, ok := err.(*exec.ExitError); ok {
		return task.Status(exit.ProcessState) + " "
	}
	return task.IOError(err) + " "
}
