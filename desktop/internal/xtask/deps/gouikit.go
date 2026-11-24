package deps

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// GouiKitRepo publishes the goui-kit downloads.
const GouiKitRepo = "ibanks42/goui-kit"

// GouiKitLibrary is the static library every goui-kit download must hold.
const GouiKitLibrary = "libgouikit_static.a"

// stampName is the file inside the unpacked download that records the
// archive's SHA-256.
const stampName = ".archive-sha256"

type gouiKitConfig struct {
	Version string            `json:"version"`
	SHA256  map[string]string `json:"sha256"`
}

// GouiKitDir is where the download is unpacked; desktop/go.mod's replace
// directive points at its go folder.
func GouiKitDir(root string) string {
	return filepath.Join(thirdParty(root), "goui-kit")
}

// gouiKitPlatform returns the download platform name and archive extension
// for a GOOS-GOARCH target (or darwin-universal).
func gouiKitPlatform(target string) (string, string, error) {
	switch target {
	case "linux-amd64":
		return "linux-x86_64", "tar.zst", nil
	case "darwin-universal", "darwin-arm64", "darwin-amd64":
		return "macos-universal", "tar.zst", nil
	case "windows-amd64":
		return "windows-x86_64", "zip", nil
	}
	return "", "", task.Errorf("no goui-kit download for %s", target)
}

// FetchGouiKit downloads the pinned goui-kit release for target (defaulting
// to this machine when empty) into desktop/third_party/goui-kit, skipping
// the download when that folder already holds the same archive. A symlink
// there (a local build) is replaced.
func FetchGouiKit(target string) error {
	root := task.Root()
	if target == "" {
		target = HostTarget()
	}
	platform, ext, err := gouiKitPlatform(target)
	if err != nil {
		return err
	}
	configPath := filepath.Join(root, "config", "goui-kit.json")
	var config gouiKitConfig
	if err := readJSON(configPath, &config); err != nil {
		return err
	}
	if strings.TrimSpace(config.Version) == "" {
		return task.Errorf("%s has no version", configPath)
	}
	name := "goui-kit-" + config.Version + "-" + platform
	tag := "v" + config.Version
	sha := config.SHA256[platform]
	if !validSHA256(sha) {
		return task.Errorf("%s has no SHA-256 for %s; fill in the SHA-256 of %s.%s from https://github.com/%s/releases/tag/%s",
			configPath, platform, name, ext, GouiKitRepo, tag)
	}

	dest := GouiKitDir(root)
	if info, err := os.Lstat(dest); err == nil && info.Mode()&os.ModeSymlink == 0 {
		if stamp, err := os.ReadFile(filepath.Join(dest, stampName)); err == nil && strings.TrimSpace(string(stamp)) == sha {
			fmt.Printf("goui-kit %s is already in %s for %s\n", config.Version, dest, target)
			return nil
		}
	}

	// Stage beside the destination: the archives are large, and the final
	// rename has to stay on one file system.
	if err := os.MkdirAll(thirdParty(root), 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	stage, err := os.MkdirTemp(thirdParty(root), ".fetch-goui-kit.")
	if err != nil {
		return task.Error(task.IOError(err))
	}
	defer os.RemoveAll(stage)

	archive := filepath.Join(stage, name+"."+ext)
	url := fmt.Sprintf("https://github.com/%s/releases/download/%s/%s.%s", GouiKitRepo, tag, name, ext)
	if err := download(url, archive, sha); err != nil {
		return err
	}
	out := filepath.Join(stage, "x")
	if ext == "zip" {
		err = extractZip(archive, out)
	} else {
		err = extractTarZst(archive, out)
	}
	if err != nil {
		return task.Errorf("%s.%s: %s", name, ext, err)
	}
	unpacked := filepath.Join(out, name)
	for _, want := range []string{filepath.Join("lib", GouiKitLibrary), filepath.Join("go", "go.mod")} {
		if info, err := os.Stat(filepath.Join(unpacked, want)); err != nil || !info.Mode().IsRegular() {
			return task.Errorf("%s.%s has no %s", name, ext, filepath.ToSlash(filepath.Join(name, want)))
		}
	}
	if err := os.WriteFile(filepath.Join(unpacked, stampName), []byte(sha+"\n"), 0o644); err != nil {
		return task.Error(task.IOError(err))
	}
	if err := os.RemoveAll(dest); err != nil {
		return task.Errorf("%s: %s", dest, task.IOError(err))
	}
	if err := os.Rename(unpacked, dest); err != nil {
		return task.Errorf("%s: %s", dest, task.IOError(err))
	}
	fmt.Printf("goui-kit %s is ready in %s for %s\n", config.Version, dest, target)
	return nil
}
