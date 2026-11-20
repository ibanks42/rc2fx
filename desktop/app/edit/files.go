package edit

import (
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync/atomic"
	"syscall"
	"unicode"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// IOMessage formats an OS error with capitalized strerror text and "(os error N)",
// leaving non-errno errors unchanged.
func IOMessage(err error) string {
	var errno syscall.Errno
	if !errors.As(err, &errno) {
		return err.Error()
	}
	text := errno.Error()
	if text != "" {
		text = strings.ToUpper(text[:1]) + text[1:]
	}
	return fmt.Sprintf("%s (os error %d)", text, int(errno))
}

// ImportTarget snapshots the channel and EQ state when an import starts so
// late-arriving file dialog results are rejected if the target changed.
type ImportTarget struct {
	ID         string
	Channels   uint8
	EQ         core.EQSettings
	Generation uint64
}

// CaptureImportTarget snapshots target at the given generation.
func CaptureImportTarget(target *core.Target, generation uint64) ImportTarget {
	return ImportTarget{ID: target.ID, Channels: target.Channels, EQ: target.EQ.Clone(), Generation: generation}
}

// Matches reports whether target still matches the captured state.
func (i ImportTarget) Matches(target *core.Target, generation uint64) bool {
	return i.Generation == generation && i.ID == target.ID && i.Channels == target.Channels &&
		i.EQ.Equal(target.EQ)
}

// ReadPreset reads a regular EQ preset or settings file up to core.MaxFileBytes.
func ReadPreset(path string) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", errors.New(IOMessage(err))
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return "", errors.New(IOMessage(err))
	}
	if !info.Mode().IsRegular() {
		return "", errors.New("Choose an EQ preset file")
	}
	bytes, err := io.ReadAll(io.LimitReader(file, core.MaxFileBytes+1))
	if err != nil {
		return "", errors.New(IOMessage(err))
	}
	if len(bytes) > core.MaxFileBytes {
		return "", errors.New("Preset file exceeds 4 MiB")
	}
	return DecodeText(bytes)
}

// DecodeText decodes BOM-marked UTF-16 or UTF-8 preset bytes into a string,
// stripping leading UTF-8 BOMs.
func DecodeText(bytes []byte) (string, error) {
	if len(bytes) >= 2 && ((bytes[0] == 0xff && bytes[1] == 0xfe) || (bytes[0] == 0xfe && bytes[1] == 0xff)) {
		if len(bytes)%2 != 0 {
			return "", errors.New("Truncated UTF-16 preset")
		}
		little := bytes[0] == 0xff
		units := make([]uint16, 0, len(bytes)/2-1)
		for i := 2; i+1 < len(bytes); i += 2 {
			if little {
				units = append(units, uint16(bytes[i])|uint16(bytes[i+1])<<8)
			} else {
				units = append(units, uint16(bytes[i])<<8|uint16(bytes[i+1]))
			}
		}
		text, ok := fromUTF16(units)
		if !ok {
			return "", errors.New("Invalid UTF-16 preset")
		}
		return text, nil
	}
	if !utf8.Valid(bytes) {
		return "", errors.New("Preset must be UTF-8 or UTF-16 text")
	}
	return trimBOMs(string(bytes)), nil
}

// fromUTF16 decodes UTF-16 strictly, rejecting unpaired surrogates instead of
// substituting U+FFFD like unicode/utf16.Decode.
func fromUTF16(units []uint16) (string, bool) {
	var b strings.Builder
	for i := 0; i < len(units); i++ {
		u := units[i]
		switch {
		case u < 0xd800 || u > 0xdfff:
			b.WriteRune(rune(u))
		case u <= 0xdbff && i+1 < len(units) && units[i+1] >= 0xdc00 && units[i+1] <= 0xdfff:
			b.WriteRune(rune(u-0xd800)<<10 | rune(units[i+1]-0xdc00) + 0x10000)
			i++
		default:
			return "", false
		}
	}
	return b.String(), true
}

func trimBOMs(text string) string {
	for strings.HasPrefix(text, "\ufeff") {
		text = text[len("\ufeff"):]
	}
	return text
}

// ParseImport parses an EasyEffects JSON preset or Equalizer APO / AutoEQ text.
func ParseImport(text string, options core.ImportOptions) (core.ImportResult, error) {
	text = strings.TrimLeftFunc(trimBOMs(text), unicode.IsSpace)
	if strings.HasPrefix(text, "{") {
		return core.ImportEasyEffects(text, options)
	}
	eq, err := core.ImportAPO(text)
	if err != nil {
		return core.ImportResult{}, err
	}
	return core.ImportResult{
		Kind: core.ImportResultReady,
		EQ:   eq,
		Source: core.CandidateSummary{
			Key:       "Equalizer APO / AutoEQ",
			Pipeline:  core.PipelineOutput,
			Instance:  "parametric text",
			BandCount: len(eq.Bands),
		},
		OtherEffects: 0,
	}, nil
}

var tempSequence atomic.Uint64

// WriteExport writes text to a sibling temporary file, fsyncs it, and renames
// it over path so failures leave any existing file intact.
func WriteExport(path, text string) error {
	parent := filepath.Dir(path)
	if path == "" || parent == path {
		return errors.New("Choose a file location")
	}
	var temporary string
	var file *os.File
	for range 32 {
		candidate := filepath.Join(parent, fmt.Sprintf(".rc2fx-export-%d-%d.tmp", os.Getpid(), tempSequence.Add(1)-1))
		f, err := os.OpenFile(candidate, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o666)
		if err == nil {
			temporary, file = candidate, f
			break
		}
		if errors.Is(err, os.ErrExist) {
			continue
		}
		return errors.New(IOMessage(err))
	}
	if file == nil {
		return errors.New("Could not create export file")
	}
	_, err := file.WriteString(text)
	if err == nil {
		err = file.Sync()
	}
	file.Close()
	if err != nil {
		err = errors.New("Writing export: " + IOMessage(err))
	} else if rerr := os.Rename(temporary, path); rerr != nil {
		err = errors.New("Replacing export: " + IOMessage(rerr))
	}
	if err != nil {
		os.Remove(temporary)
	}
	return err
}

// DraftSnapshot captures session state before opening a settings file dialog so
// the import is rejected if the draft, selection, saved config, or connection changes.
type DraftSnapshot struct {
	generation      uint64
	targets         []core.Target
	selectedTargets []string
	saved           *core.Config
}

// CaptureDraft snapshots the session's draft, selection, and saved config.
func CaptureDraft(session *core.Session, generation uint64) DraftSnapshot {
	snapshot := DraftSnapshot{
		generation:      generation,
		targets:         core.CloneTargets(session.Draft),
		selectedTargets: slices.Clone(session.SelectionDraft),
	}
	if session.Saved != nil {
		saved := session.Saved.Config.Clone()
		snapshot.saved = &saved
	}
	return snapshot
}

// Matches reports whether session still matches the captured snapshot.
func (d DraftSnapshot) Matches(session *core.Session, generation uint64) bool {
	if d.generation != generation || !core.TargetsEqual(d.targets, session.Draft) ||
		!slices.Equal(d.selectedTargets, session.SelectionDraft) {
		return false
	}
	if d.saved == nil || session.Saved == nil {
		return d.saved == nil && session.Saved == nil
	}
	return d.saved.Equal(session.Saved.Config)
}

// ImportedTargets validates config and returns its targets for the draft.
// When connected (live != nil), it reorders targets to match live hardware and
// rejects files targeting a different channel layout.
func ImportedTargets(config core.Config, live *core.Config) ([]core.Target, error) {
	if err := config.Validate(); err != nil {
		return nil, err
	}
	if live == nil {
		return config.Targets, nil
	}
	if len(live.Targets) != len(config.Targets) {
		return nil, errors.New("This file uses a different set of mixer channels.")
	}
	// Keep the live engine's channel order; file revisions never overwrite the session revision.
	targets := make([]core.Target, 0, len(live.Targets))
	for _, target := range live.Targets {
		at := slices.IndexFunc(config.Targets, func(t core.Target) bool {
			return t.ID == target.ID && t.Kind == target.Kind && t.Channels == target.Channels
		})
		if at < 0 {
			return nil, fmt.Errorf("This file does not match mixer channel %s.", target.Name)
		}
		targets = append(targets, config.Targets[at].Clone())
	}
	return targets, nil
}

// SettingsFile encodes the session's current draft as a formatted settings file.
func SettingsFile(session *core.Session) (string, error) {
	config := core.Config{
		SchemaVersion:   core.SchemaVersion,
		Revision:        1,
		SelectedTargets: slices.Clone(session.SelectionDraft),
		Targets:         core.CloneTargets(session.Draft),
	}
	if err := config.Validate(); err != nil {
		return "", err
	}
	data, err := core.EncodePretty(config)
	if err != nil {
		return "", err
	}
	return string(data), nil
}

// HomeDirectory returns the initial directory for file dialogs.
func HomeDirectory() string {
	if v, ok := os.LookupEnv("USERPROFILE"); ok {
		return v
	}
	if v, ok := os.LookupEnv("HOME"); ok {
		return v
	}
	return "."
}
