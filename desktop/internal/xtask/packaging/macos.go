package packaging

// The macOS app: `RC2FX.app`. A bare executable opens in Terminal, so the
// app ships as a bundle. It is universal, for Apple silicon and Intel Macs
// (see gobuild.go and macho.go). Velopack signs, notarizes and packages it
// (see velopack.go).

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

const (
	app      = "RC2FX.app"
	bundleID = "io.github.ibanks42.rc2fx"
	// iconPath is the bundle icon, in the repository.
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

// bundle moves the executable, and the gpuikit library beside it, into an
// app bundle: Contents/MacOS/rc2fx finds libgpuikit.dylib through its
// @executable_path run path. checkMacBundle checks the result.
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
	if library, ok := files[gpuikitLibrary("darwin")]; ok {
		delete(files, gpuikitLibrary("darwin"))
		files[contents+"/MacOS/"+gpuikitLibrary("darwin")] = library
	}
	files[contents+"/Info.plist"] = file([]byte(infoPlist(version)))
	files[contents+"/Resources/rc2fx.icns"] = file(icon)
	return files, nil
}
