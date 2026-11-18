package installer

// The RC2FX runtime bundle, the files it adds to the image, settings
// backups and readiness.

import (
	"fmt"
	"path/filepath"
	"strconv"
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer/embed"
	"github.com/ibanks42/rc2fx/desktop/installer/internal/sjson"
)

// PayloadNames are the payload's files, in the order an embedded envelope
// carries them.
var PayloadNames = embed.PayloadNames

// Bundle is a payload as name → bytes.
type Bundle = map[string][]byte

// BundleFiles reads the runtime bundle from `runtime/` (a release folder)
// or `target/mixer` (a source checkout), with scripts from `mixer/scripts`.
func BundleFiles(root string) (Bundle, error) {
	files, err := embed.Read(root, false)
	if err != nil {
		return nil, Error(err.Error())
	}
	out := make(Bundle, len(files))
	for _, f := range files {
		out[f.Name] = f.Data
	}
	return out, nil
}

// digest is what a prepared firmware records as its RC2FX runtime.
func digest(files Bundle) string {
	var manifest strings.Builder
	for _, n := range PayloadNames {
		fmt.Fprintf(&manifest, "%s  %s\n", sum(files[n]), n)
	}
	return sum([]byte(manifest.String()))
}

// view is the payload's files in pinned order, for the shared checks.
func view(files Bundle) embed.Files {
	out := make(embed.Files, 0, len(PayloadNames))
	for _, n := range PayloadNames {
		out = append(out, embed.File{Name: n, Data: files[n]})
	}
	return out
}

// catalogOf is the catalog this payload supports, after the shared payload
// checks.
func catalogOf(files Bundle) (*Catalog, error) {
	if err := embed.Validate(view(files)); err != nil {
		return nil, Error(err.Error())
	}
	var catalog Catalog
	if err := sjson.Unmarshal(files["firmware-profiles.json"], &catalog); err != nil {
		return nil, err
	}
	return &catalog, nil
}

// profileOf is the runtime profile the mixer's state is judged against.
func profileOf(files Bundle) RuntimeProfile {
	var catalog Catalog
	if sjson.Unmarshal(files["firmware-profiles.json"], &catalog) != nil {
		return RuntimeProfile{}
	}
	for i := range catalog.Profiles {
		if p := &catalog.Profiles[i]; supported(p) {
			if p.Runtime == nil {
				return RuntimeProfile{}
			}
			return *p.Runtime
		}
	}
	return RuntimeProfile{}
}

// service is whether RC2FX's state says this payload's runtime is ready.
func service(files Bundle, state []byte) Service {
	profile := profileOf(files)
	if why := readinessError(state, &profile); why != "" {
		return Service{Kind: ServiceStarting, Detail: why}
	}
	return Service{Kind: ServiceReady}
}

// imageEntries is what RC2FX adds to the root filesystem: the same layout
// `install.sh` creates on a running mixer, with autostart, and optional
// saved settings. Returns the entries and the runtime digest (recorded as
// bundle.sha256).
func imageEntries(files Bundle, profile *Profile, settings []byte) ([]Entry, string, error) {
	c, err := catalogOf(files)
	if err != nil {
		return nil, "", err
	}
	if settings != nil {
		value, err := sjson.Value(settings)
		if err != nil {
			return nil, "", err
		}
		config := value
		if object, ok := value.(map[string]any); ok {
			if inner, ok := object["config"]; ok {
				config = inner
			}
		}
		var targets []any
		if object, ok := config.(map[string]any); ok {
			targets, ok = object["targets"].([]any)
			if !ok {
				targets = nil
			}
		}
		if targets == nil {
			return nil, "", Error("settings have no target identities")
		}
		duo := profile.Model == firmware.ModelDuo.ID()
		for _, t := range targets {
			object, _ := t.(map[string]any)
			id, ok := object["id"].(string)
			if !ok || strings.HasPrefix(id, "duo-") != duo {
				return nil, "", Error("saved settings belong to a different mixer model; refusing to move audio paths")
			}
		}
	}
	found := false
	for i := range c.Profiles {
		p := &c.Profiles[i]
		if p.ID == profile.ID && p.Model == profile.Model && embed.SameRuntime(p.Runtime, profile.Runtime) &&
			p.MixerSHA == profile.MixerSHA && p.LauncherSHA == profile.LauncherSHA && supported(p) {
			found = true
			break
		}
	}
	if !found {
		return nil, "", errorf("the bundled runtime does not support firmware %s", profile.Version)
	}
	d := digest(files)
	entries := []Entry{
		{Kind: EntryDir, Path: "opt/rc2fx", Mode: 0o700},
		{Kind: EntryDir, Path: "opt/rc2fx/data", Mode: 0o700},
		{Kind: EntryDir, Path: "opt/rc2fx/logs", Mode: 0o700},
	}
	for _, n := range PayloadNames {
		mode := uint32(0o600)
		if embed.IsBinary(n) || embed.IsScript(n) {
			mode = 0o700
		}
		entries = append(entries, Entry{Kind: EntryFile, Path: "opt/rc2fx/" + n, Mode: mode, Data: files[n]})
	}
	if profile.Model == firmware.ModelDuo.ID() {
		entries = append(entries, Entry{Kind: EntryFile, Path: "opt/rc2fx/experimental-duo", Mode: 0o600, Data: []byte("1\n")})
	}
	entries = append(entries, Entry{Kind: EntryFile, Path: "opt/rc2fx/bundle.sha256", Mode: 0o600, Data: []byte(d + "\n")})
	if settings != nil {
		entries = append(entries, Entry{Kind: EntryFile, Path: "opt/rc2fx/data/state.json", Mode: 0o600, Data: settings})
	}
	// RØDE's launcher starts the mixer by name with /usr/sbin ahead of
	// /usr/bin, so this runs in its place and execs the stock executable with
	// RC2FX injected from the first mixer start of every boot.
	entries = append(entries, Entry{Kind: EntryFile, Path: "usr/sbin/rc_audio_mixer", Mode: 0o755, Data: files["rc2fx-shim.sh"]})
	// The same for RØDE's display loop, which starts rc_gui_juce by name with
	// /usr/sbin first: the GUI shim hands over to `rc2fx.sh gui-launch`,
	// which always execs the stock GUI, with the partial-conversion module
	// only when every check passes.
	entries = append(entries, Entry{Kind: EntryFile, Path: "usr/sbin/rc_gui_juce", Mode: 0o755, Data: files["rc2fx-gui-shim.sh"]})
	return entries, d, nil
}

// backupSettings is the settings file a backup saved, unchanged since. The
// record is the desktop user's to edit, so its path must name a file
// directly in work.
func backupSettings(work string, b *BackupRecord) ([]byte, error) {
	path := b.File
	if filepath.Dir(path) != filepath.Clean(work) || filepath.Base(path) == "" || strings.HasSuffix(path, string(filepath.Separator)) ||
		filepath.Base(path) == ".." || filepath.Base(path) == "." {
		return nil, Error("the settings backup is not in the installer's folder")
	}
	file, err := openWorkFile(path)
	if err != nil {
		return nil, err
	}
	defer file.Close()
	data, err := readLimited(file, maxSettings+1)
	if err != nil {
		return nil, err
	}
	if sum(data) != b.SHA {
		return nil, Error("the settings backup changed since it was saved")
	}
	if err := checkSettings(data); err != nil {
		return nil, err
	}
	return data, nil
}

func checkSettings(data []byte) error {
	if len(data) > maxSettings {
		return Error("the settings file exceeds 16 MiB")
	}
	value, err := sjson.Value(data)
	if err != nil {
		return errorf("the settings file is not valid JSON: %v", err)
	}
	if _, ok := value.(map[string]any); !ok {
		return Error("the settings file is not an RC2FX settings object")
	}
	return nil
}

// saveSettings keeps RC2FX's settings file, read over USB by the app, for
// the mixer serial.
func (e *Engine) saveSettings(serial, settings string) (*BackupRecord, error) {
	if serial == "" {
		return nil, Error("connect the mixer so its settings can be matched to it")
	}
	e.step("backup", "Saving the mixer's RC2FX settings")
	data := []byte(settings)
	if len(data) > maxSettings {
		return nil, errorf("RC2FX's settings exceed %d bytes", maxSettings)
	}
	if err := checkSettings(data); err != nil {
		return nil, err
	}
	var stamp strings.Builder
	for _, r := range utc() {
		if r < 0x80 && (unicode.IsLetter(r) || unicode.IsDigit(r)) {
			stamp.WriteRune(r)
		}
	}
	name := fmt.Sprintf("settings-%s-%s.json", sum([]byte(serial))[:12], stamp.String())
	dest := filepath.Join(e.Work, name)
	if err := atomicFile(dest, data); err != nil {
		return nil, err
	}
	record := BackupRecord{Serial: serial, File: dest, SHA: sum(data), Created: utc()}
	saved, err := e.load()
	if err != nil {
		return nil, err
	}
	saved.Backups = append(saved.Backups, record)
	if err := e.save(saved); err != nil {
		return nil, err
	}
	return &record, nil
}

type engineView struct {
	_               struct{} `serde:"default"`
	Mode            string   `json:"mode"`
	Status          string   `json:"status"`
	AudioAdvancing  bool     `json:"audioAdvancing"`
	AppliedRevision *uint64  `json:"appliedRevision"`
	SampleRate      uint32   `json:"sampleRate"`
	BlockSize       uint32   `json:"blockSize"`
	Message         string   `json:"message"`
}

type configView struct {
	_        struct{} `serde:"default"`
	Revision *uint64  `json:"revision"`
}

type stateView struct {
	_      struct{}   `serde:"default"`
	Engine engineView `json:"engine"`
	Config configView `json:"config"`
}

func readinessError(body []byte, r *RuntimeProfile) string {
	var s stateView
	if err := sjson.Unmarshal(body, &s); err != nil {
		return fmt.Sprintf("invalid state JSON: %v", err)
	}
	a := &s.Engine
	sameRevision := a.AppliedRevision != nil && s.Config.Revision != nil && *a.AppliedRevision == *s.Config.Revision
	if a.Mode == "native" && a.Status == "running" && a.AudioAdvancing && sameRevision &&
		a.SampleRate == r.SampleRate && a.BlockSize == r.BlockFrames {
		return ""
	}
	// While starting, the engine's own message says what it waits for.
	if a.Status != "running" && a.Message != "" {
		return a.Message
	}
	revision := func(v *uint64) string {
		if v == nil {
			return "missing"
		}
		return strconv.FormatUint(*v, 10)
	}
	return fmt.Sprintf("engine mode=%s status=%s advancing=%t appliedRevision=%s configRevision=%s sampleRate=%d blockSize=%d (expected %d/%d)",
		debugString(a.Mode), debugString(a.Status), a.AudioAdvancing, revision(a.AppliedRevision), revision(s.Config.Revision),
		a.SampleRate, a.BlockSize, r.SampleRate, r.BlockFrames)
}

// debugString quotes s, escaping quotes, backslashes, \n \r \t \0, and other
// unprintable runes as \u{hex}.
func debugString(s string) string {
	var out strings.Builder
	out.WriteByte('"')
	for _, r := range s {
		switch r {
		case '"':
			out.WriteString(`\"`)
		case '\\':
			out.WriteString(`\\`)
		case '\n':
			out.WriteString(`\n`)
		case '\r':
			out.WriteString(`\r`)
		case '\t':
			out.WriteString(`\t`)
		case 0:
			out.WriteString(`\0`)
		default:
			if r == utf8.RuneError || unicode.IsControl(r) || (!unicode.IsPrint(r) && r != ' ') {
				fmt.Fprintf(&out, `\u{%x}`, r)
			} else {
				out.WriteRune(r)
			}
		}
	}
	out.WriteByte('"')
	return out.String()
}
