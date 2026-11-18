package app

// The parsing half of the dev page switch (dev_page.go navigates).
// `RC2FX_DEV_PAGE`: open the app on a chosen page, for development and
// screenshots. Unset, the app opens as usual.
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
// Development builds (`-tags rc2fxdev`) also take `flow:<state>`: one state
// of the guided flows, drawn from made-up data with no mixer, helper or
// network, for comparing with the mock-up (flow_fixtures_on.go lists them,
// e.g. `flow:B4`). Other builds refuse it.
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
	// DevPageFlow: Name is a guided-flow state (development builds only).
	DevPageFlow
)

// DevPage is the page RC2FX_DEV_PAGE asks for. Name and Effect are for
// DevPageChannel (Name also for DevPageFlow); an empty Effect is none.
type DevPage struct {
	Kind   DevPageKind
	Name   string
	Effect core.Effect
}

// DevPageFromEnv is the page RC2FX_DEV_PAGE asks for; nil when it is unset
// or does not parse (which is reported on stderr).
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

// parseDevPage parses an RC2FX_DEV_PAGE value.
func parseDevPage(value string) (DevPage, error) {
	value = strings.TrimSpace(value)
	// At most three parts: page, channel, effect.
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
		case !devFlows:
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

// asciiEqualFold compares a and b ignoring the case of ASCII letters only.
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

// asciiLower lowercases ASCII letters only.
func asciiLower(s string) string {
	b := []byte(s)
	for i, c := range b {
		if 'A' <= c && c <= 'Z' {
			b[i] = c + 'a' - 'A'
		}
	}
	return string(b)
}
