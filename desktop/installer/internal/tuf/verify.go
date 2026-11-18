package tuf

// Signatures are checked over the canonical form of the `signed` object as
// it was published, as sigstore-tuf does. go-tuf's own VerifyDelegate signs
// its re-serialization of the typed payload instead, which differs from the
// published bytes whenever a timestamp's fraction ends in a zero or an
// optional field such as `consistent_snapshot` is absent, so it would refuse
// metadata the mixer accepts.

import (
	"crypto"
	"crypto/ecdsa"
	"crypto/ed25519"
	"crypto/elliptic"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/sha512"
	"crypto/x509"
	"encoding/hex"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"sort"
	"strconv"
	"strings"

	"github.com/theupdateframework/go-tuf/v2/metadata"

	"github.com/ibanks42/rc2fx/desktop/installer/internal/sjson"
)

type signature struct {
	KeyID string `json:"keyid"`
	Sig   string `json:"sig"`
}

// envelope is a metadata file's canonical signed bytes and signatures.
type envelope struct {
	canonical  []byte
	signatures []signature
}

// envelopeDocument is a metadata file's top level. It must hold exactly
// `signed` and `signatures`, each once and spelled exactly so: go-tuf reads
// the same bytes with encoding/json, which matches member names
// case-insensitively and lets a later member win, so a second `SIGNED` or
// `signed` member would be what go-tuf acts on while the signatures were
// checked over the first.
type envelopeDocument struct {
	_          struct{}    `serde:"deny_unknown_fields"`
	Signed     any         `json:"signed"`
	Signatures []signature `json:"signatures"`
}

func parseEnvelope(data []byte) (*envelope, error) {
	var doc envelopeDocument
	if err := sjson.Unmarshal(data, &doc); err != nil {
		return nil, fmt.Errorf("malformed metadata: %w", err)
	}
	canonical, err := Canonical(doc.Signed)
	if err != nil {
		return nil, err
	}
	return &envelope{canonical: canonical, signatures: doc.Signatures}, nil
}

// Canonical is securesystemslib-style canonical JSON: sorted keys, no
// whitespace, integers only, and only `"` and `\` escaped in strings.
func Canonical(value any) ([]byte, error) {
	var out []byte
	err := canonical(value, &out)
	return out, err
}

func canonical(value any, out *[]byte) error {
	switch v := value.(type) {
	case nil:
		*out = append(*out, "null"...)
	case bool:
		*out = strconv.AppendBool(*out, v)
	case json.Number:
		text := string(v)
		if strings.ContainsAny(text, ".eE") {
			return errors.New("canonical JSON allows integers only")
		}
		if i, err := strconv.ParseInt(text, 10, 64); err == nil {
			*out = strconv.AppendInt(*out, i, 10)
		} else if u, err := strconv.ParseUint(text, 10, 64); err == nil {
			*out = strconv.AppendUint(*out, u, 10)
		} else {
			return errors.New("canonical JSON allows integers only")
		}
	case string:
		canonicalString(v, out)
	case []any:
		*out = append(*out, '[')
		for i, item := range v {
			if i > 0 {
				*out = append(*out, ',')
			}
			if err := canonical(item, out); err != nil {
				return err
			}
		}
		*out = append(*out, ']')
	case map[string]any:
		keys := make([]string, 0, len(v))
		for k := range v {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		*out = append(*out, '{')
		for i, k := range keys {
			if i > 0 {
				*out = append(*out, ',')
			}
			canonicalString(k, out)
			*out = append(*out, ':')
			if err := canonical(v[k], out); err != nil {
				return err
			}
		}
		*out = append(*out, '}')
	default:
		return fmt.Errorf("canonical JSON cannot hold %T", value)
	}
	return nil
}

func canonicalString(s string, out *[]byte) {
	*out = append(*out, '"')
	for i := range len(s) {
		switch c := s[i]; c {
		case '\\':
			*out = append(*out, '\\', '\\')
		case '"':
			*out = append(*out, '\\', '"')
		default:
			*out = append(*out, c)
		}
	}
	*out = append(*out, '"')
}

// verifyThreshold checks that the envelope carries at least the role's
// threshold of valid signatures from distinct authorized keys, the keys
// looked up by declared key ID. A placeholder (empty) signature is ignored;
// one key signing twice is an error.
func (e *envelope) verifyThreshold(keys map[string]*metadata.Key, role *metadata.Role, name string) error {
	if role == nil {
		return fmt.Errorf("unknown role %s", name)
	}
	if role.Threshold < 1 {
		return fmt.Errorf("role %s declares signature threshold %d; at least 1 is required", name, role.Threshold)
	}
	authorized := map[string]bool{}
	for _, id := range role.KeyIDs {
		authorized[id] = true
	}
	good, seen := map[string]bool{}, map[string]bool{}
	for _, sig := range e.signatures {
		if !authorized[sig.KeyID] || sig.Sig == "" {
			continue
		}
		if seen[sig.KeyID] {
			return fmt.Errorf("role %s is signed twice by key %s", name, sig.KeyID)
		}
		seen[sig.KeyID] = true
		key := keys[sig.KeyID]
		if key == nil {
			continue
		}
		raw, err := hex.DecodeString(sig.Sig)
		if err != nil {
			continue
		}
		if verifyKey(key, e.canonical, raw) == nil {
			good[sig.KeyID] = true
		}
	}
	if len(good) < role.Threshold {
		return fmt.Errorf("role %s: %d valid signatures, threshold %d", name, len(good), role.Threshold)
	}
	return nil
}

func pemPublicKey(text string) (any, error) {
	block, _ := pem.Decode([]byte(text))
	if block == nil {
		return nil, errors.New("invalid PEM public key")
	}
	return x509.ParsePKIXPublicKey(block.Bytes)
}

// verifyKey checks one signature with a key, by its scheme.
func verifyKey(key *metadata.Key, data, sig []byte) error {
	public := strings.TrimSpace(key.Value.PublicKey)
	isPEM := strings.HasPrefix(public, "-----BEGIN")
	switch key.Scheme {
	case "ed25519":
		var pub ed25519.PublicKey
		if isPEM {
			parsed, err := pemPublicKey(public)
			if err != nil {
				return err
			}
			p, ok := parsed.(ed25519.PublicKey)
			if !ok {
				return errors.New("not an ed25519 key")
			}
			pub = p
		} else {
			raw, err := hex.DecodeString(public)
			if err != nil {
				return err
			}
			if len(raw) != ed25519.PublicKeySize {
				return fmt.Errorf("ed25519 public key must be 32 bytes, got %d", len(raw))
			}
			pub = ed25519.PublicKey(raw)
		}
		if !ed25519.Verify(pub, data, sig) {
			return errors.New("invalid signature")
		}
		return nil
	case "ecdsa-sha2-nistp256", "ecdsa-sha2-nistp384":
		if !isPEM {
			return errors.New("public key is neither PEM nor a hex ed25519 key")
		}
		parsed, err := pemPublicKey(public)
		if err != nil {
			return err
		}
		pub, ok := parsed.(*ecdsa.PublicKey)
		if !ok {
			return errors.New("not an ECDSA key")
		}
		var digest []byte
		if key.Scheme == "ecdsa-sha2-nistp256" {
			if pub.Curve != elliptic.P256() {
				return errors.New("key curve does not match its scheme")
			}
			d := sha256.Sum256(data)
			digest = d[:]
		} else {
			if pub.Curve != elliptic.P384() {
				return errors.New("key curve does not match its scheme")
			}
			d := sha512.Sum384(data)
			digest = d[:]
		}
		if !ecdsa.VerifyASN1(pub, digest, sig) {
			return errors.New("invalid signature")
		}
		return nil
	case "rsassa-pss-sha256", "rsassa-pss-sha384", "rsassa-pss-sha512":
		if !isPEM {
			return errors.New("public key is neither PEM nor a hex ed25519 key")
		}
		parsed, err := pemPublicKey(public)
		if err != nil {
			return err
		}
		pub, ok := parsed.(*rsa.PublicKey)
		if !ok {
			return errors.New("not an RSA key")
		}
		if bits := pub.N.BitLen(); bits < 2048 || bits > 8192 {
			return fmt.Errorf("RSA key size %d is outside 2048-8192 bits", bits)
		}
		var h crypto.Hash
		switch key.Scheme {
		case "rsassa-pss-sha256":
			h = crypto.SHA256
		case "rsassa-pss-sha384":
			h = crypto.SHA384
		default:
			h = crypto.SHA512
		}
		hasher := h.New()
		hasher.Write(data)
		return rsa.VerifyPSS(pub, h, hasher.Sum(nil), sig, &rsa.PSSOptions{SaltLength: h.Size(), Hash: h})
	}
	return fmt.Errorf("unsupported key scheme %s/%s", key.Type, key.Scheme)
}
