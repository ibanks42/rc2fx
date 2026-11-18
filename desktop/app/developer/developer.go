// Package developer is Developer mode without drawing: what the app knows
// about SSH access to the connected mixer (managed over USB 1), how it says
// it, and the login command. Developer mode only shows these controls.
// Hiding them changes nothing on the mixer; only Disable removes RC2FX's key.
// Nothing here flashes. It does not import gk, so it tests with plain
// `go test`.
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

// AddressPlaceholder stands in for the address until one is typed.
const AddressPlaceholder = "MIXER_IP"

// Reading is one answer to an SSH read: Err set is a failed read; Status nil
// with Err nil is mixer software too old to manage SSH.
type Reading struct {
	Status *core.SSHStatus
	Err    error
}

// Pinned is the mixer (serial) whose host keys were pinned, and where; Err
// when pinning failed.
type Pinned struct {
	Serial string
	Host   *installer.PinnedHost
	Err    error
}

type sentRequest struct {
	id     uint64
	change bool
}

// Change is the SSH change under way, if any.
type Change int

const (
	// ChangeNone: nothing is changing.
	ChangeNone Change = iota
	// ChangePreparingKey: preparing this computer's key before an Enable is
	// sent.
	ChangePreparingKey
	// ChangeEnabling: an Enable was sent and not yet answered.
	ChangeEnabling
	// ChangeDisabling: a Disable was sent and not yet answered.
	ChangeDisabling
)

// Busy reports a change under way: no other may start, and the row offers
// no action.
func (c Change) Busy() bool { return c != ChangeNone }

// Sent reports a change with the mixer: a read answered meanwhile would
// show the state from before it.
func (c Change) Sent() bool { return c == ChangeEnabling || c == ChangeDisabling }

// SSH is what the mixer last said about SSH, for the current connection
// only. The zero value is nothing read and no change under way.
type SSH struct {
	// Status is nil while not read yet.
	Status *Reading
	// Change is the change under way: preparing the key for an Enable, or
	// an Enable or Disable sent and not yet answered.
	Change Change
	// Pinned is reset with every status.
	Pinned *Pinned
	// sent is requests sent and not yet answered, oldest first. A reply
	// whose id is not here belongs to an earlier connection or state and is
	// ignored.
	sent []sentRequest
	// nextID is the next request id. Never reset, so ids are not reused
	// after a clear.
	nextID uint64
	// LocalFingerprint is this computer's RC2FX key fingerprint, read
	// without creating a key; "" when there is no key or it could not be
	// read.
	LocalFingerprint string
	// Epoch is bumped by every clear. Confirmations and background work
	// carry it and count only if it is unchanged, even across a same-mixer
	// reconnect.
	Epoch uint64
}

// Clear forgets everything: a disconnect, a (re)connection, a lost or
// restored stream, or Developer mode changing.
func (s *SSH) Clear() {
	epoch := s.Epoch + 1 // wraps at 2^64
	nextID := s.nextID
	*s = SSH{}
	s.Epoch = epoch
	s.nextID = nextID
}

// Track records a request about to be sent (change: an Enable or Disable)
// and returns its id.
func (s *SSH) Track(change bool) uint64 {
	s.nextID++
	s.sent = append(s.sent, sentRequest{s.nextID, change})
	return s.nextID
}

// Untrack forgets a request that could not be sent.
func (s *SSH) Untrack(id uint64) {
	s.sent = slices.DeleteFunc(s.sent, func(r sentRequest) bool { return r.id == id })
}

// AcceptReply matches a reply to its request. Unknown ids are stale.
// Replies come in order, so anything sent before this request was answered
// already. A read answered while a change is still on its way must not
// replace what is shown.
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

// Flag is the SSH row's short status: its text and tone.
type Flag struct {
	Text string
	Tone model.Tone
}

// Summary is what the SSH row shows. Flag nil and Caution "" are none.
type Summary struct {
	Line    string
	Flag    *Flag
	Caution string
}

func plain(line string, flag *Flag) Summary { return Summary{Line: line, Flag: flag} }

// Describe is the SSH row's sentence, flag and any caution, in plain words.
// Preparing the key already counts as enabling. localFingerprint "" is none.
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

// UsesLocalKey is whether the mixer's managed key is this computer's, so the
// login command will work from here. localFingerprint "" is none.
func UsesLocalKey(status *core.SSHStatus, localFingerprint string) bool {
	return status.Managed && localFingerprint != "" &&
		status.ManagedFingerprint != nil && *status.ManagedFingerprint == localFingerprint
}

// ValidAddress is whether the text could be a host name or an IP address.
// The field refuses anything else, so a pasted command holds nothing but the
// address.
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

// ReadLocalFingerprint is this computer's key fingerprint from its public
// half; "" when there is none. Never creates a key.
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

// Action is which action to offer: Enable (true) or Disable (false); ok
// false for none, as while a change is under way.
func Action(status *Reading, change Change) (enable, ok bool) {
	if status != nil && status.Err == nil && status.Status != nil && !change.Busy() {
		return !status.Status.Managed, true
	}
	return false, false
}

// quote quotes a path for the user's shell, so spaces and quotes survive
// pasting.
func quote(path string) string {
	if runtime.GOOS == "windows" {
		return `"` + strings.ReplaceAll(path, `"`, `\"`) + `"`
	}
	return "'" + strings.ReplaceAll(path, "'", `'\''`) + "'"
}

// LoginCommand is the login command, with this mixer's pinned host
// identity. The mixer does not report its network address, so an empty
// address leaves a placeholder.
func LoginCommand(key, knownHosts, alias, address string) string {
	if address == "" {
		address = AddressPlaceholder
	}
	return fmt.Sprintf("ssh -i %s -o UserKnownHostsFile=%s -o HostKeyAlias=%s root@%s",
		quote(key), quote(knownHosts), alias, address)
}
