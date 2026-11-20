// Package developer tracks SSH access state for the connected mixer over USB 1,
// formats status copy, and builds the SSH login command.
//
// Toggling Developer mode only shows or hides these controls; only an explicit
// Disable removes RC2FX's key from the mixer. Kept free of gk imports so it can
// be tested with plain `go test`.
package developer

import (
	"fmt"
	"os"
	"runtime"
	"slices"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/app/model"
	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// AddressPlaceholder is used in the login command until the user enters an address.
const AddressPlaceholder = "MIXER_IP"

// Reading holds the result of an SSH status query. A nil Status with a nil Err
// means the mixer firmware is too old to manage SSH.
type Reading struct {
	Status *core.SSHStatus
	Err    error
}

// Pinned records the mixer serial whose host keys were pinned, along with the
// pinned host info or pinning error.
type Pinned struct {
	Serial string
	Host   *installer.PinnedHost
	Err    error
}

type sentRequest struct {
	id     uint64
	change bool
}

// Change represents an in-flight SSH mutation.
type Change int

const (
	ChangeNone Change = iota
	// Preparing this machine's SSH key before sending an Enable request.
	ChangePreparingKey
	ChangeEnabling
	ChangeDisabling
)

// Busy reports whether an SSH change is in progress and new actions should be blocked.
func (c Change) Busy() bool { return c != ChangeNone }

// Sent reports whether a change has already been sent to the mixer, meaning an
// earlier read reply could reflect stale state.
func (c Change) Sent() bool { return c == ChangeEnabling || c == ChangeDisabling }

// SSH holds the last known SSH state for the current mixer connection.
type SSH struct {
	// Nil until the first status read completes.
	Status *Reading
	Change Change
	// Reset whenever Status is replaced.
	Pinned *Pinned
	// In-flight requests in send order; replies with unknown IDs are stale and ignored.
	sent []sentRequest
	// Monotonic request ID counter, preserved across Clear so old replies never collide.
	nextID uint64
	// This machine's RC2FX key fingerprint, read without creating a key (empty if absent or unreadable).
	LocalFingerprint string
	// Incremented on every Clear so background work and confirmations from an earlier
	// connection or mode toggle can be discarded, even after reconnecting the same mixer.
	Epoch uint64
}

// Clear resets connection state on disconnect, reconnect, stream loss/recovery,
// or Developer mode toggle while preserving Epoch and nextID.
func (s *SSH) Clear() {
	epoch := s.Epoch + 1 // wraps at 2^64
	nextID := s.nextID
	*s = SSH{}
	s.Epoch = epoch
	s.nextID = nextID
}

// Track registers an outgoing request (with change=true for Enable/Disable) and returns its ID.
func (s *SSH) Track(change bool) uint64 {
	s.nextID++
	s.sent = append(s.sent, sentRequest{s.nextID, change})
	return s.nextID
}

// Untrack removes a request that failed to send.
func (s *SSH) Untrack(id uint64) {
	s.sent = slices.DeleteFunc(s.sent, func(r sentRequest) bool { return r.id == id })
}

// AcceptReply matches a reply against tracked requests and returns whether its
// payload should update the UI. Replies arrive in order, so older entries are
// dropped. Read replies that arrive while a change is still in flight are ignored.
func (s *SSH) AcceptReply(id uint64) bool {
	at := slices.IndexFunc(s.sent, func(r sentRequest) bool { return r.id == id })
	if at < 0 {
		return false
	}
	change := s.sent[at].change
	s.sent = slices.Delete(s.sent, 0, at+1)
	if change {
		s.Change = ChangeNone
	}
	return change || !slices.ContainsFunc(s.sent, func(r sentRequest) bool { return r.change })
}

// Flag is the short status badge shown on the SSH row.
type Flag struct {
	Text string
	Tone model.Tone
}

// Summary holds the copy, optional badge, and optional caution text for the SSH row.
type Summary struct {
	Line    string
	Flag    *Flag
	Caution string
}

func plain(line string, flag *Flag) Summary { return Summary{Line: line, Flag: flag} }

// Describe builds the user-facing summary for the current SSH state.
// ChangePreparingKey is presented as enabling; pass an empty localFingerprint if none exists.
func Describe(status *Reading, change Change, localFingerprint string) Summary {
	if change.Busy() {
		what := "Enabling"
		if change == ChangeDisabling {
			what = "Disabling"
		}
		return plain(what+" SSH access…", &Flag{"Working", model.ToneUnknown})
	}
	switch {
	case status == nil:
		return plain("Checking…", nil)
	case status.Err != nil:
		return plain("Could not read SSH access: "+status.Err.Error(), &Flag{"Unknown", model.ToneBad})
	case status.Status == nil:
		return plain("Update the RC2FX mixer software to manage SSH access from here.",
			&Flag{"Update needed", model.ToneWarn})
	}
	st := status.Status
	stopped := st.ServerRunning != nil && !*st.ServerRunning
	var line string
	var flag Flag
	switch {
	case !st.Managed && stopped:
		line = "Log in to the mixer as root over the network. The SSH server is not running."
		flag = Flag{"Off", model.ToneUnknown}
	case !st.Managed:
		line = "Log in to the mixer as root over the network."
		flag = Flag{"Off", model.ToneUnknown}
	case stopped:
		line = "The key is in place, but the SSH server is not running."
		flag = Flag{"Not running", model.ToneWarn}
	case st.ManagedFingerprint != nil && localFingerprint != "" && *st.ManagedFingerprint == localFingerprint:
		line = "Logins use this computer's key."
		flag = Flag{"On", model.ToneUpdate}
	case st.ManagedFingerprint != nil && localFingerprint != "":
		line = "Logins use a key from another computer. To use this one, disable SSH and enable it again here."
		flag = Flag{"On", model.ToneUpdate}
	default:
		line = "Logins use an RC2FX-managed key. To use this computer's key, disable SSH and enable it again here."
		flag = Flag{"On", model.ToneUpdate}
	}
	caution := ""
	if keys := st.UnmanagedKeys; keys > 0 {
		noun, pronoun := "keys", "them"
		if keys == 1 {
			noun, pronoun = "key", "it"
		}
		caution = fmt.Sprintf("%d other %s can also log in. RC2FX did not add %s and leaves %s alone.",
			keys, noun, pronoun, pronoun)
	}
	return Summary{Line: line, Flag: &flag, Caution: caution}
}

// UsesLocalKey reports whether the mixer's managed key matches this machine's key.
func UsesLocalKey(status *core.SSHStatus, localFingerprint string) bool {
	return status.Managed && localFingerprint != "" &&
		status.ManagedFingerprint != nil && *status.ManagedFingerprint == localFingerprint
}

// ValidAddress reports whether text looks like a plain hostname or IP address,
// preventing shell metacharacters from slipping into the pasted login command.
func ValidAddress(text string) bool {
	if len(text) > 253 {
		return false
	}
	for _, c := range text {
		ok := c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z' || c >= '0' && c <= '9' ||
			c == '.' || c == ':' || c == '-'
		if !ok {
			return false
		}
	}
	return true
}

// ReadLocalFingerprint reads this machine's public key fingerprint without
// generating a key if one does not exist.
func ReadLocalFingerprint() string {
	path, err := installer.SSHKeyPath()
	if err != nil {
		return ""
	}
	text, err := os.ReadFile(path + ".pub")
	if err != nil {
		return ""
	}
	fields := strings.Fields(string(text))
	key, err := core.ParseEd25519PublicKey(strings.Join(fields[:min(len(fields), 2)], " "))
	if err != nil {
		return ""
	}
	return key.Fingerprint()
}

// Action returns which button action to offer (enable=true for Enable, false
// for Disable), or ok=false when no action should be available.
func Action(status *Reading, change Change) (enable, ok bool) {
	if status != nil && status.Err == nil && status.Status != nil && !change.Busy() {
		return !status.Status.Managed, true
	}
	return false, false
}

// quote escapes a file path for pasting into the user's shell.
func quote(path string) string {
	if runtime.GOOS == "windows" {
		return `"` + strings.ReplaceAll(path, `"`, `\"`) + `"`
	}
	return "'" + strings.ReplaceAll(path, "'", `'\''`) + "'"
}

// LoginCommand formats the ssh command using the mixer's pinned host key.
// Because the mixer does not report its IP, an empty address falls back to AddressPlaceholder.
func LoginCommand(key, knownHosts, alias, address string) string {
	if address == "" {
		address = AddressPlaceholder
	}
	return fmt.Sprintf("ssh -i %s -o UserKnownHostsFile=%s -o HostKeyAlias=%s root@%s",
		quote(key), quote(knownHosts), alias, address)
}
