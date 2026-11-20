package packaging

// The Windows, macOS and Linux AppImage packages, made by Velopack's `vpk`
// (`RC2FX_VPK`, or `vpk` on the PATH): an installer (Setup.exe, Setup.pkg),
// a portable zip or the AppImage, which can update themselves too, and the
// update package and feed the app downloads from the latest release.
//
// On Linux vpk packages the finished AppDir as it is (its name ends in
// `.AppDir`), adding only its updater and version file under usr/bin, and
// puts its own AppImage runtime in front; it needs `mksquashfs` on the PATH.
// Published names have no version, so `releases/latest/download/NAME` always
// links the newest one, and an AppImage update replaces the file in place.
//
// Signing is vpk's. On Windows, `RC2FX_ARTIFACT_SIGNING_METADATA` names the
// Azure Artifact Signing metadata file, and vpk signs rc2fx.exe, its stub,
// the updater and Setup. On macOS, `RC2FX_MACOS_IDENTITY` and
// `RC2FX_MACOS_INSTALLER_IDENTITY` name the Developer ID Application and
// Installer certificates, and `RC2FX_NOTARY_PROFILE` a notarytool profile in
// the keychain `RC2FX_MACOS_KEYCHAIN`; vpk signs, notarizes and staples the
// app and the installer. Without these settings the packages are unsigned.

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/licenses"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// PackID is the id Velopack installs, updates and names packages by. On
// Windows it is the install folder under %LOCALAPPDATA%, so it must never be
// "RC2FX": that folder holds the user's data, which uninstalling would
// remove.
const PackID = "io.github.ibanks42.rc2fx"

// VPKVersion returns the vpk release this packaging is written for (see
// licenses.VPKVersion).
func VPKVersion(root string) (string, error) { return licenses.VPKVersion(root) }

const entitlements = task.AssetsDir + "/rc2fx.entitlements"

// target is where one platform's packages come from and go.
type target struct {
	os, arch, version string
	// name is `rc2fx-<os>-<arch>`.
	name string
}

func (t target) channel() string {
	switch t.os {
	case "windows":
		return "win"
	case "linux":
		return "linux"
	}
	return "osx"
}

// runtime is vpk's --runtime. The macOS app is universal, so its RID has
// no architecture: on macOS vpk only records the RID in the package's
// manifest, and neither the installer nor Velopack's updater checks it (the
// installer's hostArchitectures come from the binaries, see
// checkInstaller). The channel, and so the feed and package names, stay
// "osx", which every installed copy, the arm64-only ones included, updates
// from.
func (t target) runtime() string {
	if t.os == "macos" {
		return t.channel()
	}
	return t.channel() + "-" + t.arch
}

// packageName is the update package. Velopack leaves the channel out of the
// default Windows channel's name.
func (t target) packageName() string {
	if t.os == "windows" {
		return fmt.Sprintf("%s-%s-full.nupkg", PackID, t.version)
	}
	return fmt.Sprintf("%s-%s-%s-full.nupkg", PackID, t.version, t.channel())
}

func (t target) feed() string { return "releases." + t.channel() + ".json" }

// output is one file vpk writes, by its default name, and what it is
// published as.
type output struct{ made, publishAs string }

// outputs is what vpk writes and what each is published as. The update
// package and feed come last.
func (t target) outputs() []output {
	channel := t.channel()
	var outputs []output
	switch t.os {
	case "windows":
		outputs = []output{
			{fmt.Sprintf("%s-%s-Setup.exe", PackID, channel), t.name + "-setup.exe"},
			{fmt.Sprintf("%s-%s-Portable.zip", PackID, channel), t.name + ".zip"},
		}
	case "linux":
		outputs = []output{{PackID + ".AppImage", t.name + ".AppImage"}}
	default:
		outputs = []output{
			{fmt.Sprintf("%s-%s-Setup.pkg", PackID, channel), t.name + ".pkg"},
			{fmt.Sprintf("%s-%s-Portable.zip", PackID, channel), t.name + ".zip"},
		}
	}
	// The feed names the package, so both keep vpk's names.
	outputs = append(outputs, output{t.packageName(), t.packageName()}, output{t.feed(), t.feed()})
	return outputs
}

func signingArgs(system string) ([]string, error) {
	var args []string
	// vpk signs nothing on Linux: the apt and dnf repositories are signed
	// instead, and the app checks AppImage updates against signed metadata.
	if system == "linux" {
		return args, nil
	}
	if system == "windows" {
		if metadata, ok := setting("RC2FX_ARTIFACT_SIGNING_METADATA"); ok {
			args = append(args, "--azureTrustedSignFile", metadata)
		} else {
			fmt.Println("Windows signing is not configured: the packages stay unsigned.")
		}
		return args, nil
	}
	appIdentity, haveApp := setting("RC2FX_MACOS_IDENTITY")
	installerIdentity, haveInstaller := setting("RC2FX_MACOS_INSTALLER_IDENTITY")
	profile, haveProfile := setting("RC2FX_NOTARY_PROFILE")
	keychain, haveKeychain := setting("RC2FX_MACOS_KEYCHAIN")
	notarizing := haveProfile || haveKeychain
	switch {
	case haveApp && haveInstaller:
		args = append(args, "--signAppIdentity", appIdentity, "--signInstallIdentity", installerIdentity)
	case !haveApp && !haveInstaller && !notarizing:
		// Ad hoc, after vpk adds its updater: an unsealed bundle is
		// reported as damaged, a sealed one can be allowed in Privacy &
		// Security. Setup.pkg stays unsigned.
		fmt.Println("macOS signing is not configured: the app is signed ad hoc only.")
		return append(args, "--signAppIdentity", "-"), nil
	default:
		return nil, task.Error("set both RC2FX_MACOS_IDENTITY and RC2FX_MACOS_INSTALLER_IDENTITY, or neither")
	}
	switch {
	case haveProfile && haveKeychain:
		args = append(args, "--notaryProfile", profile, "--keychain", keychain)
	case !haveProfile && !haveKeychain:
		fmt.Println("Notarization is not configured: the packages are signed only.")
	default:
		return nil, task.Error("set both RC2FX_NOTARY_PROFILE and RC2FX_MACOS_KEYCHAIN, or neither")
	}
	return args, nil
}

func packArgs(root string, t target, packDir, staging string) []string {
	args := []string{
		"pack",
		"--yes",
		"--skip-updates",
		"--legacyConsole",
		"--packId", PackID,
		"--packVersion", t.version,
		"--packTitle", "RC2FX",
		"--packAuthors", "Isaiah Banks",
		// Every update downloads the full package: no delta bookkeeping
		// across releases.
		"--delta", "None",
	}
	args = append(args, "--runtime", t.runtime())
	args = append(args, "--packDir", packDir)
	args = append(args, "--outputDir", staging)
	switch t.os {
	case "windows":
		args = append(args, "--mainExe", "rc2fx.exe")
		args = append(args, "--icon", filepath.Join(root, filepath.FromSlash(task.AssetsDir), "rc2fx.ico"))
		args = append(args, "--shortcuts", "StartMenuRoot")
	case "linux":
		// Found at usr/bin/rc2fx in the AppDir. The AppDir already has its
		// desktop entry and icon, so vpk's --icon does not apply.
		// The image keeps vpk's gzip compression: its only other choice, xz,
		// makes every start slower.
		args = append(args, "--mainExe", "rc2fx")
	default:
		args = append(args, "--mainExe", "rc2fx")
		// The app needs no hardened-runtime exceptions; vpk's default grants
		// .NET's.
		args = append(args, "--signEntitlements", filepath.Join(root, filepath.FromSlash(entitlements)))
	}
	return args
}

// checkFeed: the feed must offer exactly this release's package, with its
// digest: that digest is what the app checks the download against.
func checkFeed(feed []byte, version, pkg, digest string) error {
	value, err := core.ParseJSONBytes(feed, 16<<20, 1_000_000)
	if err != nil {
		return task.Errorf("update feed: %s", err)
	}
	assetsValue, _ := value.Get("Assets")
	assets, _ := assetsValue.AsArray()
	if len(assets) != 1 {
		return task.Errorf("update feed lists %d packages, not 1", len(assets))
	}
	field := func(name string) string {
		v, _ := assets[0].Get(name)
		s, _ := v.AsString()
		return s
	}
	if field("PackageId") != PackID || field("Version") != version || field("Type") != "Full" ||
		field("FileName") != pkg || !strings.EqualFold(field("SHA256"), digest) {
		return task.Errorf("update feed does not describe %s %s with SHA-256 %s", pkg, version, digest)
	}
	return nil
}

// pack packages the app in packDir (the folder holding rc2fx.exe,
// RC2FX.app, or the AppDir) into out, returning the published files.
func pack(root, system, arch, version, name, packDir, out string) ([]string, error) {
	t := target{os: system, arch: arch, version: version, name: name}
	vpk, ok := setting("RC2FX_VPK")
	if !ok {
		vpk = "vpk"
	}
	staging := filepath.Join(out, name+"-velopack")
	_ = os.RemoveAll(staging)
	args := packArgs(root, t, packDir, staging)
	signing, err := signingArgs(system)
	if err != nil {
		return nil, err
	}
	args = append(args, signing...)
	if err := run(vpk, args); err != nil {
		vpkVer, _ := VPKVersion(root)
		return nil, task.Errorf("%s\nInstall vpk %s: dotnet tool install -g vpk --version %s", err, vpkVer, vpkVer)
	}
	var published []string
	for _, o := range t.outputs() {
		from := filepath.Join(staging, o.made)
		to := filepath.Join(out, o.publishAs)
		_ = os.Remove(to)
		if err := os.Rename(from, to); err != nil {
			return nil, task.Errorf("%s: %s", from, task.IOError(err))
		}
		published = append(published, to)
	}
	if system == "macos" {
		// What vpk made: the bundle with its updater, and the installer.
		for _, o := range t.outputs()[:2] {
			path := filepath.Join(out, o.publishAs)
			data, err := read(path)
			if err != nil {
				return nil, err
			}
			check := checkPortableZip
			if strings.HasSuffix(path, ".pkg") {
				check = checkInstaller
			}
			if err := check(o.publishAs, data); err != nil {
				return nil, err
			}
		}
	}
	pkg := t.packageName()
	data, err := read(filepath.Join(out, pkg))
	if err != nil {
		return nil, err
	}
	feed, err := read(filepath.Join(out, t.feed()))
	if err != nil {
		return nil, err
	}
	if err := checkFeed(feed, version, pkg, task.Sha256(data)); err != nil {
		return nil, err
	}
	_ = os.RemoveAll(staging)
	return published, nil
}
