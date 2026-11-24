package packaging

import (
	"archive/zip"
	"bytes"
	"compress/zlib"
	"debug/macho"
	"encoding/binary"
	"fmt"
	"slices"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/licenses"
)

// Minimal synthetic Mach-O builders (header + load commands) for debug/macho.

type loadCommand []byte

func padded(text string) []byte {
	b := append([]byte(text), 0)
	for len(b)%8 != 0 {
		b = append(b, 0)
	}
	return b
}

func le32(values ...uint32) []byte {
	var b []byte
	for _, v := range values {
		b = binary.LittleEndian.AppendUint32(b, v)
	}
	return b
}

func buildVersion(platform uint32, minos string) loadCommand {
	v, ok := parseOSVersion(minos)
	if !ok {
		panic(minos)
	}
	return loadCommand(le32(lcBuildVersion, 24, platform, v, 0x1a0200, 0))
}

func versionMinMacOS(minos string) loadCommand {
	v, _ := parseOSVersion(minos)
	return loadCommand(le32(lcVersionMinMacOS, 16, v, 0))
}

func dylibCommand(cmd uint32, name string) loadCommand {
	text := padded(name)
	return loadCommand(append(le32(cmd, uint32(24+len(text)), 24, 2, 0x10000, 0x10000), text...))
}

func rpathCommand(name string) loadCommand {
	// The 12-byte header and the string together fill 8-byte units.
	text := append([]byte(name), 0)
	for (12+len(text))%8 != 0 {
		text = append(text, 0)
	}
	return loadCommand(append(le32(lcRpath, uint32(12+len(text)), 12), text...))
}

func thinMachO(cpu macho.Cpu, kind macho.Type, loads ...loadCommand) []byte {
	var commands []byte
	for _, l := range loads {
		commands = append(commands, l...)
	}
	subtype := uint32(0)
	if cpu == macho.CpuAmd64 {
		subtype = 3
	}
	header := le32(macho.Magic64, uint32(cpu), subtype, uint32(kind), uint32(len(loads)), uint32(len(commands)), 0, 0)
	return append(append(header, commands...), make([]byte, 64)...)
}

func fatMachO(thin ...[]byte) []byte {
	const align = 12 // 4 KiB
	var header []byte
	header = binary.BigEndian.AppendUint32(header, macho.MagicFat)
	header = binary.BigEndian.AppendUint32(header, uint32(len(thin)))
	offset := uint32(1 << align)
	var body []byte
	for _, t := range thin {
		cpu := binary.LittleEndian.Uint32(t[4:])
		sub := binary.LittleEndian.Uint32(t[8:])
		for _, v := range []uint32{cpu, sub, offset, uint32(len(t)), align} {
			header = binary.BigEndian.AppendUint32(header, v)
		}
		for uint32(len(body))+(1<<align) < offset {
			body = append(body, 0)
		}
		body = append(body, t...)
		offset += (uint32(len(t)) + (1<<align - 1)) &^ (1<<align - 1)
	}
	out := append(header, make([]byte, (1<<align)-len(header))...)
	return append(out, body...)
}

func executableSlice(cpu macho.Cpu, minos string) []byte {
	return thinMachO(cpu, macho.TypeExec, buildVersion(platformMacOS, minos),
		dylibCommand(lcLoadDylib, "/usr/lib/libSystem.B.dylib"),
		dylibCommand(lcLoadDylib, "/System/Library/Frameworks/IOKit.framework/Versions/A/IOKit"))
}

func universalExecutable() []byte {
	return fatMachO(executableSlice(macho.CpuArm64, "13.0"), executableSlice(macho.CpuAmd64, "13.0"))
}

func besideSlice(cpu macho.Cpu, rpath bool) []byte {
	commands := []loadCommand{buildVersion(platformMacOS, "13.0"),
		dylibCommand(lcLoadDylib, "@rpath/libextra.dylib"), dylibCommand(lcLoadDylib, "/usr/lib/libSystem.B.dylib")}
	if rpath {
		commands = append(commands, rpathCommand("@executable_path"))
	}
	return thinMachO(cpu, macho.TypeExec, commands...)
}

func librarySlice(cpu macho.Cpu, id, minos string) []byte {
	return thinMachO(cpu, macho.TypeDylib, versionMinMacOS(minos), dylibCommand(lcIDDylib, id),
		dylibCommand(lcLoadDylib, "/usr/lib/libc++.1.dylib"))
}

func universalLibrary() []byte {
	return fatMachO(librarySlice(macho.CpuAmd64, "@rpath/libextra.dylib", "11.0"),
		librarySlice(macho.CpuArm64, "@rpath/libextra.dylib", "11.0"))
}

func TestOSVersions(t *testing.T) {
	for text, want := range map[string]uint32{"13.0": 0x0d0000, "10.12": 0x0a0c00, "11": 0x0b0000, "14.2.1": 0x0e0201} {
		if v, ok := parseOSVersion(text); !ok || v != want {
			t.Errorf("%s: %#x %v", text, v, ok)
		}
	}
	for _, bad := range []string{"", "13.", "x", "1.2.3.4", "13.256", "70000"} {
		if _, ok := parseOSVersion(bad); ok {
			t.Errorf("%q accepted", bad)
		}
	}
	if formatOSVersion(0x0a0c00) != "10.12" || formatOSVersion(0x0e0201) != "14.2.1" {
		t.Fatal("format")
	}
}

func TestUniversalMachOHoldsBothSlicesForTheMinimum(t *testing.T) {
	found, err := checkUniversal("rc2fx", universalExecutable(), "13.0")
	if err != nil {
		t.Fatal(err)
	}
	if len(found) != 2 || found[0].arch != "arm64" || found[1].arch != "x86_64" ||
		len(found[0].rpaths) != 0 || found[0].loads[0] != "/usr/lib/libSystem.B.dylib" {
		t.Fatalf("%+v", found)
	}
	// The slices are the thin files, for the per-slice payload checks.
	if !bytes.Equal(found[1].data, executableSlice(macho.CpuAmd64, "13.0")) {
		t.Fatal("slice data")
	}
	// A library older than the app's minimum is fine; LC_VERSION_MIN_MACOSX
	// counts too.
	if _, err := checkUniversal("libextra.dylib", universalLibrary(), "13.0"); err != nil {
		t.Fatal(err)
	}

	refused := map[string][]byte{
		"arm64 only":              executableSlice(macho.CpuArm64, "13.0"),
		"thin x86_64":             executableSlice(macho.CpuAmd64, "13.0"),
		"one slice in a fat file": fatMachO(executableSlice(macho.CpuArm64, "13.0")),
		"an extra slice": fatMachO(executableSlice(macho.CpuArm64, "13.0"), executableSlice(macho.CpuAmd64, "13.0"),
			thinMachO(macho.Cpu386, macho.TypeExec, buildVersion(platformMacOS, "10.12"))),
		"needs macOS 15": fatMachO(executableSlice(macho.CpuArm64, "13.0"), executableSlice(macho.CpuAmd64, "15.0")),
		"no minimum": fatMachO(executableSlice(macho.CpuArm64, "13.0"),
			thinMachO(macho.CpuAmd64, macho.TypeExec, rpathCommand("@executable_path"))),
		"built for iOS": fatMachO(executableSlice(macho.CpuArm64, "13.0"),
			thinMachO(macho.CpuAmd64, macho.TypeExec, buildVersion(2, "13.0"))),
		"not Mach-O": []byte("#!/bin/sh\n"),
	}
	for name, data := range refused {
		if _, err := checkUniversal("rc2fx", data, "13.0"); err == nil {
			t.Errorf("%s accepted", name)
		}
	}
	if _, err := checkUniversal("rc2fx", universalExecutable(), "12.0"); err == nil ||
		!strings.Contains(err.Error(), "needs macOS 13.0") {
		t.Fatalf("%v", err)
	}
}

func testBundle(t *testing.T) Files {
	t.Helper()
	files, err := bundle(Files{"rc2fx": {Data: universalExecutable(), Mode: 0o755}}, "1.2.3")
	if err != nil {
		t.Fatal(err)
	}
	return files
}

func TestTheBundleIsUniversalForItsMinimumMacOS(t *testing.T) {
	files := testBundle(t)
	if !strings.Contains(string(files["RC2FX.app/Contents/Info.plist"].Data),
		"<key>LSMinimumSystemVersion</key><string>"+macOSMinimum+"</string>") {
		t.Fatal("LSMinimumSystemVersion")
	}
	if err := checkMacBundle(files); err != nil {
		t.Fatal(err)
	}
	exe, lib := "RC2FX.app/Contents/MacOS/rc2fx", "RC2FX.app/Contents/MacOS/libextra.dylib"
	// A library beside the executable, found through its run path, passes.
	withLibrary := func(f Files) {
		f[exe] = Member{Data: fatMachO(besideSlice(macho.CpuArm64, true), besideSlice(macho.CpuAmd64, true))}
		f[lib] = Member{Data: universalLibrary()}
	}
	files = testBundle(t)
	withLibrary(files)
	if err := checkMacBundle(files); err != nil {
		t.Fatal(err)
	}
	for name, change := range map[string]func(Files){
		"an arm64-only executable": func(f Files) { f[exe] = Member{Data: executableSlice(macho.CpuArm64, "13.0")} },
		"an arm64-only library": func(f Files) {
			withLibrary(f)
			f[lib] = Member{Data: librarySlice(macho.CpuArm64, "@rpath/libextra.dylib", "11.0")}
		},
		"a library needing macOS 14": func(f Files) {
			withLibrary(f)
			f[lib] = Member{Data: fatMachO(librarySlice(macho.CpuAmd64, "@rpath/libextra.dylib", "14.0"),
				librarySlice(macho.CpuArm64, "@rpath/libextra.dylib", "11.0"))}
		},
		"a library not beside the executable": func(f Files) { withLibrary(f); delete(f, lib) },
		"a library with a build machine's install name": func(f Files) {
			withLibrary(f)
			f[lib] = Member{Data: fatMachO(librarySlice(macho.CpuAmd64, "/opt/lib/libextra.dylib", "11.0"),
				librarySlice(macho.CpuArm64, "/opt/lib/libextra.dylib", "11.0"))}
		},
		"no run path": func(f Files) {
			withLibrary(f)
			f[exe] = Member{Data: fatMachO(besideSlice(macho.CpuArm64, false), besideSlice(macho.CpuAmd64, false))}
		},
		"a library from Homebrew": func(f Files) {
			s := func(cpu macho.Cpu) []byte {
				return thinMachO(cpu, macho.TypeExec, buildVersion(platformMacOS, "13.0"),
					dylibCommand(lcLoadDylib, "/opt/homebrew/lib/libusb-1.0.0.dylib"), rpathCommand("@executable_path"))
			}
			f[exe] = Member{Data: fatMachO(s(macho.CpuArm64), s(macho.CpuAmd64))}
		},
		"an older LSMinimumSystemVersion": func(f Files) {
			plist := strings.Replace(infoPlist("1.2.3"), "<string>"+macOSMinimum+"</string>", "<string>11.0</string>", 1)
			f["RC2FX.app/Contents/Info.plist"] = file([]byte(plist))
		},
		"no Info.plist": func(f Files) { delete(f, "RC2FX.app/Contents/Info.plist") },
	} {
		f := testBundle(t)
		change(f)
		if err := checkMacBundle(f); err == nil {
			t.Errorf("%s accepted", name)
		}
	}
}

func macUpdatePackage(t *testing.T, files Files, extra map[string][]byte) []byte {
	t.Helper()
	var out bytes.Buffer
	w := zip.NewWriter(&out)
	add := func(name string, data []byte) {
		f, err := w.Create(name)
		if err != nil {
			t.Fatal(err)
		}
		if _, err := f.Write(data); err != nil {
			t.Fatal(err)
		}
	}
	for _, name := range files.sortedNames() {
		rel := strings.TrimPrefix(name, app+"/")
		add("lib/app/"+rel, files[name].Data)
	}
	for name, data := range extra {
		add(name, data)
	}
	if err := w.Close(); err != nil {
		t.Fatal(err)
	}
	return out.Bytes()
}

func TestTheMacUpdatePackageChecksVelopacksUpdaterToo(t *testing.T) {
	updater := fatMachO(thinMachO(macho.CpuAmd64, macho.TypeExec, versionMinMacOS("10.12")),
		thinMachO(macho.CpuArm64, macho.TypeExec, buildVersion(platformMacOS, "11.0")))
	metadata := map[string][]byte{"io.github.ibanks42.rc2fx.nuspec": []byte("<package/>")}
	good := testBundle(t)
	good["RC2FX.app/Contents/MacOS/UpdateMac"] = Member{Data: updater}
	if err := checkMacUpdatePackage("p.nupkg", macUpdatePackage(t, good, metadata)); err != nil {
		t.Fatal(err)
	}
	if err := checkMacUpdatePackage("p.nupkg", macUpdatePackage(t, testBundle(t), nil)); err == nil || !strings.Contains(err.Error(), "UpdateMac") {
		t.Fatalf("%v", err)
	}
	thinUpdater := testBundle(t)
	thinUpdater["RC2FX.app/Contents/MacOS/UpdateMac"] = Member{Data: thinMachO(macho.CpuArm64, macho.TypeExec, buildVersion(platformMacOS, "11.0"))}
	if err := checkMacUpdatePackage("p.nupkg", macUpdatePackage(t, thinUpdater, nil)); err == nil {
		t.Fatal("an arm64-only updater was accepted")
	}
	if err := checkMacUpdatePackage("p.nupkg", []byte("not a zip")); err == nil {
		t.Fatal("not a zip")
	}
}

func xarArchive(t *testing.T, distribution string, compressed bool) []byte {
	t.Helper()
	stored, style := []byte(distribution), "application/octet-stream"
	if compressed {
		var z bytes.Buffer
		w := zlib.NewWriter(&z)
		_, _ = w.Write(stored)
		_ = w.Close()
		stored, style = z.Bytes(), "application/x-gzip"
	}
	toc := fmt.Sprintf(`<?xml version="1.0" encoding="UTF-8"?>
<xar><toc><creation-time>2026-11-15T00:00:00</creation-time>
<file id="1"><data><length>%d</length><offset>20</offset><size>%d</size><encoding style="%s"/></data><name>Distribution</name><type>file</type></file>
<file id="2"><name>1.pkg</name><type>directory</type><file id="3"><name>Payload</name><type>file</type></file></file>
</toc></xar>`, len(stored), len(distribution), style)
	var z bytes.Buffer
	w := zlib.NewWriter(&z)
	_, _ = w.Write([]byte(toc))
	_ = w.Close()
	header := []byte("xar!")
	header = binary.BigEndian.AppendUint16(header, 28)
	header = binary.BigEndian.AppendUint16(header, 1)
	header = binary.BigEndian.AppendUint64(header, uint64(z.Len()))
	header = binary.BigEndian.AppendUint64(header, uint64(len(toc)))
	header = binary.BigEndian.AppendUint32(header, 0)
	out := append(header, z.Bytes()...)
	out = append(out, make([]byte, 20)...) // the TOC checksum's place in the heap
	return append(out, stored...)
}

func distribution(hosts string) string {
	return `<?xml version="1.0" encoding="utf-8"?>
<installer-gui-script minSpecVersion="2">
    <domains enable_anywhere="false" enable_currentUserHome="true" enable_localSystem="true" />
    <title>RC2FX</title>
    <pkg-ref id="io.github.ibanks42.rc2fx"/>
    <options customize="never" require-scripts="false"` + hosts + `/>
    <choices-outline><line choice="default"/></choices-outline>
</installer-gui-script>`
}

func TestTheInstallerOffersItselfToBothKindsOfMac(t *testing.T) {
	for _, compressed := range []bool{false, true} {
		if err := checkInstaller("i.pkg", xarArchive(t, distribution(` hostArchitectures="x86_64,arm64"`), compressed)); err != nil {
			t.Fatal(err)
		}
	}
	for name, hosts := range map[string]string{
		"Apple silicon only": ` hostArchitectures="arm64"`,
		"Intel only":         ` hostArchitectures="x86_64"`,
		"no architectures":   ``,
	} {
		if err := checkInstaller("i.pkg", xarArchive(t, distribution(hosts), true)); err == nil {
			t.Errorf("%s accepted", name)
		}
	}
	for name, data := range map[string][]byte{
		"not xar":   []byte("PK\x03\x04 a zip"),
		"truncated": xarArchive(t, distribution(` hostArchitectures="x86_64,arm64"`), false)[:60],
	} {
		if err := checkInstaller("i.pkg", data); err == nil {
			t.Errorf("%s accepted", name)
		}
	}
}

func envValue(env []string, name string) string {
	value := ""
	for _, entry := range env {
		if k, v, ok := strings.Cut(entry, "="); ok && k == name {
			value = v
		}
	}
	return value
}

func TestEachMacSliceIsBuiltForItsArchitectureAndTheMinimum(t *testing.T) {
	base := []string{"PATH=/usr/bin", "CGO_CFLAGS=", "CGO_LDFLAGS=-L/old"}
	for goarch, arch := range map[string]string{"arm64": "arm64", "amd64": "x86_64"} {
		env := buildEnv(base, "darwin", goarch, "-L/g/lib")
		want := map[string]string{
			"GOOS":                     "darwin",
			"GOARCH":                   goarch,
			"CGO_ENABLED":              "1",
			"CC":                       "clang",
			"CGO_CFLAGS":               "-O2 -g -arch " + arch + " -mmacosx-version-min=13.0",
			"CGO_LDFLAGS":              "-L/g/lib -arch " + arch + " -mmacosx-version-min=13.0",
			"MACOSX_DEPLOYMENT_TARGET": "13.0",
			"PATH":                     "/usr/bin",
		}
		for name, value := range want {
			if got := envValue(env, name); got != value {
				t.Errorf("%s %s: %q, not %q", goarch, name, got, value)
			}
		}
	}
	// A chosen compiler and C flags are kept.
	env := buildEnv([]string{"CC=/opt/clang", "CGO_CFLAGS=-O3"}, "darwin", "amd64", "-L/g")
	if envValue(env, "CC") != "/opt/clang" || envValue(env, "CGO_CFLAGS") != "-O3 -arch x86_64 -mmacosx-version-min=13.0" {
		t.Fatalf("%v", env)
	}
	// Elsewhere only cgo and the link flags.
	env = buildEnv([]string{"CC=gcc"}, "linux", "amd64", "-L/g")
	if envValue(env, "GOARCH") != "amd64" || envValue(env, "CGO_LDFLAGS") != "-L/g" || envValue(env, "CC") != "gcc" ||
		envValue(env, "CGO_CFLAGS") != "" || envValue(env, "MACOSX_DEPLOYMENT_TARGET") != "" {
		t.Fatalf("%v", env)
	}
	// Static libgouikit with linker dead-stripping; no @executable_path rpath needed.
	if ld, cgo := appLinkFlags("darwin", "/g/lib", "-s -w"); !strings.HasSuffix(cgo, "-L/g/lib") || ld != "-s -w -extldflags=-Wl,-dead_strip" {
		t.Fatal(ld, cgo)
	}
	if ld, _ := appLinkFlags("linux", "/g/lib", ""); ld != "-extldflags=-Wl,--gc-sections" {
		t.Fatal(ld)
	}
	ld, cgo := appLinkFlags("windows", "/g/lib", "-s -w")
	if !strings.Contains(ld, "-H windowsgui '-extldflags=-fuse-ld=lld ") || !strings.Contains(ld, "-Xlink=-force:multipleres'") ||
		!strings.Contains(ld, "--stack,8388608") || !strings.HasSuffix(cgo, "-L/g/lib -static-libgcc") {
		t.Fatal(ld, cgo)
	}
}

func TestMacPackagesAreUniversal(t *testing.T) {
	if !slices.Equal(goArches("darwin", "universal"), []string{"arm64", "amd64"}) ||
		!slices.Equal(goArches("linux", "amd64"), []string{"amd64"}) || !slices.Equal(goArches("windows", "amd64"), []string{"amd64"}) {
		t.Fatal("goArches")
	}
	if fetchTarget("darwin", "universal") != "darwin-universal" || fetchTarget("linux", "amd64") != "linux-amd64" {
		t.Fatal("fetchTarget")
	}
	for _, triple := range []string{"aarch64-apple-darwin", "x86_64-apple-darwin", "universal-apple-darwin"} {
		system, arch, err := platform(triple)
		if err != nil || system != "macos" || arch != "universal" {
			t.Errorf("%s: %s %s %v", triple, system, arch, err)
		}
	}
	if goos, goarch := goPlatform("macos", "universal"); goos != "darwin" || goarch != "universal" {
		t.Fatal(goos, goarch)
	}
	if goos, goarch := goPlatform("linux", "x64"); goos != "linux" || goarch != "amd64" {
		t.Fatal(goos, goarch)
	}
}

func TestTheExecutableIsCheckedSliceBySlice(t *testing.T) {
	exe := universalExecutable()
	images, err := executableImages("macos", exe)
	if err != nil || len(images) != 2 || images[0].arch != "arm64" || images[1].arch != "x86_64" ||
		!bytes.Equal(images[0].data, executableSlice(macho.CpuArm64, "13.0")) {
		t.Fatalf("%v %v", images, err)
	}
	if _, err := executableImages("macos", executableSlice(macho.CpuArm64, "13.0")); err == nil {
		t.Fatal("an arm64-only executable was accepted for macOS")
	}
	if images, err := executableImages("windows", []byte("MZ")); err != nil || len(images) != 1 || string(images[0].data) != "MZ" {
		t.Fatalf("%v %v", images, err)
	}
}

func TestNoticesCoverEverySlicesDependencies(t *testing.T) {
	a := []licenses.Section{{Title: "Go", Text: "BSD"}, {Title: "x/sys", Text: "BSD"}}
	b := []licenses.Section{{Title: "Go", Text: "BSD"}, {Title: "only amd64", Text: "MIT"}}
	if got := mergeSections(a, b); !slices.Equal(got, []licenses.Section{a[0], a[1], b[1]}) {
		t.Fatalf("%v", got)
	}
}

func TestTheMacPackageKeepsTheOsxChannelUnderUniversalNames(t *testing.T) {
	mac := target{os: "macos", arch: "universal", version: "1.2.3", name: "rc2fx-macos-universal"}
	if mac.runtime() != "osx-arm64" || mac.channel() != "osx" || mac.feed() != "releases.osx.json" ||
		mac.packageName() != "io.github.ibanks42.rc2fx-1.2.3-osx-full.nupkg" {
		t.Fatal(mac.runtime(), mac.feed(), mac.packageName())
	}
	var published []string
	for _, o := range mac.outputs() {
		published = append(published, o.publishAs)
	}
	if !slices.Equal(published, []string{"rc2fx-macos-universal.pkg",
		"io.github.ibanks42.rc2fx-1.2.3-osx-full.nupkg", "releases.osx.json"}) {
		t.Fatalf("%v", published)
	}
	joined := strings.Join(packArgs("/r", mac, "/p/RC2FX.app", "/s"), " ")
	if !strings.Contains(joined, "--runtime osx-arm64 --") || !strings.Contains(joined, "--noPortable") || strings.Contains(joined, "--channel") {
		t.Fatal(joined)
	}
}
