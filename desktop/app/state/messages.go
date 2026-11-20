package state

// This file does not import gk: its functions are tested with plain `go test`.

import "strings"

// progressMessage is whether message reports a step still under way rather
// than an outcome.
func ProgressMessage(message string) bool {
	if strings.HasPrefix(message, SaveFailedPrefix) {
		// A failed save is an outcome whatever its reason says.
		return false
	}
	return strings.HasSuffix(message, "…") || strings.Contains(message, "waiting for") ||
		strings.HasPrefix(message, "Requesting")
}

// MixerChangedElsewhere is what the app says when the mixer's settings
// changed while it held unsaved edits. The edits are kept, put on top of the
// new settings and saved like any other edit.
const MixerChangedElsewhere = "Mixer settings changed elsewhere. Your edits were kept and are saved on top of them."

// SaveFailedPrefix starts every saveFailedMessage.
const SaveFailedPrefix = "Couldn't save"

// saveFailedMessage is what a failed save of the settings says.
func SaveFailedMessage(reason string) string {
	if reason == "" {
		return SaveFailedPrefix + " the settings to the mixer."
	}
	return SaveFailedPrefix + ": " + reason
}
