package packaging

// `app`: build the desktop app for this machine and run it, for `just app`
// and the recipes beside it (cargo run/build -p rc2fx-app before). Unlike
// `package` it needs neither Velopack nor the license notices: a build
// without the `velopack` tag does not update itself, and the checkout's
// placeholder notices are compiled in.
//
//	(default)    development build (-tags rc2fxdev) in
//	             target/debug
//	--release    release build (-trimpath -ldflags="-s -w") in target/release
//	--mock       the simulated mixer (-tags mockmixer); a development build
//	             only, stripped so it can go to another computer, in
//	             target/mock
//	--embedded   stage the mixer payload from target/mixer and compile it in
//	             (-tags embedded)
//	--build-only build, do not run
//
// Arguments after `--` go to the app. The gpuikit static library is linked
// into the executable, so it runs on its own.

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
	gpuikit, err := findGpuikit(root)
	if err != nil {
		return err
	}
	host := func(args ...string) *exec.Cmd {
		command := exec.Command("go", args...)
		command.Dir = desktop
		return command
	}
	if embedded {
		if err := task.Run(host("run", "./cmd/stage-payload", "-root", root)); err != nil {
			return err
		}
	}
	if goos == "windows" {
		syso, err := windowsResources(root)
		if err != nil {
			return err
		}
		if syso != "" {
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
		ldflags = "-s -w"
	case mock:
		out = filepath.Join(root, "target", "mock")
		tags = append(tags, "mockmixer", "rc2fxdev")
		flags = append(flags, "-trimpath")
		ldflags = "-s -w"
	default:
		tags = append(tags, "rc2fxdev")
	}
	if embedded {
		tags = append(tags, "embedded")
	}
	ldflags, cgoLDFlags := appLinkFlags(goos, gpuikit, ldflags)
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
	// Builds before the static library put it beside the executable.
	for _, old := range []string{"libgpuikit.so", "libgpuikit.dylib", "gpuikit.dll", "rc2fx.exe.manifest"} {
		_ = os.Remove(filepath.Join(out, old))
	}
	if goos == "windows" {
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
// links with on goos: the gpuikit static library from libDir, of which the C
// linker keeps only the component families the app calls into (package gk
// names the library and the system libraries it needs; the C linker drops
// what nothing uses only when told to, and Go lets no package ask for that).
func appLinkFlags(goos, libDir, ldflags string) (string, string) {
	cgoLDFlags := "-L" + libDir
	if ldflags != "" {
		ldflags += " "
	}
	switch goos {
	case "linux":
		ldflags += "-extldflags=-Wl,--gc-sections"
	case "darwin":
		ldflags += "-extldflags=-Wl,-dead_strip"
	case "windows":
		// No console window, and an 8 MiB main stack: GPUI's layout of the full
		// EQ editor exceeds Windows' default 1 MiB. LLVM's linker, because
		// MinGW's GNU ld keeps every function that has unwind data, which is
		// all of them; -force:multipleres lets package gk's manifest replace
		// the default one MinGW's gcc adds (lld keeps the first, gk's).
		ldflags += "-H windowsgui '-extldflags=-fuse-ld=lld -Wl,--stack,8388608,--gc-sections,-Xlink=-force:multipleres'"
		// MinGW's libgcc goes into the executable: a clean Windows has no
		// libgcc_s DLL (checkWindowsImports refuses one).
		cgoLDFlags += " -static-libgcc"
	}
	if existing := os.Getenv("CGO_LDFLAGS"); existing != "" {
		cgoLDFlags = existing + " " + cgoLDFlags
	}
	return ldflags, cgoLDFlags
}
