package core

import (
	"bytes"
	"crypto/sha256"
	"encoding/base64"
	"encoding/binary"
	"strings"
	"unicode"
)

// Developer SSH access managed over USB 1 without reflashing.
//
// RØDE's firmware runs OpenSSH with key-only root login and an empty
// `authorized_keys`. RC2FX can add one key of its own to that file, marked as
// RC2FX's, and remove it again; every other key is reported and left alone.
// Removing the key stops new logins; it does not end sessions already open.

// SSHManagedMarker is the comment that marks the one `authorized_keys` line
// RC2FX manages.
const SSHManagedMarker = "rc2fx-managed-developer-key"

// MaxSSHPublicKey is the longest public key line accepted, far above an
// Ed25519 key's length.
const MaxSSHPublicKey = 512

// SSHStatus is what the mixer reports about SSH access. `managed`,
// `unmanagedKeys` and `hostKeys` are required, so a malformed reply is an
// error rather than a confident "off"; unknown extra fields are accepted.
type SSHStatus struct {
	// The RC2FX-managed key is installed.
	Managed bool `json:"managed"`
	// `SHA256:…` of the managed key, when installed.
	ManagedFingerprint *string `json:"managedFingerprint,omitempty"`
	// Other `authorized_keys` lines, such as a key built into an earlier
	// flash. RC2FX never changes them.
	UnmanagedKeys uint32 `json:"unmanagedKeys"`
	// The mixer's SSH server public host keys, for pinning.
	HostKeys []string `json:"hostKeys"`
	// Whether the SSH server was seen running; nil if that could not be
	// determined.
	ServerRunning *bool   `json:"serverRunning,omitempty"`
	Detail        *string `json:"detail,omitempty"`
}

func (SSHStatus) AllowUnknownJSONFields() {}

// Equal compares field by field (deepEqual).
func (s SSHStatus) Equal(other SSHStatus) bool { return deepEqual(s, other) }

// The most host keys a reply may list; a server has one per key type.
const maxHostKeys = 16

// The longest host key line, above a 4096-bit RSA key's.
const maxHostKey = 2048

// `SHA256:` and 43 characters of base64, with room for a longer hash.
const maxFingerprint = 128

// Validate refuses text the mixer's SSH server could not have produced:
// these fields are shown as they arrive, and host keys are pinned.
func (s SSHStatus) Validate() error {
	invalid := len(s.HostKeys) > maxHostKeys
	for _, key := range s.HostKeys {
		if !ValidDisplayText(key, maxHostKey) {
			invalid = true
		}
	}
	if s.ManagedFingerprint != nil && !ValidDisplayText(*s.ManagedFingerprint, maxFingerprint) {
		invalid = true
	}
	if s.Detail != nil && !ValidDisplayText(*s.Detail, MaxStatusText) {
		invalid = true
	}
	if invalid {
		return CoreError("Invalid SSH status")
	}
	return nil
}

// SSHRequest is `PUT api/v1/ssh`.
type SSHRequest struct {
	Enabled bool `json:"enabled"`
	// Required when enabling: one plain `ssh-ed25519` public key line.
	PublicKey *string `json:"publicKey,omitempty"`
}

// Ed25519PublicKey is an Ed25519 public key in its canonical
// `ssh-ed25519 BASE64` form, without options or comment.
type Ed25519PublicKey struct {
	line string
}

// ParseEd25519PublicKey accepts exactly one line `ssh-ed25519 BASE64
// [comment]` whose blob is a well-formed Ed25519 key. Options, other key
// types, control characters, extra lines and oversized input are refused.
// The comment is dropped.
func ParseEd25519PublicKey(text string) (Ed25519PublicKey, error) {
	fail := func(why string) (Ed25519PublicKey, error) {
		return Ed25519PublicKey{}, CoreError("invalid SSH public key: " + why)
	}
	if len(text) > MaxSSHPublicKey {
		return fail("too long")
	}
	line := strings.TrimSuffix(text, "\n")
	if strings.ContainsAny(line, "\n\r") {
		return fail("more than one line")
	}
	if strings.IndexFunc(line, func(c rune) bool { return unicode.IsControl(c) && c != '\t' }) >= 0 {
		return fail("control characters")
	}
	parts := strings.FieldsFunc(line, isASCIIWhitespace)
	if len(parts) < 2 {
		return fail("expected `ssh-ed25519 KEY`")
	}
	kind, blob := parts[0], parts[1]
	if kind != "ssh-ed25519" {
		return fail("only ssh-ed25519 keys are accepted")
	}
	decoded, ok := decodeBase64(blob)
	if !ok {
		return fail("the key is not base64")
	}
	// string "ssh-ed25519" then string of 32 key bytes.
	expected := make([]byte, 0, 19)
	expected = binary.BigEndian.AppendUint32(expected, 11)
	expected = append(expected, "ssh-ed25519"...)
	expected = binary.BigEndian.AppendUint32(expected, 32)
	if len(decoded) != 51 || !bytes.Equal(decoded[:19], expected) {
		return fail("the key data is not an Ed25519 key")
	}
	return Ed25519PublicKey{line: "ssh-ed25519 " + blob}, nil
}

// isASCIIWhitespace is ASCII whitespace: space, \t, \n, \x0C, \r (not \v).
func isASCIIWhitespace(c rune) bool {
	return c == ' ' || c == '\t' || c == '\n' || c == '\x0c' || c == '\r'
}

// AsStr is `ssh-ed25519 BASE64`.
func (k Ed25519PublicKey) AsStr() string { return k.line }

func (k Ed25519PublicKey) String() string { return k.line }

// Fingerprint is OpenSSH's fingerprint, `SHA256:` and the unpadded base64 of
// the SHA-256 of the key blob, as `ssh-keygen -l` prints it.
func (k Ed25519PublicKey) Fingerprint() string {
	blob, ok := decodeBase64(strings.TrimPrefix(k.line, "ssh-ed25519 "))
	if !ok {
		panic("checked by parse")
	}
	digest := sha256.Sum256(blob)
	return "SHA256:" + base64.RawStdEncoding.EncodeToString(digest[:])
}

// ManagedLine is the line RC2FX writes: the key and its marker comment.
func (k Ed25519PublicKey) ManagedLine() string { return k.line + " " + SSHManagedMarker }

// decodeBase64 is padded standard base64 that, like the mixer's, does not check
// the unused low bits of the last group.
func decodeBase64(text string) ([]byte, bool) {
	value := func(c byte) (uint32, bool) {
		switch {
		case c >= 'A' && c <= 'Z':
			return uint32(c - 'A'), true
		case c >= 'a' && c <= 'z':
			return uint32(c-'a') + 26, true
		case c >= '0' && c <= '9':
			return uint32(c-'0') + 52, true
		case c == '+':
			return 62, true
		case c == '/':
			return 63, true
		}
		return 0, false
	}
	data := []byte(text)
	if len(data) == 0 || len(data)%4 != 0 {
		return nil, false
	}
	out := make([]byte, 0, len(data)/4*3)
	chunks := len(data) / 4
	for index := 0; index < chunks; index++ {
		chunk := data[index*4 : index*4+4]
		last := index == chunks-1
		pad := 0
		for i := 3; i >= 0 && chunk[i] == '='; i-- {
			pad++
		}
		if pad > 2 || (pad > 0 && !last) {
			return nil, false
		}
		var n uint32
		for _, c := range chunk[:4-pad] {
			v, ok := value(c)
			if !ok {
				return nil, false
			}
			n = n<<6 | v
		}
		n <<= 6 * uint32(pad)
		decoded := [3]byte{byte(n >> 16), byte(n >> 8), byte(n)}
		out = append(out, decoded[:3-pad]...)
	}
	return out, true
}
