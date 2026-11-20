// Package firmware defines mixer models, the firmware profile catalog, and the
// offline ABI/compatibility checks used by the installer before modifying a
// mixer.
//
// Ports the desktop-relevant parts of crates/rc2fx-firmware (catalog parsing
// and offline inspection); runtime selection and the CLI stay in Rust on the
// mixer. Shared fixtures live in fixtures/firmware.
package firmware

import (
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"math"
	"math/bits"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Catalog represents config/firmware-profiles.json. Decoded via core's strict
// JSON decoder to match Rust serde's deny_unknown_fields behavior: fields are
// required unless they are pointers (optional/nullable) or tagged `default`
// (missing allowed, null rejected).
type Catalog struct {
	SchemaVersion uint32    `json:"schemaVersion"`
	Profiles      []Profile `json:"profiles"`
}

const maxCatalogBytes = 128 * 1024

// Profile describes a single firmware release for a specific mixer model.
type Profile struct {
	MinimumAppVersion string   `json:"minimumAppVersion,default"`
	ID                string   `json:"id"`
	Model             string   `json:"model"`
	Version           string   `json:"version"`
	MixerSHA256       string   `json:"mixerSha256"`
	LauncherSHA256    string   `json:"launcherSha256"`
	ArchiveSHA256     string   `json:"archiveSha256"`
	ArchiveMD5        string   `json:"archiveMd5"`
	Runtime           *Runtime `json:"runtime"`
	Notes             string   `json:"notes"`
}

// Runtime holds the hook addresses and ABI metadata for a runtime-qualified
// firmware release.
type Runtime struct {
	CallbackABI string      `json:"callbackAbi"`
	RouteLayout string      `json:"routeLayout"`
	SourceEnum  string      `json:"sourceEnum"`
	SampleRate  uint32      `json:"sampleRate"`
	BlockFrames uint32      `json:"blockFrames"`
	ShowPath    string      `json:"showPath"`
	Callback    Site        `json:"callback"`
	JobLifetime JobLifetime `json:"jobLifetime"`
}

type JobLifetime struct {
	ABI     string `json:"abi"`
	Enqueue Site   `json:"enqueue"`
	Manager Site   `json:"manager"`
	Invoker Site   `json:"invoker"`
	Body    Site   `json:"body"`
}

// Site is a hook location: an executable-relative offset and the 16 original
// instruction bytes expected there.
type Site struct {
	Offset   string `json:"offset"`
	Expected string `json:"expected"`
}

func isHex(text string, length int) bool {
	if len(text) != length {
		return false
	}
	for i := 0; i < len(text); i++ {
		if c := text[i]; !('0' <= c && c <= '9' || 'a' <= c && c <= 'f') {
			return false
		}
	}
	return true
}

func sum(data []byte) string {
	digest := sha256.Sum256(data)
	return hex.EncodeToString(digest[:])
}

// OffsetValue parses s.Offset into an aligned executable-relative byte offset.
func (s *Site) OffsetValue() (int, error) {
	value, ok := strings.CutPrefix(s.Offset, "0x")
	if !ok {
		return 0, errors.New("site offset needs 0x prefix")
	}
	offset, err := parseHexUint(value)
	if err != nil {
		return 0, err
	}
	if offset == 0 || offset >= 64*1024*1024 || offset%4 != 0 {
		return 0, errors.New("site offset must be an aligned executable-relative address below 64 MiB")
	}
	return int(offset), nil
}

// parseHexUint matches Rust's usize::from_str_radix(_, 16) and its error text.
func parseHexUint(text string) (uint64, error) {
	digits := text
	if strings.HasPrefix(digits, "+") && len(digits) > 1 {
		digits = digits[1:]
	}
	if digits == "" {
		return 0, errors.New("cannot parse integer from empty string")
	}
	var value uint64
	for i := 0; i < len(digits); i++ {
		c := digits[i]
		var d uint64
		switch {
		case '0' <= c && c <= '9':
			d = uint64(c - '0')
		case 'a' <= c && c <= 'f':
			d = uint64(c-'a') + 10
		case 'A' <= c && c <= 'F':
			d = uint64(c-'A') + 10
		default:
			return 0, errors.New("invalid digit found in string")
		}
		hi, lo := bits.Mul64(value, 16)
		if hi != 0 || lo+d < lo {
			return 0, errors.New("number too large to fit in target type")
		}
		value = lo + d
	}
	return value, nil
}

// Bytes decodes the 16 original instruction bytes for this hook site.
func (s *Site) Bytes() ([16]byte, error) {
	var out [16]byte
	if !isHex(s.Expected, 32) {
		return out, errors.New("site requires exactly 16 original instruction bytes")
	}
	if _, err := hex.Decode(out[:], []byte(s.Expected)); err != nil {
		return out, err
	}
	return out, nil
}

// Sites returns all five hook sites with the audio callback first.
func (r *Runtime) Sites() [5]*Site {
	return [5]*Site{&r.Callback, &r.JobLifetime.Enqueue, &r.JobLifetime.Manager, &r.JobLifetime.Invoker, &r.JobLifetime.Body}
}

func (r *Runtime) validate() error {
	layout := r.RouteLayout == "pro-ii-25-v1" && r.SourceEnum == "pro-ii-sources-v1" ||
		r.RouteLayout == "duo-23-v1" && r.SourceEnum == "duo-sources-v1"
	if r.CallbackABI != "jack-process-v1" || !layout || r.JobLifetime.ABI != "capture-values-v1" ||
		r.SampleRate != 48_000 || r.BlockFrames != 112 {
		return errors.New("profile requests a callback, route, source, frame or job ABI this build does not implement")
	}
	if !strings.HasPrefix(r.ShowPath, "/Application/update-staging/") || strings.Contains(r.ShowPath, "..") ||
		!showPathBytes(r.ShowPath) {
		return errors.New("invalid saved-show path")
	}
	offsets := map[int]bool{}
	for _, site := range r.Sites() {
		offset, err := site.OffsetValue()
		if err != nil {
			return err
		}
		if offsets[offset] {
			return errors.New("duplicate hook site")
		}
		offsets[offset] = true
		if _, err := site.Bytes(); err != nil {
			return err
		}
	}
	// The 16-byte trampoline replays these instructions at a new address.
	// PC-relative branches/addressing/literal loads need a relocator that
	// capture-values-v1 deliberately does not implement.
	enqueue, err := r.JobLifetime.Enqueue.Bytes()
	if err != nil {
		return err
	}
	for at := 0; at < len(enqueue); at += 4 {
		instruction := binary.LittleEndian.Uint32(enqueue[at:])
		if instruction&0x7c00_0000 == 0x1400_0000 ||
			instruction&0x1f00_0000 == 0x1000_0000 ||
			instruction&0xff00_0010 == 0x5400_0000 ||
			instruction&0x7e00_0000 == 0x3400_0000 ||
			instruction&0x7e00_0000 == 0x3600_0000 ||
			instruction&0x3b00_0000 == 0x1800_0000 {
			return errors.New("job enqueuer prologue needs unsupported PC-relative relocation")
		}
	}
	return nil
}

func showPathBytes(path string) bool {
	for i := 0; i < len(path); i++ {
		c := path[i]
		if !('0' <= c && c <= '9' || 'a' <= c && c <= 'z' || 'A' <= c && c <= 'Z' || strings.IndexByte("/._-", c) >= 0) {
			return false
		}
	}
	return true
}

// ParseCatalog parses and validates a firmware catalog JSON document using the
// same strict rules as the Rust serde decoder.
func ParseCatalog(text string) (*Catalog, error) {
	if len(text) > maxCatalogBytes {
		return nil, errors.New("firmware catalog exceeds 128 KiB")
	}
	catalog := new(Catalog)
	if err := core.DecodeStrictLimited([]byte(text), maxCatalogBytes, core.DefaultMaxJSONNodes, catalog); err != nil {
		return nil, err
	}
	if catalog.SchemaVersion != 1 || len(catalog.Profiles) == 0 || len(catalog.Profiles) > 64 {
		return nil, errors.New("unsupported or empty firmware catalog")
	}
	type pair struct{ a, b string }
	ids := map[string]bool{}
	hashes := map[pair]bool{}
	releases := map[pair]bool{}
	for i := range catalog.Profiles {
		profile := &catalog.Profiles[i]
		_, modelErr := ParseModel(profile.Model)
		if profile.ID == "" || len(profile.ID) > 96 || !profileIDBytes(profile.ID) ||
			modelErr != nil ||
			!isHex(profile.MixerSHA256, 64) ||
			!isHex(profile.LauncherSHA256, 64) ||
			!isHex(profile.ArchiveSHA256, 64) ||
			!isHex(profile.ArchiveMD5, 32) ||
			ids[profile.ID] ||
			hashes[pair{profile.Model, profile.MixerSHA256}] ||
			releases[pair{profile.Model, profile.Version}] {
			return nil, errors.New("invalid or duplicate firmware profile identity")
		}
		ids[profile.ID] = true
		hashes[pair{profile.Model, profile.MixerSHA256}] = true
		releases[pair{profile.Model, profile.Version}] = true
		if runtime := profile.Runtime; runtime != nil {
			model, err := ParseModel(profile.Model)
			if err != nil {
				return nil, err
			}
			if runtime.RouteLayout != model.RouteLayout() || runtime.SourceEnum != model.SourceEnum() {
				return nil, errors.New("runtime routing does not match the device model")
			}
			if err := runtime.validate(); err != nil {
				return nil, fmt.Errorf("%s: %v", profile.ID, err)
			}
		}
	}
	return catalog, nil
}

func profileIDBytes(id string) bool {
	for i := 0; i < len(id); i++ {
		c := id[i]
		if !('a' <= c && c <= 'z' || '0' <= c && c <= '9' || c == '-' || c == '.') {
			return false
		}
	}
	return true
}

func (c *Catalog) identify(model Model, image []byte) (*Profile, error) {
	hash := sum(image)
	for i := range c.Profiles {
		if p := &c.Profiles[i]; p.Model == model.ID() && p.MixerSHA256 == hash {
			return p, nil
		}
	}
	return nil, fmt.Errorf("unrecognized %s mixer executable SHA-256 %s", model.ID(), hash)
}

// InspectForModel runs offline executable and launcher verification for model.
// It may borrow instruction checks from another model's qualified profile that
// shares the exact same executable and launcher hashes, without granting that
// model's physical route layout; the returned profile may still have Runtime==nil.
func (c *Catalog) InspectForModel(model Model, image, launcher []byte) (*Profile, error) {
	profile, err := c.identify(model, image)
	if err != nil {
		return nil, err
	}
	if err := profile.VerifyLauncher(launcher); err != nil {
		return nil, err
	}
	var abi *Profile
	for i := range c.Profiles {
		p := &c.Profiles[i]
		if p.MixerSHA256 == profile.MixerSHA256 && p.LauncherSHA256 == profile.LauncherSHA256 && p.Runtime != nil {
			abi = p
			break
		}
	}
	if abi == nil {
		return nil, errors.New("no qualified executable ABI matches this release")
	}
	if err := abi.VerifyImage(image); err != nil {
		return nil, err
	}
	return profile, nil
}

// VerifyImage checks the executable's hash and every hook site's original
// instructions.
func (p *Profile) VerifyImage(image []byte) error {
	if sum(image) != p.MixerSHA256 {
		return errors.New("mixer executable differs from its profile hash")
	}
	runtime := p.Runtime
	if runtime == nil {
		return fmt.Errorf("%s is not runtime-qualified: %s", p.ID, p.Notes)
	}
	if err := runtime.validate(); err != nil {
		return err
	}
	model, err := ParseModel(p.Model)
	if err != nil {
		return err
	}
	if runtime.RouteLayout != model.RouteLayout() || runtime.SourceEnum != model.SourceEnum() {
		return errors.New("runtime routing does not match the device model")
	}
	for _, site := range runtime.Sites() {
		offset, err := site.OffsetValue()
		if err != nil {
			return err
		}
		found, err := imageBytes(image, uint64(offset))
		if err != nil {
			return err
		}
		want, err := site.Bytes()
		if err != nil {
			return err
		}
		if string(found) != string(want[:]) {
			return fmt.Errorf("%s: original instructions differ at %s", p.ID, site.Offset)
		}
	}
	return nil
}

// VerifyLauncher checks the audio launcher's hash.
func (p *Profile) VerifyLauncher(launcher []byte) error {
	if sum(launcher) != p.LauncherSHA256 {
		return fmt.Errorf("%s: audio launcher differs from its profile hash", p.ID)
	}
	return nil
}

// imageBytes reads 16 bytes at virtual address from an executable ELF64 PT_LOAD
// segment, translating vaddr -> file offset explicitly.
func imageBytes(image []byte, address uint64) ([]byte, error) {
	truncated := errors.New("truncated ELF image")
	read := func(offset, size uint64) ([]byte, error) {
		end, carry := bits.Add64(offset, size, 0)
		if carry != 0 {
			end = math.MaxUint64
		}
		if end > uint64(len(image)) {
			return nil, truncated
		}
		return image[offset:end], nil
	}
	u16At := func(offset uint64) (uint16, error) {
		b, err := read(offset, 2)
		if err != nil {
			return 0, err
		}
		return binary.LittleEndian.Uint16(b), nil
	}
	u32At := func(offset uint64) (uint32, error) {
		b, err := read(offset, 4)
		if err != nil {
			return 0, err
		}
		return binary.LittleEndian.Uint32(b), nil
	}
	u64At := func(offset uint64) (uint64, error) {
		b, err := read(offset, 8)
		if err != nil {
			return 0, err
		}
		return binary.LittleEndian.Uint64(b), nil
	}
	notPIE := errors.New("profile needs an AArch64 little-endian ELF64 PIE")
	magic, err := read(0, 6)
	if err != nil {
		return nil, err
	}
	if string(magic) != "\x7fELF\x02\x01" {
		return nil, notPIE
	}
	if kind, err := u16At(16); err != nil {
		return nil, err
	} else if kind != 3 {
		return nil, notPIE
	}
	if machine, err := u16At(18); err != nil {
		return nil, err
	} else if machine != 183 {
		return nil, notPIE
	}
	table, err := u64At(32)
	if err != nil {
		return nil, err
	}
	stride, err := u16At(54)
	if err != nil {
		return nil, err
	}
	count, err := u16At(56)
	if err != nil {
		return nil, err
	}
	if stride < 56 || count > 128 {
		return nil, errors.New("invalid ELF program header table")
	}
	for index := range uint64(count) {
		start, carry := bits.Add64(table, index*uint64(stride), 0)
		if carry != 0 {
			return nil, errors.New("ELF header overflow")
		}
		if _, err := read(start, 56); err != nil {
			return nil, err
		}
		kind, err := u32At(start)
		if err != nil {
			return nil, err
		}
		if kind != 1 {
			continue
		}
		flags, err := u32At(start + 4)
		if err != nil {
			return nil, err
		}
		if flags&1 == 0 {
			continue
		}
		file, err := u64At(start + 8)
		if err != nil {
			return nil, err
		}
		base, err := u64At(start + 16)
		if err != nil {
			return nil, err
		}
		size, err := u64At(start + 32)
		if err != nil {
			return nil, err
		}
		if address >= base {
			relative := address - base
			if end, carry := bits.Add64(relative, 16, 0); carry == 0 && end <= size {
				at, carry := bits.Add64(file, relative, 0)
				if carry != 0 {
					return nil, errors.New("ELF segment overflow")
				}
				return read(at, 16)
			}
		}
	}
	return nil, errors.New("hook site is outside executable file-backed ELF code")
}
