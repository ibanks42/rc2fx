package usbcontrol

// Session handoff across an app restart. The mixer refuses a new session for
// a few seconds after the previous client's last report, so an app that
// restarts with a fresh nonce waits that out. Instead, the old process passes
// its nonce and request number to the new one, which continues the same
// session and is answered at once.

import (
	"fmt"
	"strconv"
	"strings"
	"sync"
)

// handoffGap skips request numbers the old process may still have used after
// the token was taken. The mixer only refuses numbers at or below the last
// one it admitted, so a gap costs nothing.
const handoffGap = 1 << 16

type sessionToken struct {
	serial   string
	nonce    uint64
	sequence uint32
}

var handoff struct {
	mu sync.Mutex
	// latest is the newest session this process opened.
	latest *sessionToken
	// resume seeds the next Open, once.
	resume *sessionToken
}

func (c *Client) remember() {
	handoff.mu.Lock()
	defer handoff.mu.Unlock()
	handoff.latest = &sessionToken{serial: c.serial, nonce: c.nonce, sequence: c.sequence}
}

// HandoffToken encodes this process's mixer session for a restarted app to
// continue, or returns ok=false if no session can be handed off.
func HandoffToken() (token string, ok bool) {
	handoff.mu.Lock()
	defer handoff.mu.Unlock()
	t := handoff.latest
	if t == nil || t.serial == "" || strings.Contains(t.serial, ":") || t.sequence > ^uint32(0)-2*handoffGap {
		return "", false
	}
	return fmt.Sprintf("%s:%x:%x", t.serial, t.nonce, t.sequence+handoffGap), true
}

// ResumeFrom seeds the next Open to continue the session in token if it
// matches the connected mixer. Malformed tokens are ignored.
func ResumeFrom(token string) bool {
	parts := strings.Split(token, ":")
	if len(parts) != 3 || parts[0] == "" {
		return false
	}
	nonce, err := strconv.ParseUint(parts[1], 16, 64)
	if err != nil || nonce == 0 {
		return false
	}
	sequence, err := strconv.ParseUint(parts[2], 16, 32)
	if err != nil {
		return false
	}
	handoff.mu.Lock()
	defer handoff.mu.Unlock()
	handoff.resume = &sessionToken{serial: parts[0], nonce: nonce, sequence: uint32(sequence)}
	return true
}

func takeResume() *sessionToken {
	handoff.mu.Lock()
	defer handoff.mu.Unlock()
	t := handoff.resume
	handoff.resume = nil
	return t
}
