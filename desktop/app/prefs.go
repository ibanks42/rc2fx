package app

// The view preferences file and the connection screen's decisions. This file
// does not import gk: its functions are tested with plain `go test`.

import (
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/app/edit"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
)

func preferencesDir() (string, bool) {
	var base string
	switch runtime.GOOS {
	case "windows":
		v, ok := os.LookupEnv("LOCALAPPDATA")
		if !ok {
			return "", false
		}
		base = v
	case "darwin":
		home, ok := os.LookupEnv("HOME")
		if !ok {
			return "", false
		}
		base = filepath.Join(home, "Library/Application Support")
	default:
		if v, ok := os.LookupEnv("XDG_CONFIG_HOME"); ok && filepath.IsAbs(v) {
			base = v
		} else if home, ok := os.LookupEnv("HOME"); ok {
			base = filepath.Join(home, ".config")
		} else {
			return "", false
		}
	}
	return filepath.Join(base, "RC2FX"), true
}

// viewPath is the view preferences file: what the channel list shows and
// whether the Telemetry section is on.
func viewPath() (string, bool) {
	dir, ok := preferencesDir()
	if !ok {
		return "", false
	}
	return filepath.Join(dir, "view.json"), true
}

// viewPrefs is the file's top-level object. json writes its keys sorted,
// as the file has always been written.
type viewPrefs map[string]core.Value

func rememberedView() viewPrefs {
	path, ok := viewPath()
	if !ok {
		return readView("")
	}
	return readView(path)
}

// readView reads the view preferences. An empty path, or anything
// unreadable, over 4096 bytes, not strict JSON or not an object, is an empty
// view.
func readView(path string) viewPrefs {
	view := viewPrefs{}
	if path == "" {
		return view
	}
	data, err := os.ReadFile(path)
	if err != nil || len(data) > 4096 {
		return view
	}
	value, err := core.ParseJSON(string(data))
	if err != nil {
		return view
	}
	members, ok := value.AsObject()
	if !ok {
		return view
	}
	for _, m := range members {
		view[m.Key] = m.Value
	}
	return view
}

// json is the object as compact JSON, keys in sorted order.
func (v viewPrefs) json() string {
	keys := make([]string, 0, len(v))
	for k := range v {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	members := make([]core.Member, 0, len(keys))
	for _, k := range keys {
		members = append(members, core.Member{Key: k, Value: v[k]})
	}
	return core.ObjectValue(members...).String()
}

func (v viewPrefs) boolean(key string) bool {
	value, ok := v[key]
	if !ok {
		return false
	}
	b, ok := value.AsBool()
	return ok && b
}

// rememberView saves one view preference, keeping the others.
func rememberView(key string, value core.Value) {
	view := rememberedView()
	view[key] = value
	path, ok := viewPath()
	if !ok {
		return
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o777); err != nil {
		return
	}
	// A preference that fails to save only costs the choice next time.
	_ = edit.WriteExport(path, view.json())
}

// rememberedTelemetry is whether the Telemetry section was last on.
func rememberedTelemetry() bool { return rememberedView().boolean("showTelemetry") }

func rememberTelemetry(on bool) { rememberView("showTelemetry", core.BoolValue(on)) }

// rememberedAppearance is the appearance last chosen; Dusk when none was.
func rememberedAppearance() Appearance {
	if value, ok := rememberedView()["appearance"]; ok {
		if key, ok := value.AsString(); ok {
			if a, ok := AppearanceFromKey(key); ok {
				return a
			}
		}
	}
	return AppearanceDusk
}

func rememberAppearance(a Appearance) { rememberView("appearance", core.StringValue(a.Key())) }

func rememberedDuo() bool { return duoEnabled(rememberedView()) }

func duoEnabled(view viewPrefs) bool { return view.boolean("experimentalDuoFlash") }

// rememberDuo reports a failed save, unlike view-only preferences, rather than
// implying that the experimental opt-in will survive restarting the app.
func rememberDuo(allow bool) error {
	path, ok := viewPath()
	if !ok {
		return errors.New("No application settings folder")
	}
	return saveDuo(path, allow)
}

func saveDuo(path string, allow bool) error { return saveFlag(path, "experimentalDuoFlash", allow) }

// rememberedFirstChannelHint is whether the first channel's hint was
// dismissed or completed: it then never shows again.
func rememberedFirstChannelHint() bool { return rememberedView().boolean("firstChannelHintDone") }

// rememberFirstChannelHint records that the hint is done. A failed save
// only means it may show once more after a restart.
func rememberFirstChannelHint() {
	if path, ok := viewPath(); ok {
		_ = saveFlag(path, "firstChannelHintDone", true)
	}
}

// rememberedDeveloper is whether Developer mode was left on. It only shows
// developer controls in Settings; it grants nothing by itself.
func rememberedDeveloper() bool { return developerEnabled(rememberedView()) }

func developerEnabled(view viewPrefs) bool { return view.boolean("developerMode") }

// rememberDeveloper reports a failed save, like the Duo opt-in.
func rememberDeveloper(on bool) error {
	path, ok := viewPath()
	if !ok {
		return errors.New("No application settings folder")
	}
	return saveFlag(path, "developerMode", on)
}

func saveFlag(path, key string, on bool) error {
	dir := filepath.Dir(path)
	if path == "" || dir == path {
		return errors.New("No settings parent")
	}
	if err := os.MkdirAll(dir, 0o777); err != nil {
		return errors.New(edit.IOMessage(err))
	}
	view := readView(path)
	view[key] = core.BoolValue(on)
	return edit.WriteExport(path, view.json())
}

// USBUndo is how to remove the rule again, as the screen shows and copies it.
const USBUndo = "sudo rm " + helper.USBRulePath

// usbPermission is whether Linux refused the app the mixer's USB 1 control
// interface: the one failure the udev rule fixes (helper.USBPermission).
func usbPermission(lastResult string) bool {
	return strings.HasPrefix(lastResult, helper.USBPermission)
}

// mixerFound: this exact discovery failure is emitted only after finding one
// supported mixer without the RC2FX interface (control's controlInterface).
// Do not infer "found" from arbitrary failures/timeouts or a substring of an
// error.
func mixerFound(lastResult string) bool {
	return lastResult == "The mixer is connected, but RC2FX isn't running on it"
}
