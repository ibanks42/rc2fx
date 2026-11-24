package pkgcheck

import (
	"os"
	"os/exec"
	"path/filepath"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/deps"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/packaging"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

// containerCache is the build container's cache mount.
const containerCache = "/work/cache"

// inContainer reports whether this runs in a podman or docker container.
func inContainer() bool {
	for _, marker := range []string{"/.dockerenv", "/run/.containerenv"} {
		if _, err := os.Stat(marker); err == nil {
			return true
		}
	}
	return false
}

// Build is `package-check-build`, which package-check runs in the build
// container against its source snapshot: it builds the desktop AppImage
// with release settings (the app downloads the mixer runtime separately).
func Build() error {
	if !inContainer() {
		return task.Error("package-check-build runs inside the package-check container; run `just package-check`")
	}
	root := task.Root()
	// goui-kit and velopack_libc are gitignored, so the snapshot has neither.
	if err := deps.FetchGouiKit("linux-amd64"); err != nil {
		return err
	}
	if err := deps.FetchVelopack([]string{"linux-amd64"}); err != nil {
		return err
	}
	tools := filepath.Join(containerCache, "tools")
	appimage, err := deps.FetchAppImageTools(tools)
	if err != nil {
		return err
	}
	vpk, err := deps.FetchVPK(tools)
	if err != nil {
		return err
	}
	vpkPackage, err := deps.VPKPackage(tools)
	if err != nil {
		return err
	}
	for name, value := range map[string]string{
		"RC2FX_LINUXDEPLOY":   appimage.Linuxdeploy,
		"RC2FX_PATCHELF":      appimage.Patchelf,
		"RC2FX_VPK":           vpk,
		"RC2FX_RELEASE_BUILD": "1",
	} {
		if err := os.Setenv(name, value); err != nil {
			return task.Error(task.IOError(err))
		}
	}

	stale, _ := filepath.Glob(filepath.Join(root, "target", "*-appimage-work"))
	for _, path := range append(stale, filepath.Join(root, "target", "package")) {
		if err := os.RemoveAll(path); err != nil {
			return task.Errorf("%s: %s", path, task.IOError(err))
		}
	}
	scripts := filepath.Join(root, ".github", "scripts")
	if err := task.Run(exec.Command("python3", filepath.Join(scripts, "test_prepare_appimage_notices.py"))); err != nil {
		return err
	}
	notices := exec.Command("python3", filepath.Join(scripts, "prepare-appimage-notices.py"),
		"--cache", filepath.Join(containerCache, "notices"), "--vpk-package", vpkPackage)
	if err := task.Run(notices); err != nil {
		return err
	}
	return packaging.Run(nil)
}
