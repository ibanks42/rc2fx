package app

// This file does not import gk: its functions are tested with plain `go test`.

import "strings"

// progressMessage is whether message reports a step still under way rather
// than an outcome.
func progressMessage(message string) bool {
	if strings.HasPrefix(message, saveFailedPrefix) {
		// A failed save is an outcome whatever its reason says.
		return false
	}
	return strings.HasSuffix(message, "…") || strings.Contains(message, "waiting for") ||
		strings.HasPrefix(message, "Requesting")
}

// mixerChangedElsewhere is what the app says when the mixer's settings
// changed while it held unsaved edits. The edits are kept, put on top of the
// new settings and saved like any other edit.
const mixerChangedElsewhere = "Mixer settings changed elsewhere. Your edits were kept and are saved on top of them."

// saveFailedPrefix starts every saveFailedMessage.
const saveFailedPrefix = "Couldn't save"

// saveFailedMessage is what a failed save of the settings says.
func saveFailedMessage(reason string) string {
	if reason == "" {
		return saveFailedPrefix + " the settings to the mixer."
	}
	return saveFailedPrefix + ": " + reason
}
