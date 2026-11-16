package app

// This file does not import gk: its functions are tested with plain `go test`.

import "strings"

// progressMessage is whether message reports a step still under way rather
// than an outcome. (lib.rs progress_message)
func progressMessage(message string) bool {
	return strings.HasSuffix(message, "…") || strings.Contains(message, "waiting for") ||
		strings.HasPrefix(message, "Requesting")
}
