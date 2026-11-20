package state

import "strings"

// ProgressMessage reports whether message describes an in-flight operation
// rather than a final outcome (used to suppress transient toasts).
func ProgressMessage(message string) bool {
	if strings.HasPrefix(message, SaveFailedPrefix) {
		return false
	}
	return strings.HasSuffix(message, "…") || strings.Contains(message, "waiting for") ||
		strings.HasPrefix(message, "Requesting")
}

// MixerChangedElsewhere is shown when a revision conflict rebases local edits
// onto updated mixer settings.
const MixerChangedElsewhere = "Mixer settings changed elsewhere. Your edits were kept and are saved on top of them."

const SaveFailedPrefix = "Couldn't save"

func SaveFailedMessage(reason string) string {
	if reason == "" {
		return SaveFailedPrefix + " the settings to the mixer."
	}
	return SaveFailedPrefix + ": " + reason
}
