package model

import (
	"strings"
	"unicode"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// Group is a section of the mixer's channels.
type Group int

const (
	GroupMicrophones Group = iota
	GroupSources
	GroupHeadphones
	GroupOutputs
)

// GroupAll is every group in display order.
var GroupAll = [4]Group{
	GroupMicrophones,
	GroupSources,
	GroupHeadphones,
	GroupOutputs,
}

// OutputLabel is what the app calls the mixer's other outputs, the same on a
// Pro II and a Duo. Configs saved before these names carry older ones.
func OutputLabel(id string) (string, bool) {
	// Like Rust's trim_start_matches: every leading "duo-" goes.
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

// DisplayName is a channel name as the app shows it: every word capitalised,
// like "Main Mix Out", and USB in capitals ("USB a" becomes "USB A"). Words
// already carrying capitals keep them ("CallMe", "RØDE"). Display only: saved
// settings keep the mixer's own names.
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
		// Rust's char::to_uppercase uses full case mapping ("ß" becomes
		// "SS"); Go maps one rune to one rune, so such letters stay as they
		// are here.
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

// GroupOf is the group a target is shown in.
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

// Label is the group's heading.
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
