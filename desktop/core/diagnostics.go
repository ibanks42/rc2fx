package core

import (
	"unicode"
	"unicode/utf8"
)

// MaxEventMessage is the byte cap for sanitized event messages.
const MaxEventMessage = 512

// ForDisplay strips control and bidi characters, truncates Message to
// MaxEventMessage bytes, and returns false if Path is not a valid target ID.
func (e DiagnosticsEvent) ForDisplay() (DiagnosticsEvent, bool) {
	if e.Path != nil && !ValidID(*e.Path) {
		return DiagnosticsEvent{}, false
	}
	message := make([]byte, 0, min(len(e.Message), MaxEventMessage))
	for _, c := range e.Message {
		if unicode.IsControl(c) || IsBidiControl(c) {
			continue
		}
		if len(message)+utf8.RuneLen(c) > MaxEventMessage {
			break
		}
		message = utf8.AppendRune(message, c)
	}
	e.Message = string(message)
	return e, true
}
