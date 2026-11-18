// Package elfcheck makes static checks that a cross-built binary will load
// on the mixer: AArch64, only the firmware's system libraries, no symbol
// versions newer than the firmware provides, and no build-host search paths
// or text relocations.
package elfcheck

import (
	"bytes"
	"debug/elf"
	"encoding/binary"
	"errors"
	"fmt"
	"os"
	"slices"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// Libraries firmware 1.7.6 ships in /lib.
var libraries = []string{
	"libc.so.6",
	"libm.so.6",
	"libdl.so.2",
	"libpthread.so.0",
	"librt.so.1",
	"libgcc_s.so.1",
	"ld-linux-aarch64.so.1",
}

// Newest symbol versions in the firmware's glibc 2.35 and GCC 11 libgcc_s.
const (
	glibcMinor  = 35
	gccMajor    = 11
	interpreter = "/lib/ld-linux-aarch64.so.1"
)

// ELF constants debug/elf does not name.
const (
	dfTextrel = 0x4
	df1PIE    = 0x08000000
)

// Check checks the file at path, a shared library when shared.
func Check(path string, shared bool) error {
	data, err := os.ReadFile(path)
	if err != nil {
		return task.Errorf("%s: %s", path, task.IOError(err))
	}
	return CheckData(path, data, shared)
}

// CheckData is Check for bytes already read: path only names them, in the
// error and in the line that says they pass.
func CheckData(path string, data []byte, shared bool) error {
	if err := CheckBytes(data, shared); err != nil {
		return task.Errorf("%s: %s", path, err)
	}
	fmt.Printf("%s: meets the mixer ABI baseline (AArch64, glibc 2.35, GCC 11)\n", path)
	return nil
}

// CheckBytes checks an ELF image.
func CheckBytes(data []byte, shared bool) error {
	file, err := elf.NewFile(bytes.NewReader(data))
	if err != nil {
		return err
	}
	if file.Class != elf.ELFCLASS64 {
		return errors.New("Unsupported ELF header")
	}
	order := file.ByteOrder
	if file.Machine != elf.EM_AARCH64 {
		return errors.New("not an AArch64 binary")
	}
	if file.Type != elf.ET_DYN {
		return errors.New("not position independent")
	}
	var requested *string
	for _, segment := range file.Progs {
		if segment.Type != elf.PT_INTERP {
			continue
		}
		raw, err := readAll(segment.Open(), segment.Filesz)
		if err != nil {
			return errors.New("Invalid ELF interpreter offset/size")
		}
		path := string(bytes.TrimSuffix(raw, []byte{0}))
		requested = &path
	}
	switch {
	case shared && requested == nil:
	case shared:
		return errors.New("a shared library must not request an interpreter")
	case requested != nil && *requested == interpreter:
	default:
		return errors.New("executable must use /lib/ld-linux-aarch64.so.1")
	}

	dynamicFound := false
	for _, section := range file.Sections {
		if section.Type != elf.SHT_DYNAMIC {
			continue
		}
		dynamicFound = true
		entries, err := section.Data()
		if err != nil {
			return errors.New("Invalid ELF dynamic section offset or size")
		}
		table, err := linkedStrings(file, section)
		if err != nil {
			return err
		}
		for at := 0; at+16 <= len(entries); at += 16 {
			tag := elf.DynTag(order.Uint64(entries[at:]))
			value := order.Uint64(entries[at+8:])
			switch {
			case tag == elf.DT_NEEDED:
				library, err := cString(table, value)
				if err != nil {
					return err
				}
				if !slices.Contains(libraries, library) {
					return fmt.Errorf("needs %s, which the mixer does not ship", library)
				}
			case tag == elf.DT_RPATH || tag == elf.DT_RUNPATH:
				return errors.New("carries a library search path")
			case tag == elf.DT_TEXTREL:
				return errors.New("has text relocations")
			case tag == elf.DT_FLAGS && value&dfTextrel != 0:
				return errors.New("has text relocations")
			case tag == elf.DT_FLAGS_1 && shared && value&df1PIE != 0:
				return errors.New("is a PIE executable, not a library")
			}
		}
	}
	if !dynamicFound {
		return errors.New("is not dynamically linked")
	}
	for _, section := range file.Sections {
		if section.Type != elf.SHT_GNU_VERNEED {
			continue
		}
		data, err := section.Data()
		if err != nil {
			return errors.New("Invalid ELF section size or offset")
		}
		table, err := linkedStrings(file, section)
		if err != nil {
			return err
		}
		if err := checkVersionNeeds(data, table, order); err != nil {
			return err
		}
	}
	return nil
}

// checkVersionNeeds walks a SHT_GNU_VERNEED section: Elf64_Verneed records
// (vn_version, vn_cnt, vn_file, vn_aux, vn_next) each with a chain of
// Elf64_Vernaux (vna_hash, vna_flags, vna_other, vna_name, vna_next).
func checkVersionNeeds(data, table []byte, order binary.ByteOrder) error {
	invalid := errors.New("Invalid ELF GNU verneed")
	for at := 0; len(data[at:]) > 0; {
		if len(data[at:]) < 16 {
			return invalid
		}
		count := int(order.Uint16(data[at+2:]))
		aux := int(order.Uint32(data[at+8:]))
		next := int(order.Uint32(data[at+12:]))
		for i, a := 0, at+aux; i < count; i++ {
			if aux < 0 || a < 0 || a+16 > len(data) {
				return errors.New("Invalid ELF GNU vernaux")
			}
			name, err := cString(table, uint64(order.Uint32(data[a+8:])))
			if err != nil {
				return err
			}
			if !Supported(name) {
				return fmt.Errorf("needs symbol version %s, newer than the firmware", name)
			}
			step := int(order.Uint32(data[a+12:]))
			if step == 0 {
				break
			}
			a += step
		}
		if next == 0 {
			break
		}
		at += next
		if at > len(data) {
			return invalid
		}
	}
	return nil
}

func linkedStrings(file *elf.File, section *elf.Section) ([]byte, error) {
	link := int(section.Link)
	if link <= 0 || link >= len(file.Sections) {
		return nil, errors.New("Invalid ELF section index")
	}
	table := file.Sections[link]
	if table.Type != elf.SHT_STRTAB {
		return nil, errors.New("Invalid ELF string section type")
	}
	data, err := table.Data()
	if err != nil {
		return nil, errors.New("Invalid ELF string section offset or size")
	}
	return data, nil
}

func cString(table []byte, offset uint64) (string, error) {
	if offset >= uint64(len(table)) {
		return "", errors.New("Invalid ELF string offset")
	}
	rest := table[offset:]
	end := bytes.IndexByte(rest, 0)
	if end < 0 {
		return "", errors.New("Invalid ELF string offset")
	}
	return strings.ToValidUTF8(string(rest[:end]), "�"), nil
}

func readAll(reader interface{ Read([]byte) (int, error) }, size uint64) ([]byte, error) {
	if size > 1<<20 {
		return nil, errors.New("too large")
	}
	out := make([]byte, size)
	read := 0
	for read < len(out) {
		n, err := reader.Read(out[read:])
		read += n
		if err != nil {
			if read == len(out) {
				break
			}
			return nil, err
		}
	}
	return out, nil
}

// Supported reports a symbol version the firmware provides.
func Supported(version string) bool {
	number := func(text string, index int) (uint32, bool) {
		parts := strings.Split(text, ".")
		if index >= len(parts) {
			return 0, false
		}
		value, ok := task.ParseUint(parts[index], 32)
		return uint32(value), ok
	}
	if rest, ok := strings.CutPrefix(version, "GLIBC_"); ok {
		major, okMajor := number(rest, 0)
		minor, okMinor := number(rest, 1)
		return okMajor && major == 2 && okMinor && minor <= glibcMinor
	}
	if rest, ok := strings.CutPrefix(version, "GCC_"); ok {
		major, ok := number(rest, 0)
		return ok && major <= gccMajor
	}
	return false
}
