package deps

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
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
