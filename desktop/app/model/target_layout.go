package model

import (
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Group categorizes mixer channels in the sidebar.
type Group int

const (
	GroupMicrophones Group = iota
	GroupSources
	GroupHeadphones
	GroupOutputs
)

// GroupAll lists channel groups in display order.
var GroupAll = [4]Group{
	GroupMicrophones,
	GroupSources,
	GroupHeadphones,
	GroupOutputs,
}

// OutputLabel maps output target IDs (on both Pro II and Duo) to current display
// labels, overriding older names persisted in saved configs.
func OutputLabel(id string) (string, bool) {
	for strings.HasPrefix(id, "duo-") {
		id = id[len("duo-"):]
	}
	switch id {
	case "monitors":
		return "Monitors", true
	case "send-main":
		return "Main Mix Out", true
	case "send-chat":
		return "Chat Mix Out", true
	case "send-secondary":
		return "Secondary Mix Out", true
	}
	return "", false
}

// DisplayName formats a channel name for display by capitalizing each word and
// uppercasing "USB" while preserving internal capitals ("CallMe", "RØDE").
// Saved configs retain the mixer's original names.
func DisplayName(name string) string {
	words := strings.Split(name, " ")
	for i, word := range words {
		if strings.EqualFold(word, "usb") && isASCII(word) {
			words[i] = "USB"
			continue
		}
		first, size := utf8.DecodeRuneInString(word)
		if size == 0 {
			continue
		}
		// Single-rune ToUpper intentionally leaves multi-rune expansions (like "ß" -> "SS") alone.
		words[i] = string(unicode.ToUpper(first)) + word[size:]
	}
	return strings.Join(words, " ")
}

func isASCII(s string) bool {
	for i := 0; i < len(s); i++ {
		if s[i] >= utf8.RuneSelf {
			return false
		}
	}
	return true
}

// GroupOf returns the sidebar group for target.
func GroupOf(target *core.Target) Group {
	switch target.Kind {
	case core.TargetKindMic:
		return GroupMicrophones
	case core.TargetKindPlayback:
		return GroupSources
	default:
		switch target.ID {
		case "headphones-1", "headphones-2", "headphones-3", "headphones-4":
			return GroupHeadphones
		}
		return GroupOutputs
	}
}

// Label returns the uppercase section heading for g.
func (g Group) Label() string {
	switch g {
	case GroupMicrophones:
		return "MICROPHONES"
	case GroupSources:
		return "SOURCES"
	case GroupHeadphones:
		return "HEADPHONES"
	default:
		return "OUTPUTS"
	}
}
