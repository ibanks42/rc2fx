package state

// Persisted view preferences (view.json) and connection-screen classifiers.

import (
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/app/edit"
	"github.com/ibanks42/rc2fx/desktop/app/theme"
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

func viewPath() (string, bool) {
	dir, ok := preferencesDir()
	if !ok {
		return "", false
	}
	return filepath.Join(dir, "view.json"), true
}

type viewPrefs map[string]core.Value

func rememberedView() viewPrefs {
	path, ok := viewPath()
	if !ok {
		return readView("")
	}
	return readView(path)
}

// readView parses view.json, returning an empty map on missing, oversized
// (>4096 bytes), or malformed input.
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

// json serializes viewPrefs as compact JSON with sorted keys.
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
	_ = edit.WriteExport(path, view.json())
}

func RememberedTelemetry() bool { return rememberedView().boolean("showTelemetry") }

func RememberTelemetry(on bool) { rememberView("showTelemetry", core.BoolValue(on)) }

// RememberedAppearance returns the saved theme appearance, defaulting to Dusk.
func RememberedAppearance() theme.Appearance {
	if value, ok := rememberedView()["appearance"]; ok {
		if key, ok := value.AsString(); ok {
			if a, ok := theme.AppearanceFromKey(key); ok {
				return a
			}
		}
	}
	return theme.AppearanceDusk
}

func RememberAppearance(a theme.Appearance) { rememberView("appearance", core.StringValue(a.Key())) }

func RememberedDuo() bool { return duoEnabled(rememberedView()) }

func duoEnabled(view viewPrefs) bool { return view.boolean("experimentalDuoFlash") }

// RememberDuo persists the experimental Duo opt-in and returns any write error
// so the caller doesn't silently imply the choice will survive a restart.
func RememberDuo(allow bool) error {
	path, ok := viewPath()
	if !ok {
		return errors.New("No application settings folder")
	}
	return saveDuo(path, allow)
}

func saveDuo(path string, allow bool) error { return saveFlag(path, "experimentalDuoFlash", allow) }

func RememberedFirstChannelHint() bool { return rememberedView().boolean("firstChannelHintDone") }

func RememberFirstChannelHint() {
	if path, ok := viewPath(); ok {
		_ = saveFlag(path, "firstChannelHintDone", true)
	}
}

func RememberedDeveloper() bool { return developerEnabled(rememberedView()) }

func developerEnabled(view viewPrefs) bool { return view.boolean("developerMode") }

func RememberDeveloper(on bool) error {
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

const USBUndo = "sudo rm " + helper.USBRulePath

// USBPermission reports whether lastResult is a Linux USB permission denial
// fixable by installing the udev rule.
func USBPermission(lastResult string) bool {
	return strings.HasPrefix(lastResult, helper.USBPermission)
}

// MixerFound matches only the exact discovery message emitted when a supported
// mixer is attached without RC2FX running (do not match arbitrary errors).
func MixerFound(lastResult string) bool {
	return lastResult == "The mixer is connected, but RC2FX isn't running on it"
}
