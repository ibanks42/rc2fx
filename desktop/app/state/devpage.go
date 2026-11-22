package state

// Parsing for RC2FX_DEV_PAGE, which opens the app on a specific page for
// development and screenshots without modifying settings.
//
// Supported values: home, settings, diagnostics, installer, repair,
// channel:<name or id>[:<effect>] (highpass, suppression, eq, compressor,
// gain, limiter), and flow:<state> (dev builds only).

import (
	"fmt"
	"os"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/internal/common"
)

type DevPageKind int

const (
	DevPageHome DevPageKind = iota
	DevPageSettings
	DevPageDiagnostics
	DevPageInstaller
	DevPageRepair
	DevPageChannel
	// DevPageFlow renders a synthetic guided-flow state (dev builds only).
	DevPageFlow
)

// DevPage is the parsed RC2FX_DEV_PAGE target. Name applies to DevPageChannel
// and DevPageFlow; Effect is optional for DevPageChannel.
type DevPage struct {
	Kind   DevPageKind
	Name   string
	Effect core.Effect
}

// DevPageFromEnv reads and parses RC2FX_DEV_PAGE, returning nil if unset or invalid.
func DevPageFromEnv(allowFlows bool) *DevPage {
	value, ok := os.LookupEnv("RC2FX_DEV_PAGE")
	if !ok {
		return nil
	}
	page, err := ParseDevPage(value, allowFlows)
	if err != nil {
		fmt.Fprintf(os.Stderr, "RC2FX_DEV_PAGE ignored: %s\n", err)
		return nil
	}
	return &page
}

// NeedsMixer reports whether opening the page requires an active mixer connection.
func (p DevPage) NeedsMixer() bool {
	return p.Kind == DevPageChannel || p.Kind == DevPageHome
}

// ParseDevPage parses an RC2FX_DEV_PAGE value.
func ParseDevPage(value string, allowFlows bool) (DevPage, error) {
	value = strings.TrimSpace(value)
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
	case "flow":
		name := ""
		if len(parts) > 1 {
			name = strings.TrimSpace(strings.Join(parts[1:], ":"))
		}
		switch {
		case !allowFlows:
			return DevPage{}, fmt.Errorf("`flow:` pages are in development builds only")
		case name == "":
			return DevPage{}, fmt.Errorf("`flow:` needs a state, such as `flow:B4`")
		}
		return DevPage{Kind: DevPageFlow, Name: name}, nil
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

// FindChannel returns the index of the first [id, name] entry matching wanted
// (ASCII case-insensitive).
func FindChannel(names [][2]string, wanted string) (int, bool) {
	for i, n := range names {
		if common.ASCIIEqualFold(n[0], wanted) || common.ASCIIEqualFold(n[1], wanted) {
			return i, true
		}
	}
	return 0, false
}

func asciiLower(s string) string {
	b := []byte(s)
	for i, c := range b {
		if 'A' <= c && c <= 'Z' {
			b[i] = c + 'a' - 'A'
		}
	}
	return string(b)
}
