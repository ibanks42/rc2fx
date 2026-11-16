package packaging

// Checks on the universal macOS app, in Go so they run, and are tested, on
// any machine (lipo and otool exist only on macOS): every Mach-O file the
// app ships holds an arm64 and an x86_64 slice, each built for macOS and for
// no newer macOS than the bundle's LSMinimumSystemVersion, and loads its
// libraries only from the system or from beside the executable. The
// installer must offer itself to both kinds of Mac.

import (
	"archive/zip"
	"bytes"
	"compress/gzip"
	"compress/zlib"
	"debug/macho"
	"encoding/binary"
	"encoding/xml"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"path"
	"slices"
	"strconv"
	"strings"

	"howett.net/plist"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// macOSMinimum is the oldest macOS the app runs on, which is Go's: Go 1.27
// requires macOS 13 Ventura (its linker's default minimum). It is the
// bundle's LSMinimumSystemVersion, every slice is compiled and linked for it
// (-mmacosx-version-min), and no file the app ships may need a newer one.
// Raise it with the Go toolchain, never lower it below Go's.
const macOSMinimum = "13.0"

// universalArches is the slices every shipped Mach-O file holds, by lipo's
// names, sorted.
var universalArches = []string{"arm64", "x86_64"}

// darwinGoArches is the GOARCH of each slice of the macOS app, which is
// built once per architecture and joined by lipo.
var darwinGoArches = []string{"arm64", "amd64"}

// clangArch is clang's -arch name for a GOARCH.
func clangArch(goarch string) string {
	if goarch == "amd64" {
		return "x86_64"
	}
	return goarch
}

// Load commands the checks read (mach-o/loader.h).
const (
	lcLoadDylib       = 0xc
	lcIDDylib         = 0xd
	lcLazyLoadDylib   = 0x20
	lcVersionMinMacOS = 0x24
	lcBuildVersion    = 0x32
	lcReqDyld         = 0x80000000
	lcLoadWeakDylib   = 0x18 | lcReqDyld
	lcRpath           = 0x1c | lcReqDyld
	lcReexportDylib   = 0x1f | lcReqDyld
	lcLoadUpwardDylib = 0x23 | lcReqDyld
	platformMacOS     = 1
)

// machoSlice is one architecture of a Mach-O file.
type machoSlice struct {
	arch string // lipo's name: arm64, x86_64
	// minOS is the oldest macOS it runs on (X.Y.Z as 0xXXXXYYZZ), 0 when it
	// names none; platform is LC_BUILD_VERSION's platform (macOS is 1).
	minOS, platform uint32
	dylib           bool     // MH_DYLIB
	install         string   // LC_ID_DYLIB
	loads           []string // the libraries it loads
	rpaths          []string
	data            []byte
}

// isMachO: data starts like a Mach-O file, thin or fat.
func isMachO(data []byte) bool {
	if len(data) < 4 {
		return false
	}
	switch binary.BigEndian.Uint32(data) {
	case macho.Magic32, macho.Magic64, 0xcefaedfe, 0xcffaedfe, macho.MagicFat:
		return true
	}
	return false
}

func lipoName(cpu macho.Cpu) string {
	switch cpu {
	case macho.CpuAmd64:
		return "x86_64"
	case macho.CpuArm64:
		return "arm64"
	}
	return cpu.String()
}

// loadString is the NUL-terminated string a load command points at with
// the 32-bit offset at raw[at:].
func loadString(order binary.ByteOrder, raw []byte, at int) (string, error) {
	if len(raw) < at+4 {
		return "", errors.New("load command too short")
	}
	offset := int(order.Uint32(raw[at:]))
	if offset < at+4 || offset >= len(raw) {
		return "", errors.New("load command string out of range")
	}
	text := raw[offset:]
	if end := bytes.IndexByte(text, 0); end >= 0 {
		text = text[:end]
	}
	return string(text), nil
}

// readSlice reads one thin Mach-O file.
func readSlice(data []byte) (machoSlice, error) {
	f, err := macho.NewFile(bytes.NewReader(data))
	if err != nil {
		return machoSlice{}, err
	}
	s := machoSlice{arch: lipoName(f.Cpu), dylib: f.Type == macho.TypeDylib, data: data}
	for _, load := range f.Loads {
		raw := load.Raw()
		if len(raw) < 8 {
			return machoSlice{}, errors.New("load command too short")
		}
		switch cmd := f.ByteOrder.Uint32(raw); cmd {
		case lcBuildVersion:
			if len(raw) < 16 {
				return machoSlice{}, errors.New("LC_BUILD_VERSION too short")
			}
			s.platform = f.ByteOrder.Uint32(raw[8:])
			s.minOS = f.ByteOrder.Uint32(raw[12:])
		case lcVersionMinMacOS:
			if len(raw) < 12 {
				return machoSlice{}, errors.New("LC_VERSION_MIN_MACOSX too short")
			}
			s.platform = platformMacOS
			s.minOS = f.ByteOrder.Uint32(raw[8:])
		case lcIDDylib, lcLoadDylib, lcLoadWeakDylib, lcReexportDylib, lcLazyLoadDylib, lcLoadUpwardDylib, lcRpath:
			name, err := loadString(f.ByteOrder, raw, 8)
			if err != nil {
				return machoSlice{}, err
			}
			switch cmd {
			case lcIDDylib:
				s.install = name
			case lcRpath:
				s.rpaths = append(s.rpaths, name)
			default:
				s.loads = append(s.loads, name)
			}
		}
	}
	return s, nil
}

// machoSlices is every architecture in a Mach-O file, and whether it is a
// fat (universal) file.
func machoSlices(data []byte) ([]machoSlice, bool, error) {
	if len(data) >= 4 && binary.BigEndian.Uint32(data) == macho.MagicFat {
		fat, err := macho.NewFatFile(bytes.NewReader(data))
		if err != nil {
			return nil, true, err
		}
		var out []machoSlice
		for _, arch := range fat.Arches {
			end := uint64(arch.Offset) + uint64(arch.Size)
			if end > uint64(len(data)) {
				return nil, true, errors.New("a slice runs past the end of the file")
			}
			s, err := readSlice(data[arch.Offset:end])
			if err != nil {
				return nil, true, err
			}
			out = append(out, s)
		}
		return out, true, nil
	}
	s, err := readSlice(data)
	if err != nil {
		return nil, false, err
	}
	return []machoSlice{s}, false, nil
}

// parseOSVersion is "X[.Y[.Z]]" as Mach-O encodes it, 0xXXXXYYZZ.
func parseOSVersion(text string) (uint32, bool) {
	parts := strings.Split(text, ".")
	if len(parts) == 0 || len(parts) > 3 {
		return 0, false
	}
	limits := []uint64{0xffff, 0xff, 0xff}
	shifts := []uint{16, 8, 0}
	var v uint32
	for i, part := range parts {
		n, err := strconv.ParseUint(part, 10, 16)
		if err != nil || n > limits[i] {
			return 0, false
		}
		v |= uint32(n) << shifts[i]
	}
	return v, true
}

func formatOSVersion(v uint32) string {
	text := fmt.Sprintf("%d.%d", v>>16, v>>8&0xff)
	if v&0xff != 0 {
		text += fmt.Sprintf(".%d", v&0xff)
	}
	return text
}

// checkUniversal: data, the file name, is a universal Mach-O with exactly
// an arm64 and an x86_64 slice, each for macOS and needing no newer macOS
// than minimum. It returns the slices.
func checkUniversal(name string, data []byte, minimum string) ([]machoSlice, error) {
	limit, ok := parseOSVersion(minimum)
	if !ok {
		return nil, task.Errorf("%q is not a macOS version", minimum)
	}
	found, fat, err := machoSlices(data)
	if err != nil {
		return nil, task.Errorf("%s is not a Mach-O file: %s", name, err)
	}
	var arches []string
	for _, s := range found {
		arches = append(arches, s.arch)
	}
	sorted := slices.Clone(arches)
	slices.Sort(sorted)
	if !fat || !slices.Equal(sorted, universalArches) {
		return nil, task.Errorf("%s holds %s, not exactly arm64 and x86_64: the macOS app is universal", name, strings.Join(arches, " and "))
	}
	for _, s := range found {
		switch {
		case s.platform != platformMacOS:
			return nil, task.Errorf("%s (%s) is not built for macOS (platform %d)", name, s.arch, s.platform)
		case s.minOS == 0:
			return nil, task.Errorf("%s (%s) names no minimum macOS", name, s.arch)
		case s.minOS > limit:
			return nil, task.Errorf("%s (%s) needs macOS %s, newer than the app's minimum %s (LSMinimumSystemVersion)",
				name, s.arch, formatOSVersion(s.minOS), minimum)
		}
	}
	return found, nil
}

// checkLibrary: a library the app loads from beside itself is universal,
// fits the app's minimum, and is found through the executable's run path.
func checkLibrary(name string, data []byte) error {
	found, err := checkUniversal(name, data, macOSMinimum)
	if err != nil {
		return err
	}
	want := "@rpath/" + path.Base(name)
	for _, s := range found {
		if !s.dylib || s.install != want {
			return task.Errorf("%s (%s) has install name %q, not %q", name, s.arch, s.install, want)
		}
	}
	return nil
}

// systemLibrary: a library macOS itself provides.
func systemLibrary(name string) bool {
	return strings.HasPrefix(name, "/usr/lib/") || strings.HasPrefix(name, "/System/Library/")
}

// checkMacBundle checks the app bundle's files (paths under RC2FX.app/):
// LSMinimumSystemVersion is macOSMinimum, every Mach-O file in it is
// universal and fits that minimum, and every library one loads is the
// system's or one beside the executable, which finds those through its
// @executable_path run path.
func checkMacBundle(files Files) error {
	contents := app + "/Contents/"
	info, ok := files[contents+"Info.plist"]
	if !ok {
		return task.Errorf("%sInfo.plist is missing", contents)
	}
	var keys map[string]any
	if _, err := plist.Unmarshal(info.Data, &keys); err != nil {
		return task.Errorf("%sInfo.plist: %s", contents, err)
	}
	minimum, _ := keys["LSMinimumSystemVersion"].(string)
	if minimum != macOSMinimum {
		return task.Errorf("LSMinimumSystemVersion is %q, not %s, the oldest macOS the Go app runs on", minimum, macOSMinimum)
	}
	executable, _ := keys["CFBundleExecutable"].(string)
	macOS := contents + "MacOS/"
	if _, ok := files[macOS+executable]; executable == "" || !ok {
		return task.Errorf("CFBundleExecutable %q is not in %s", executable, macOS)
	}
	checked := 0
	for _, name := range files.sortedNames() {
		data := files[name].Data
		if !strings.HasPrefix(name, contents) || !isMachO(data) {
			continue
		}
		found, err := checkUniversal(name, data, minimum)
		if err != nil {
			return err
		}
		checked++
		for _, s := range found {
			if name == macOS+executable && !slices.Contains(s.rpaths, "@executable_path") {
				return task.Errorf("%s (%s) has no @executable_path run path, so it cannot find its libraries", name, s.arch)
			}
			if s.dylib && path.Dir(name)+"/" == macOS && s.install != "@rpath/"+path.Base(name) {
				return task.Errorf("%s (%s) has install name %q, not %q", name, s.arch, s.install, "@rpath/"+path.Base(name))
			}
			for _, load := range s.loads {
				if systemLibrary(load) {
					continue
				}
				beside := ""
				for _, prefix := range []string{"@rpath/", "@executable_path/", "@loader_path/"} {
					if rest, ok := strings.CutPrefix(load, prefix); ok && !strings.Contains(rest, "/") {
						beside = rest
					}
				}
				if _, ok := files[macOS+beside]; beside == "" || !ok {
					return task.Errorf("%s (%s) loads %s, which is neither the system's nor in %s", name, s.arch, load, macOS)
				}
			}
		}
	}
	if checked == 0 {
		return task.Errorf("%s holds no Mach-O file", contents)
	}
	return nil
}

// checkPortableZip checks the bundle in vpk's portable zip, which has
// Velopack's updater in it too.
func checkPortableZip(name string, data []byte) error {
	archive, err := zip.NewReader(bytes.NewReader(data), int64(len(data)))
	if err != nil {
		return task.Errorf("%s: %s", name, err)
	}
	files := Files{}
	for _, entry := range archive.File {
		if !strings.HasPrefix(entry.Name, app+"/") || !entry.Mode().IsRegular() {
			continue
		}
		if entry.UncompressedSize64 > 1<<30 {
			return task.Errorf("%s: %s is too large", name, entry.Name)
		}
		r, err := entry.Open()
		if err != nil {
			return task.Errorf("%s: %s: %s", name, entry.Name, err)
		}
		content, err := io.ReadAll(r)
		_ = r.Close()
		if err != nil {
			return task.Errorf("%s: %s: %s", name, entry.Name, err)
		}
		files[entry.Name] = Member{Data: content, Mode: entry.Mode() & fs.ModePerm}
	}
	if _, ok := files[app+"/Contents/MacOS/UpdateMac"]; !ok {
		return task.Errorf("%s has no %s/Contents/MacOS/UpdateMac", name, app)
	}
	if err := checkMacBundle(files); err != nil {
		return task.Errorf("%s: %s", name, err)
	}
	return nil
}

// xarFile is a file in a xar archive's table of contents.
type xarFile struct {
	Name string `xml:"name"`
	Data *struct {
		Offset   uint64 `xml:"offset"`
		Length   uint64 `xml:"length"`
		Size     uint64 `xml:"size"`
		Encoding struct {
			Style string `xml:"style,attr"`
		} `xml:"encoding"`
	} `xml:"data"`
}

// xarMember is the top-level file name of a xar archive (an installer
// package), decoded; at most limit bytes.
func xarMember(data []byte, name string, limit uint64) ([]byte, error) {
	if len(data) < 28 || string(data[:4]) != "xar!" {
		return nil, errors.New("not a xar archive")
	}
	headerSize := uint64(binary.BigEndian.Uint16(data[4:]))
	tocCompressed := binary.BigEndian.Uint64(data[8:])
	tocSize := binary.BigEndian.Uint64(data[16:])
	heap := headerSize + tocCompressed
	if headerSize < 28 || tocSize > 16<<20 || tocCompressed > uint64(len(data)) || heap > uint64(len(data)) {
		return nil, errors.New("damaged xar header")
	}
	z, err := zlib.NewReader(bytes.NewReader(data[headerSize:heap]))
	if err != nil {
		return nil, fmt.Errorf("table of contents: %w", err)
	}
	toc, err := io.ReadAll(io.LimitReader(z, int64(tocSize)+1))
	if err != nil || uint64(len(toc)) != tocSize {
		return nil, errors.New("damaged table of contents")
	}
	var contents struct {
		Files []xarFile `xml:"toc>file"`
	}
	if err := xml.Unmarshal(toc, &contents); err != nil {
		return nil, fmt.Errorf("table of contents: %w", err)
	}
	for _, f := range contents.Files {
		if f.Name != name || f.Data == nil {
			continue
		}
		start, length := heap+f.Data.Offset, f.Data.Length
		if f.Data.Offset > uint64(len(data)) || length > uint64(len(data)) || start > uint64(len(data))-length ||
			f.Data.Size > limit {
			return nil, fmt.Errorf("%s is out of range", name)
		}
		stored := data[start : start+length]
		var r io.Reader
		switch f.Data.Encoding.Style {
		case "application/octet-stream", "":
			r = bytes.NewReader(stored)
		case "application/x-gzip":
			// xar's "gzip" is zlib; accept a real gzip stream too.
			if z, err := zlib.NewReader(bytes.NewReader(stored)); err == nil {
				r = z
			} else if g, err := gzip.NewReader(bytes.NewReader(stored)); err == nil {
				r = g
			} else {
				return nil, fmt.Errorf("%s: %w", name, err)
			}
		default:
			return nil, fmt.Errorf("%s has encoding %s", name, f.Data.Encoding.Style)
		}
		out, err := io.ReadAll(io.LimitReader(r, int64(limit)+1))
		if err != nil || uint64(len(out)) != f.Data.Size {
			return nil, fmt.Errorf("%s is damaged", name)
		}
		return out, nil
	}
	return nil, fmt.Errorf("no %s", name)
}

// checkInstaller: the installer package (a product archive) installs on
// both kinds of Mac. Without arm64 in its hostArchitectures, macOS asks
// Apple silicon owners to install Rosetta first; without x86_64 Intel Macs
// refuse it.
func checkInstaller(name string, data []byte) error {
	distribution, err := xarMember(data, "Distribution", 1<<20)
	if err != nil {
		return task.Errorf("%s: %s", name, err)
	}
	var script struct {
		XMLName xml.Name `xml:"installer-gui-script"`
		Options []struct {
			HostArchitectures *string `xml:"hostArchitectures,attr"`
		} `xml:"options"`
	}
	if err := xml.Unmarshal(distribution, &script); err != nil {
		return task.Errorf("%s: Distribution: %s", name, err)
	}
	var hosts []string
	for _, options := range script.Options {
		if options.HostArchitectures != nil {
			for _, host := range strings.Split(*options.HostArchitectures, ",") {
				hosts = append(hosts, strings.TrimSpace(host))
			}
		}
	}
	for _, arch := range universalArches {
		if !slices.Contains(hosts, arch) {
			return task.Errorf("%s: Distribution's hostArchitectures are %q, without %s", name, strings.Join(hosts, ","), arch)
		}
	}
	return nil
}
