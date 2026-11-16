package app

// dev_page.rs, the parsing half. `RC2FX_DEV_PAGE`: open the app on a chosen
// page, for development and screenshots. Unset, the app opens as usual.
//
// Values: `home`, `settings`, `diagnostics`, `installer`, `repair`, or
// `channel:<name or id>[:<effect>]`, where the effect is one of `highpass`,
// `suppression`, `eq`, `compressor`, `gain` or `limiter`. Names match
// case-insensitively, e.g. `channel:mic 1:suppression`.
//
// It only navigates: it never changes settings or sends anything to the
// mixer beyond what opening that page normally does. (openDevPage, in
// dev_page.go, opens it.)
//
// This file does not import gk: its functions are tested with plain `go test`.

import (
	"fmt"
	"os"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// DevPageKind is DevPage's variant.
type DevPageKind int

const (
	DevPageHome DevPageKind = iota
	DevPageSettings
	DevPageDiagnostics
	DevPageInstaller
	DevPageRepair
	DevPageChannel
)

// DevPage is dev_page.rs DevPage. Name and Effect are for DevPageChannel; an
// empty Effect is None.
type DevPage struct {
	Kind   DevPageKind
	Name   string
	Effect core.Effect
}

// DevPageFromEnv is DevPage::from_env.
func DevPageFromEnv() *DevPage {
	value, ok := os.LookupEnv("RC2FX_DEV_PAGE")
	if !ok {
		return nil
	}
	page, err := parseDevPage(value)
	if err != nil {
		fmt.Fprintf(os.Stderr, "RC2FX_DEV_PAGE ignored: %s\n", err)
		return nil
	}
	return &page
}

// needsMixer: channels need the mixer's state; the other pages can open at
// once.
func (p DevPage) needsMixer() bool {
	return p.Kind == DevPageChannel || p.Kind == DevPageHome
}

// parseDevPage is dev_page.rs parse.
func parseDevPage(value string) (DevPage, error) {
	value = strings.TrimSpace(value)
	// splitn(3, ':')
	parts := strings.SplitN(value, ":", 3)
	page := asciiLower(strings.TrimSpace(parts[0]))
	var out DevPage
	switch page {
	case "home":
		out = DevPage{Kind: DevPageHome}
	case "settings":
		out = DevPage{Kind: DevPageSettings}
	case "diagnostics":
		out = DevPage{Kind: DevPageDiagnostics}
	case "installer":
		out = DevPage{Kind: DevPageInstaller}
	case "repair":
		out = DevPage{Kind: DevPageRepair}
	case "channel":
		name := ""
		if len(parts) > 1 {
			name = strings.TrimSpace(parts[1])
		}
		if name == "" {
			return DevPage{}, fmt.Errorf("`channel:` needs a channel name or id")
		}
		out = DevPage{Kind: DevPageChannel, Name: name}
		if len(parts) > 2 {
			e := asciiLower(strings.TrimSpace(parts[2]))
			switch e {
			case "highpass":
				out.Effect = core.EffectHighpass
			case "suppression":
				out.Effect = core.EffectSuppression
			case "eq":
				out.Effect = core.EffectEQ
			case "compressor":
				out.Effect = core.EffectCompressor
			case "gain":
				out.Effect = core.EffectGain
			case "limiter":
				out.Effect = core.EffectLimiter
			default:
				return DevPage{}, fmt.Errorf("unknown effect `%s`", e)
			}
		}
		return out, nil
	default:
		return DevPage{}, fmt.Errorf("unknown page `%s`", page)
	}
	if len(parts) > 1 {
		return DevPage{}, fmt.Errorf("`%s` has extra parts", value)
	}
	return out, nil
}

// findChannel is the channel whose name or id matches, ignoring (ASCII) case.
// names holds [id, name] pairs.
func findChannel(names [][2]string, wanted string) (int, bool) {
	for i, n := range names {
		if asciiEqualFold(n[0], wanted) || asciiEqualFold(n[1], wanted) {
			return i, true
		}
	}
	return 0, false
}

// asciiEqualFold is str::eq_ignore_ascii_case.
func asciiEqualFold(a, b string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := 0; i < len(a); i++ {
		x, y := a[i], b[i]
		if 'A' <= x && x <= 'Z' {
			x += 'a' - 'A'
		}
		if 'A' <= y && y <= 'Z' {
			y += 'a' - 'A'
		}
		if x != y {
			return false
		}
	}
	return true
}

// asciiLower is str::to_ascii_lowercase.
func asciiLower(s string) string {
	b := []byte(s)
	for i, c := range b {
		if 'A' <= c && c <= 'Z' {
			b[i] = c + 'a' - 'A'
		}
	}
	return string(b)
}
