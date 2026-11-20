package installer

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/pem"
	"errors"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/archive"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"unicode/utf8"

	"golang.org/x/crypto/ssh"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// authorizedLine is the canonical `authorized_keys` line for a key:
// algorithm and base64, no comment. This is the exact text written into the
// patched firmware.
func authorizedLine(key ssh.PublicKey) []byte {
	return ssh.MarshalAuthorizedKey(key)
}

// singleKey parses text holding exactly one plain public key line (no
// options).
func singleKey(text []byte) ssh.PublicKey {
	if len(text) > 20000 || !utf8.Valid(text) {
		return nil
	}
	var lines []string
	for _, line := range strings.Split(string(text), "\n") {
		line = strings.TrimSpace(strings.TrimSuffix(line, "\r"))
		if line != "" {
			lines = append(lines, line)
		}
	}
	if len(lines) != 1 {
		return nil
	}
	key, _, options, rest, err := ssh.ParseAuthorizedKey([]byte(lines[0]))
	if err != nil || len(options) != 0 || len(rest) != 0 {
		return nil
	}
	// A plain line starts with its algorithm.
	if !strings.HasPrefix(lines[0], key.Type()+" ") {
		return nil
	}
	return key
}

func expandHome(value string) (string, error) {
	if rest, ok := strings.CutPrefix(value, "~/"); ok {
		home, ok := homeDir()
		if !ok {
			return "", Error("cannot find the home directory")
		}
		return filepath.Join(home, rest), nil
	}
	return value, nil
}

// privatePublicKey is the public half of an OpenSSH private key, which may
// be encrypted: its public key is readable without the passphrase.
func privatePublicKey(text []byte) (ssh.PublicKey, error) {
	key, err := ssh.ParseRawPrivateKey(text)
	var missing *ssh.PassphraseMissingError
	if errors.As(err, &missing) && missing.PublicKey != nil {
		return missing.PublicKey, nil
	}
	if err != nil {
		return nil, err
	}
	signer, err := ssh.NewSignerFromKey(key)
	if err != nil {
		return nil, err
	}
	return signer.PublicKey(), nil
}

// ensureKey returns the private key path and its canonical public line,
// creating an Ed25519 pair when neither half exists. Existing keys are only
// validated.
func ensureKey(value string) (string, []byte, error) {
	if value == "" {
		return "", nil, Error("missing SSH key path")
	}
	expanded, err := expandHome(value)
	if err != nil {
		return "", nil, err
	}
	private, err := filepath.Abs(expanded)
	if err != nil {
		return "", nil, err
	}
	public := private + ".pub"
	if exists(private) != exists(public) {
		return "", nil, Error("only one half of the SSH key pair exists; preserve it and inspect the key path")
	}
	if !exists(private) {
		if err := privateDirs(filepath.Dir(private)); err != nil {
			return "", nil, err
		}
		_, key, err := ed25519.GenerateKey(rand.Reader)
		if err != nil {
			return "", nil, err
		}
		block, err := ssh.MarshalPrivateKey(key, "rc2fx-rodecaster-pro-ii")
		if err != nil {
			return "", nil, err
		}
		publicKey, err := ssh.NewPublicKey(key.Public())
		if err != nil {
			return "", nil, err
		}
		// Exclusive creation prevents concurrent installers replacing a key.
		if err := exclusiveFile(private, pem.EncodeToMemory(block)); err != nil {
			return "", nil, err
		}
		if err := exclusiveFile(public, authorizedLine(publicKey)); err != nil {
			return "", nil, err
		}
	}
	text, err := os.ReadFile(private)
	if err != nil {
		return "", nil, err
	}
	if !utf8.Valid(text) {
		return "", nil, Error("stream did not contain valid UTF-8")
	}
	signer, err := privatePublicKey(text)
	if err != nil {
		return "", nil, errorf("read SSH key (OpenSSH format required): %v", err)
	}
	publicText, err := os.ReadFile(public)
	if err != nil {
		return "", nil, err
	}
	parsed := singleKey(publicText)
	if parsed == nil {
		return "", nil, Error("expected one plain SSH public key")
	}
	if !bytes.Equal(parsed.Marshal(), signer.Marshal()) {
		return "", nil, Error("SSH public and private keys do not match")
	}
	return private, authorizedLine(parsed), nil
}

// SSHKeyPath is where the desktop app's developer SSH key lives.
func SSHKeyPath() (string, error) {
	path := DefaultKey()
	if !filepath.IsAbs(path) {
		return "", Error("cannot find the home directory for the SSH key")
	}
	return path, nil
}

// SSHPublicKey is the desktop app's developer SSH public key as one
// `authorized_keys` line (`ssh-ed25519 BASE64`), creating the Ed25519 key
// pair at SSHKeyPath if neither half exists. The private key never leaves
// this computer.
func SSHPublicKey() (string, error) {
	path, err := SSHKeyPath()
	if err != nil {
		return "", err
	}
	_, line, err := ensureKey(path)
	if err != nil {
		return "", err
	}
	key, err := core.ParseEd25519PublicKey(string(line))
	if err != nil {
		return "", Error(err.Error())
	}
	return key.AsStr(), nil
}

// PinnedHost is where a mixer's SSH host keys are pinned, for
// `ssh -o UserKnownHostsFile=<known_hosts> -o HostKeyAlias=<alias>`.
type PinnedHost struct {
	// Alias is `rc2fx-` and twelve hex digits from the mixer's serial:
	// stable across network addresses.
	Alias      string
	KnownHosts string
}

// MixerHostAlias is the `HostKeyAlias` for the mixer with this USB serial.
func MixerHostAlias(serial string) (string, error) {
	if strings.TrimSpace(serial) == "" {
		return "", Error("connect the mixer so its SSH identity can be matched to it")
	}
	return "rc2fx-" + archive.Sum([]byte(serial))[:12], nil
}

// PinMixerHostKeys pins hostKeys (public host key lines the mixer reported
// over USB 1) for the mixer serial in the installer cache's `known_hosts`.
// Only that mixer's alias lines are replaced; every other line is kept.
func PinMixerHostKeys(serial string, hostKeys []string) (*PinnedHost, error) {
	work, ok := DefaultWork()
	if !ok {
		return nil, Error("cannot find the installer cache folder")
	}
	return pinHostKeysIn(work, serial, hostKeys)
}

// maxHostKeys is at most how many host keys are read from one report.
const maxHostKeys = 16

// knownHosts: one writer of the cache's `known_hosts` at a time in this
// process, so overlapping pins for different mixers cannot drop each
// other's lines.
var knownHosts sync.Mutex

func isASCIISpace(b byte) bool {
	return b == ' ' || b == '\t' || b == '\n' || b == '\r' || b == '\f'
}

func pinHostKeysIn(work, serial string, hostKeys []string) (*PinnedHost, error) {
	alias, err := MixerHostAlias(serial)
	if err != nil {
		return nil, err
	}
	if len(hostKeys) > maxHostKeys {
		return nil, Error("the mixer reported too many SSH host keys")
	}
	// Only Ed25519 host keys are pinned, each parsed in full. OpenSSH prefers
	// the key types known for a host, so the mixer's other types are not
	// needed; a report without a valid Ed25519 key changes nothing.
	var lines []string
	for _, key := range hostKeys {
		if !strings.HasPrefix(key, "ssh-ed25519 ") {
			continue
		}
		// The report carries `type key` only, so anything more is malformed.
		if len(strings.Split(key, " ")) != 2 {
			return nil, Error("the mixer reported a malformed SSH host key")
		}
		parsed, err := core.ParseEd25519PublicKey(key)
		if err != nil {
			return nil, errorf("the mixer reported a malformed SSH host key: %v", err)
		}
		lines = append(lines, alias+" "+parsed.AsStr()+"\n")
	}
	if len(lines) == 0 {
		return nil, Error("the mixer reported no Ed25519 SSH host key")
	}
	knownHosts.Lock()
	defer knownHosts.Unlock()
	if err := privateDirs(work); err != nil {
		return nil, err
	}
	path := filepath.Join(work, "known_hosts")
	old, err := os.ReadFile(path)
	if err != nil && !errors.Is(err, fs.ErrNotExist) {
		return nil, err
	}
	// Every other line is kept byte for byte; only a missing final newline
	// is added, before the new lines.
	text := make([]byte, 0, len(old)+256)
	for rest := old; len(rest) > 0; {
		end := bytes.IndexByte(rest, '\n')
		line := rest
		if end >= 0 {
			line = rest[:end+1]
		}
		rest = rest[len(line):]
		words := bytes.FieldsFunc(line, func(r rune) bool { return r < 0x80 && isASCIISpace(byte(r)) })
		if len(words) == 0 || string(words[0]) != alias {
			text = append(text, line...)
		}
	}
	if len(text) > 0 && text[len(text)-1] != '\n' {
		text = append(text, '\n')
	}
	for _, line := range lines {
		text = append(text, line...)
	}
	if err := archive.AtomicFile(path, text); err != nil {
		return nil, err
	}
	return &PinnedHost{Alias: alias, KnownHosts: path}, nil
}
