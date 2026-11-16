package packaging

// `app`: build the desktop app for this machine and run it, for `just app`
// and the recipes beside it (cargo run/build -p rc2fx-app before). Unlike
// `package` it needs neither Velopack nor the licence notices: a build
// without the `velopack` tag does not update itself, and the checkout's
// placeholder notices are compiled in.
//
//	(default)    development build (-tags rc2fxdebug,rc2fxdev, Rust's debug
//	             build) in target/debug
//	--release    release build (-trimpath -ldflags=-w) in target/release
//	--mock       the simulated mixer (-tags mockmixer); a development build
//	             only, stripped so it can go to another computer, in
//	             target/mock (the Rust `mock` profile)
//	--embedded   stage the mixer payload from target/mixer and compile it in
//	             (-tags embedded)
//	--build-only build, do not run
//
// Arguments after `--` go to the app. The gpuikit library is copied beside
// the executable, which finds it there.

import (
	"os"
	"os/exec"
	"path/filepath"
	"runtime"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// RunApp is `./xtask app [--release] [--mock] [--embedded] [--build-only] [-- ARGS...]`.
func RunApp(args []string) error {
	var release, mock, embedded, buildOnly bool
	var appArgs []string
	for i, arg := range args {
		switch arg {
		case "--release":
			release = true
		case "--mock":
			mock = true
		case "--embedded":
			embedded = true
		case "--build-only":
			buildOnly = true
		case "--":
			appArgs = args[i+1:]
		default:
			return task.Errorf("unknown app option %s; see `./xtask help`", arg)
		}
		if arg == "--" {
			break
		}
	}
	if release && mock {
		return task.Error("the mock mixer is for development builds only; drop --release")
	}
	root := task.Root()
	desktop := filepath.Join(root, "desktop")
	goos := runtime.GOOS
	gpuikit, err := findGpuikit(root, goos)
	if err != nil {
		return err
	}
	host := func(args ...string) *exec.Cmd {
		command := exec.Command("go", args...)
		command.Dir = desktop
		return command
	}
	if embedded {
		if err := task.Run(host("run", "./installer/embed/cmd/stage-payload", "-root", root)); err != nil {
			return err
		}
	}
	manifestEmbedded := false
	if goos == "windows" {
		syso, err := windowsResources(root)
		if err != nil {
			return err
		}
		if syso != "" {
			manifestEmbedded = true
			defer func() { _ = os.Remove(syso) }()
		}
	}
	var tags []string
	var flags []string
	ldflags := ""
	out := filepath.Join(root, "target", "debug")
	switch {
	case release:
		out = filepath.Join(root, "target", "release")
		flags = append(flags, "-trimpath")
		ldflags = "-w"
	case mock:
		out = filepath.Join(root, "target", "mock")
		tags = append(tags, "mockmixer", "rc2fxdebug", "rc2fxdev")
		flags = append(flags, "-trimpath")
		ldflags = "-s -w"
	default:
		tags = append(tags, "rc2fxdebug", "rc2fxdev")
	}
	if embedded {
		tags = append(tags, "embedded")
	}
	ldflags, cgoLDFlags := appLinkFlags(goos, filepath.Dir(gpuikit), ldflags)
	if err := os.MkdirAll(out, 0o777); err != nil {
		return task.Error(task.IOError(err))
	}
	exe := filepath.Join(out, "rc2fx")
	if goos == "windows" {
		exe += ".exe"
	}
	buildArgs := append([]string{"build"}, flags...)
	if len(tags) > 0 {
		tagList := tags[0]
		for _, tag := range tags[1:] {
			tagList += "," + tag
		}
		buildArgs = append(buildArgs, "-tags", tagList)
	}
	if ldflags != "" {
		buildArgs = append(buildArgs, "-ldflags", ldflags)
	}
	buildArgs = append(buildArgs, "-o", exe, "./cmd/rc2fx")
	build := host(buildArgs...)
	// This machine's architecture only; on macOS for the release's minimum
	// macOS too (buildEnv), so a development build runs where a release does.
	build.Env = buildEnv(os.Environ(), goos, runtime.GOARCH, cgoLDFlags)
	if err := task.Run(build); err != nil {
		return err
	}
	if err := copyLibrary(gpuikit, filepath.Join(out, gpuikitLibrary(goos))); err != nil {
		return err
	}
	if goos == "windows" {
		// Without a resource compiler the manifest goes beside the program;
		// with one it is embedded, and an old copy beside it must not linger.
		if manifestEmbedded {
			_ = os.Remove(exe + ".manifest")
		} else if err := besideManifest(root, exe); err != nil {
			return err
		}
		if err := checkWindowsManifest(exe); err != nil {
			return err
		}
	}
	if buildOnly {
		return nil
	}
	return task.Run(exec.Command(exe, appArgs...))
}

// appLinkFlags adds to ldflags and CGO_LDFLAGS what every build of the app
// links with on goos: the gpuikit library in libDir, found again at run
// time beside the executable.
func appLinkFlags(goos, libDir, ldflags string) (string, string) {
	cgoLDFlags := "-L" + libDir
	switch goos {
	case "linux":
		// usr/bin/rc2fx finds usr/lib/libgpuikit.so in the AppDir.
		cgoLDFlags += " -Wl,-rpath,$ORIGIN/../lib -Wl,-rpath,$ORIGIN"
	case "darwin":
		cgoLDFlags += " -Wl,-rpath,@executable_path"
	case "windows":
		// No console window (main.rs's windows_subsystem = "windows"), and
		// the 8 MiB main stack build.rs reserved: GPUI's layout of the full
		// EQ editor exceeds Windows' default 1 MiB.
		if ldflags != "" {
			ldflags += " "
		}
		ldflags += "-H windowsgui -extldflags=-Wl,--stack,8388608"
		// MinGW's libgcc goes into the executable: a clean Windows has no
		// libgcc_s DLL (checkWindowsImports refuses one).
		cgoLDFlags += " -static-libgcc"
	}
	if existing := os.Getenv("CGO_LDFLAGS"); existing != "" {
		cgoLDFlags = existing + " " + cgoLDFlags
	}
	return ldflags, cgoLDFlags
}
