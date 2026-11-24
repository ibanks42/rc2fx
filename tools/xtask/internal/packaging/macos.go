package packaging

// Builds the universal RC2FX.app bundle for macOS (signed, notarized, and
// packaged by Velopack in velopack.go).

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

const (
	app      = "RC2FX.app"
	bundleID = "io.github.ibanks42.rc2fx"
	iconPath = task.AssetsDir + "/rc2fx.icns"
)

func infoPlist(version string) string {
	return fmt.Sprintf(`<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key><string>en</string>
    <key>CFBundleDisplayName</key><string>RC2FX</string>
    <key>CFBundleExecutable</key><string>rc2fx</string>
    <key>CFBundleIconFile</key><string>rc2fx</string>
    <key>CFBundleIdentifier</key><string>%s</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>CFBundleName</key><string>RC2FX</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>%s</string>
    <key>CFBundleVersion</key><string>%s</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
    <key>LSMinimumSystemVersion</key><string>%s</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
`, bundleID, version, version, macOSMinimum)
}

// bundle wraps the rc2fx executable in RC2FX.app with Info.plist and its icon.
func bundle(files Files, version string) (Files, error) {
	executable, ok := files["rc2fx"]
	if !ok {
		return nil, task.Error("package has no rc2fx executable")
	}
	icon, err := os.ReadFile(filepath.Join(task.Root(), filepath.FromSlash(iconPath)))
	if err != nil {
		return nil, task.Errorf("%s: %s", iconPath, task.IOError(err))
	}
	delete(files, "rc2fx")
	contents := app + "/Contents"
	files[contents+"/MacOS/rc2fx"] = executable
	files[contents+"/Info.plist"] = file([]byte(infoPlist(version)))
	files[contents+"/Resources/rc2fx.icns"] = file(icon)
	return files, nil
}
