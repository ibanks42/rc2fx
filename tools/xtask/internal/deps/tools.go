package deps

import (
	"bytes"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

// pinnedTool is a download checked against a SHA-256 before it is used.
type pinnedTool struct {
	version, url, sha256 string
}

var (
	linuxdeployPin = pinnedTool{
		version: "1-alpha-20251107-1",
		url:     "https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage",
		sha256:  "c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d",
	}
	patchelfPin = pinnedTool{
		version: "0.19.3",
		url:     "https://github.com/NixOS/patchelf/releases/download/0.19.3/patchelf-0.19.3-x86_64.tar.gz",
		sha256:  "38861b171079e440b25ac86483284f690d0ee7921dba98b0c8fe04a9b0a11667",
	}
	zigPin = pinnedTool{
		version: "0.16.0",
		url:     "https://ziglang.org/download/0.16.0/zig-x86_64-linux-0.16.0.tar.xz",
		sha256:  "70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00",
	}
)

// ToolsDir is where `go -C tools/xtask run . setup` installs the packaging tools:
// vpk, and on Linux linuxdeploy and patchelf. Packaging uses them when
// RC2FX_VPK, RC2FX_LINUXDEPLOY, and RC2FX_PATCHELF are unset.
func ToolsDir(root string) string {
	return filepath.Join(thirdParty(root), "tools")
}

// toolsDir is dir, or ToolsDir when dir is empty.
func toolsDir(dir string) string {
	if dir == "" {
		return ToolsDir(task.Root())
	}
	return dir
}

// VPKDir is where FetchVPK installs vpk under tools: source/ holds the
// verified NuGet package and bin/ the installed tool.
func VPKDir(tools string) string {
	return filepath.Join(tools, "rc2fx-vpk")
}

// LocalVPK returns the path to the vpk installed by setup, or "" if it is missing.
func LocalVPK(root string) string {
	return installedVPK(ToolsDir(root))
}

func installedVPK(tools string) string {
	name := "vpk"
	if runtime.GOOS == "windows" {
		name = "vpk.exe"
	}
	return existingFile(filepath.Join(VPKDir(tools), "bin", name))
}

// LocalLinuxdeploy returns the path to the linuxdeploy installed by setup, or "" if it is missing.
func LocalLinuxdeploy(root string) string {
	return linuxdeployPath(ToolsDir(root))
}

func linuxdeployPath(tools string) string {
	return existingFile(filepath.Join(tools, "appimage", "linuxdeploy", "AppRun"))
}

// LocalPatchelf returns the path to the patchelf installed by setup, or "" if it is missing.
func LocalPatchelf(root string) string {
	return patchelfPath(ToolsDir(root))
}

func patchelfPath(tools string) string {
	return existingFile(filepath.Join(tools, "appimage", "patchelf-"+patchelfPin.version, "bin", "patchelf"))
}

func existingFile(path string) string {
	if info, err := os.Stat(path); err == nil && info.Mode().IsRegular() {
		return path
	}
	return ""
}

// stamped reports whether dir holds a download whose stamp is sha.
func stamped(dir, sha string) bool {
	stamp, err := os.ReadFile(filepath.Join(dir, stampName))
	return err == nil && strings.TrimSpace(string(stamp)) == sha
}

// install fills dest from a fresh download: fill gets the verified
// download and an empty folder to unpack it into, which then replaces dest
// with a stamp of the download's SHA-256.
func install(tools, dest string, pin pinnedTool, fill func(download, out string) error) error {
	if err := os.MkdirAll(tools, 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	stage, err := os.MkdirTemp(tools, ".fetch.")
	if err != nil {
		return task.Error(task.IOError(err))
	}
	defer os.RemoveAll(stage)
	file := filepath.Join(stage, filepath.Base(pin.url))
	if err := download(pin.url, file, pin.sha256); err != nil {
		return err
	}
	out := filepath.Join(stage, "out")
	if err := os.Mkdir(out, 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	if err := fill(file, out); err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(out, stampName), []byte(pin.sha256+"\n"), 0o644); err != nil {
		return task.Error(task.IOError(err))
	}
	if err := os.MkdirAll(filepath.Dir(dest), 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	if err := os.RemoveAll(dest); err != nil {
		return task.Errorf("%s: %s", dest, task.IOError(err))
	}
	if err := os.Rename(out, dest); err != nil {
		return task.Errorf("%s: %s", dest, task.IOError(err))
	}
	return nil
}

// AppImageTools are the linuxdeploy and patchelf FetchAppImageTools
// installed.
type AppImageTools struct {
	Linuxdeploy, Patchelf string
}

// FetchAppImageTools installs the pinned linuxdeploy and patchelf for x86-64
// Linux into tools (default: ToolsDir), skipping what is already there.
func FetchAppImageTools(tools string) (AppImageTools, error) {
	if runtime.GOOS != "linux" || runtime.GOARCH != "amd64" {
		return AppImageTools{}, task.Error("the AppImage tools run on x86-64 Linux only")
	}
	tools = toolsDir(tools)
	linuxdeploy := filepath.Join(tools, "appimage", "linuxdeploy")
	if linuxdeployPath(tools) == "" || !stamped(linuxdeploy, linuxdeployPin.sha256) {
		fmt.Printf("Installing linuxdeploy %s\n", linuxdeployPin.version)
		err := install(tools, linuxdeploy, linuxdeployPin, func(download, out string) error {
			if err := os.Chmod(download, 0o755); err != nil {
				return task.Error(task.IOError(err))
			}
			// The AppImage unpacks itself into squashfs-root beside it.
			extract := exec.Command(download, "--appimage-extract")
			extract.Dir = filepath.Dir(download)
			var output bytes.Buffer
			extract.Stdout, extract.Stderr = &output, &output
			if err := extract.Run(); err != nil {
				return task.Errorf("could not unpack linuxdeploy: %s\n%s", err, output.String())
			}
			if err := os.Remove(out); err != nil {
				return task.Error(task.IOError(err))
			}
			return os.Rename(filepath.Join(filepath.Dir(download), "squashfs-root"), out)
		})
		if err != nil {
			return AppImageTools{}, err
		}
	}
	patchelf := filepath.Join(tools, "appimage", "patchelf-"+patchelfPin.version)
	if patchelfPath(tools) == "" || !stamped(patchelf, patchelfPin.sha256) {
		fmt.Printf("Installing patchelf %s\n", patchelfPin.version)
		err := install(tools, patchelf, patchelfPin, func(download, out string) error {
			if err := extractTarGz(download, out); err != nil {
				return task.Errorf("%s: %s", filepath.Base(download), err)
			}
			return nil
		})
		if err != nil {
			return AppImageTools{}, err
		}
	}
	result := AppImageTools{Linuxdeploy: linuxdeployPath(tools), Patchelf: patchelfPath(tools)}
	if result.Linuxdeploy == "" || result.Patchelf == "" {
		return AppImageTools{}, task.Errorf("the AppImage tools are incomplete in %s", tools)
	}
	fmt.Printf("AppImage tools ready in %s\n", filepath.Join(tools, "appimage"))
	return result, nil
}

// FetchZig installs the pinned zig for x86-64 Linux into tools (default:
// ToolsDir) and returns its folder. In a GitHub workflow it also puts that
// folder on the PATH of later steps.
func FetchZig(tools string) (string, error) {
	if runtime.GOOS != "linux" || runtime.GOARCH != "amd64" {
		return "", task.Error("the pinned zig download is for x86-64 Linux only")
	}
	tools = toolsDir(tools)
	dest := filepath.Join(tools, "zig-"+zigPin.version)
	if existingFile(filepath.Join(dest, "zig")) == "" || !stamped(dest, zigPin.sha256) {
		fmt.Printf("Installing zig %s\n", zigPin.version)
		err := install(tools, dest, zigPin, func(download, out string) error {
			// Go has no xz reader; every Linux system this runs on has tar and xz.
			return task.Run(exec.Command("tar", "-xJf", download, "--strip-components=1", "-C", out))
		})
		if err != nil {
			return "", err
		}
	}
	version, err := exec.Command(filepath.Join(dest, "zig"), "version").Output()
	if err != nil || strings.TrimSpace(string(version)) != zigPin.version {
		return "", task.Errorf("%s does not report zig %s", dest, zigPin.version)
	}
	if err := appendGitHubFile("GITHUB_PATH", dest); err != nil {
		return "", err
	}
	fmt.Printf("Zig ready: %s\n", dest)
	return dest, nil
}

// FetchVPK installs Velopack's vpk at the version and SHA-256 pinned in
// config/velopack.json into tools (default: ToolsDir), using only the
// verified package as its NuGet source, and returns its path.
func FetchVPK(tools string) (string, error) {
	tools = toolsDir(tools)
	var config struct {
		Version   string `json:"version"`
		VPKSHA256 string `json:"vpkSha256"`
	}
	configPath := filepath.Join(task.Root(), "config", "velopack.json")
	if err := readJSON(configPath, &config); err != nil {
		return "", err
	}
	if strings.TrimSpace(config.Version) == "" || !validSHA256(config.VPKSHA256) {
		return "", task.Errorf("%s has no version or vpkSha256", configPath)
	}
	dir := VPKDir(tools)
	if vpk := installedVPK(tools); vpk != "" && stamped(dir, config.VPKSHA256) {
		fmt.Printf("vpk %s is already in %s\n", config.Version, dir)
		return vpk, nil
	}
	if _, err := exec.LookPath("dotnet"); err != nil {
		return "", task.Error("vpk needs the .NET SDK (8 or newer) on PATH. Install it, then rerun setup:\n  " +
			"Fedora:  sudo dnf install dotnet-sdk-8.0\n  " +
			"Ubuntu:  sudo apt install dotnet-sdk-8.0\n  " +
			"macOS:   brew install --cask dotnet-sdk\n  " +
			"Windows: winget install Microsoft.DotNet.SDK.8\n  " +
			"https://dotnet.microsoft.com/download")
	}
	fmt.Printf("Installing vpk %s\n", config.Version)
	// dotnet's launcher in bin/ finds the tool by a relative path, so
	// install in place rather than in a staging folder; the stamp, written
	// last, marks a finished install.
	if err := os.RemoveAll(dir); err != nil {
		return "", task.Errorf("%s: %s", dir, task.IOError(err))
	}
	source := filepath.Join(dir, "source")
	if err := os.MkdirAll(source, 0o777); err != nil {
		return "", task.Error(task.IOError(err))
	}
	url := fmt.Sprintf("https://api.nuget.org/v3-flatcontainer/vpk/%s/vpk.%s.nupkg", config.Version, config.Version)
	if err := download(url, filepath.Join(source, "vpk."+config.Version+".nupkg"), config.VPKSHA256); err != nil {
		return "", err
	}
	nugetConfig := filepath.Join(dir, "nuget.config")
	text := "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<configuration>\n  <packageSources>\n    <clear />\n" +
		"    <add key=\"verified\" value=\"" + source + "\" />\n  </packageSources>\n</configuration>\n"
	if err := os.WriteFile(nugetConfig, []byte(text), 0o644); err != nil {
		return "", task.Error(task.IOError(err))
	}
	command := exec.Command("dotnet", "tool", "install", "vpk", "--version", config.Version,
		"--tool-path", filepath.Join(dir, "bin"), "--configfile", nugetConfig)
	command.Env = append(os.Environ(), "DOTNET_CLI_TELEMETRY_OPTOUT=1", "DOTNET_NOLOGO=1")
	if err := task.Run(command); err != nil {
		return "", err
	}
	vpk := installedVPK(tools)
	if vpk == "" {
		return "", task.Errorf("dotnet installed no vpk into %s", filepath.Join(dir, "bin"))
	}
	check := exec.Command(vpk, "--help")
	if output, err := check.CombinedOutput(); err != nil {
		return "", task.Errorf("%s --help failed: %s\n%s", vpk, err, output)
	}
	if err := os.WriteFile(filepath.Join(dir, stampName), []byte(config.VPKSHA256+"\n"), 0o644); err != nil {
		return "", task.Error(task.IOError(err))
	}
	fmt.Printf("vpk %s ready: %s\n", config.Version, vpk)
	return vpk, nil
}

// VPKPackage is the verified vpk NuGet package FetchVPK keeps beside the
// tool, for the AppImage runtime notices.
func VPKPackage(tools string) (string, error) {
	matches, _ := filepath.Glob(filepath.Join(VPKDir(toolsDir(tools)), "source", "vpk.*.nupkg"))
	if len(matches) != 1 {
		return "", task.Errorf("expected one vpk package in %s; run `go -C tools/xtask run . fetch-vpk`", filepath.Join(VPKDir(toolsDir(tools)), "source"))
	}
	return matches[0], nil
}

// appendGitHubFile appends line to the workflow file a GitHub Actions
// variable such as GITHUB_PATH names, and does nothing outside a workflow.
func appendGitHubFile(variable, line string) error {
	path := os.Getenv(variable)
	if path == "" {
		return nil
	}
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
	if err != nil {
		return task.Errorf("%s: %s", variable, task.IOError(err))
	}
	_, err = fmt.Fprintln(file, line)
	if closeErr := file.Close(); err == nil {
		err = closeErr
	}
	if err != nil {
		return task.Errorf("%s: %s", variable, task.IOError(err))
	}
	return nil
}
