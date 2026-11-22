package tufsign

import (
	"bytes"
	"crypto"
	"crypto/ecdsa"
	"crypto/ed25519"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/x509"
	"encoding/hex"
	"encoding/pem"
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

const (
	KeyTypeRSA      = "rsa"
	KeyTypeEd25519  = "ed25519"
	KeyTypeECDSA    = "ecdsa"
	KeyTypeECDSAOld = "ecdsa-sha2-nistp256"

	SchemeRSAPSS  = "rsassa-pss-sha256"
	SchemeEd25519 = "ed25519"
	SchemeECDSA   = "ecdsa-sha2-nistp256"
)

// Key represents a TUF public key from metadata.
type Key struct {
	Value    core.Value // original JSON object; hashed canonically for the key ID
	Type     string
	Scheme   string
	material []byte // decoded key bytes (PKCS #1 RSA, 32-byte Ed25519, or EC point)
	public   crypto.PublicKey
	extra    bool // true if the key or keyval carried unknown _extra fields
}

// ID returns the lowercase hex SHA-256 of the key's canonical JSON.
func (k Key) ID() (string, error) {
	data, err := Canonical(k.Value)
	if err != nil {
		return "", err
	}
	sum := sha256.Sum256(data)
	return hex.EncodeToString(sum[:]), nil
}

// equal matches tough's derived Key equality (type, scheme, decoded bytes, and
// no extra fields).
func (k Key) equal(o Key) bool {
	return k.variant() == o.variant() && k.Scheme == o.Scheme && bytes.Equal(k.material, o.material) &&
		!k.extra && !o.extra
}

func (k Key) variant() string { return k.Type }

func keyValue(keyType, public, scheme string) core.Value {
	return core.ObjectValue(
		core.Member{Key: "keytype", Value: core.StringValue(keyType)},
		core.Member{Key: "keyval", Value: core.ObjectValue(core.Member{Key: "public", Value: core.StringValue(public)})},
		core.Member{Key: "scheme", Value: core.StringValue(scheme)},
	)
}

func parseKey(value core.Value) (Key, error) {
	members, ok := value.AsObject()
	if !ok {
		return Key{}, errors.New("invalid type: expected a key object")
	}
	keyType, err := stringField(value, "keytype")
	if err != nil {
		return Key{}, err
	}
	keyval, ok := value.Get("keyval")
	if !ok {
		return Key{}, errors.New("missing field `keyval`")
	}
	scheme, err := stringField(value, "scheme")
	if err != nil {
		return Key{}, err
	}
	public, err := stringField(keyval, "public")
	if err != nil {
		return Key{}, err
	}
	key := Key{Value: value.Clone(), Type: keyType, Scheme: scheme}
	for _, m := range members {
		if m.Key != "keytype" && m.Key != "keyval" && m.Key != "scheme" {
			key.extra = true
		}
	}
	keyvalMembers, _ := keyval.AsObject()
	for _, m := range keyvalMembers {
		if m.Key != "public" {
			key.extra = true
		}
	}
	switch keyType {
	case KeyTypeRSA:
		if scheme != SchemeRSAPSS {
			return Key{}, fmt.Errorf("unknown variant `%s`, expected `%s`", scheme, SchemeRSAPSS)
		}
		pub, err := decodeSPKI(public)
		if err != nil {
			return Key{}, err
		}
		rsaKey, ok := pub.(*rsa.PublicKey)
		if !ok {
			return Key{}, errors.New("Failed to decode SPKI: not an RSA key")
		}
		key.public = rsaKey
		key.material = x509.MarshalPKCS1PublicKey(rsaKey)
	case KeyTypeEd25519:
		if scheme != SchemeEd25519 {
			return Key{}, fmt.Errorf("unknown variant `%s`, expected `%s`", scheme, SchemeEd25519)
		}
		raw, err := hex.DecodeString(public)
		if err != nil {
			return Key{}, err
		}
		key.material = raw
		if len(raw) == ed25519.PublicKeySize {
			key.public = ed25519.PublicKey(raw)
		}
	case KeyTypeECDSA, KeyTypeECDSAOld:
		if scheme != SchemeECDSA {
			return Key{}, fmt.Errorf("unknown variant `%s`, expected `%s`", scheme, SchemeECDSA)
		}
		var ecKey *ecdsa.PublicKey
		if strings.HasPrefix(public, "-----BEGIN ") {
			pub, err := decodeSPKI(public)
			if err != nil {
				return Key{}, err
			}
			k, ok := pub.(*ecdsa.PublicKey)
			if !ok || k.Curve != elliptic.P256() {
				return Key{}, errors.New("Failed to decode SPKI: not a P-256 key")
			}
			ecKey = k
		} else {
			raw, err := hex.DecodeString(public)
			if err != nil {
				return Key{}, err
			}
			k, err := ecdsa.ParseUncompressedPublicKey(elliptic.P256(), raw)
			if err == nil {
				ecKey = k
			}
			key.material = raw
		}
		if ecKey != nil {
			key.public = ecKey
			if point, err := ecKey.Bytes(); err == nil && key.material == nil {
				key.material = point
			}
		}
	default:
		return Key{}, fmt.Errorf("unknown variant `%s`, expected one of `rsa`, `ed25519`, `ecdsa`, `ecdsa-sha2-nistp256`", keyType)
	}
	return key, nil
}

func decodeSPKI(text string) (crypto.PublicKey, error) {
	block, _ := pem.Decode([]byte(text))
	if block == nil {
		return nil, errors.New("Failed to decode PEM: no PEM block")
	}
	pub, err := x509.ParsePKIXPublicKey(block.Bytes)
	if err != nil {
		return nil, fmt.Errorf("Failed to decode SPKI: %w", err)
	}
	return pub, nil
}

// verify checks a TUF signature (RSA-PSS/SHA-256 2048 to 4096 bit, Ed25519, or
// ECDSA P-256 ASN.1).
func (k Key) verify(message, signature []byte) bool {
	digest := sha256.Sum256(message)
	switch pub := k.public.(type) {
	case *rsa.PublicKey:
		if bits := pub.N.BitLen(); bits < 2048 || bits > 4096 {
			return false
		}
		return rsa.VerifyPSS(pub, crypto.SHA256, digest[:], signature, &rsa.PSSOptions{SaltLength: rsa.PSSSaltLengthEqualsHash}) == nil
	case ed25519.PublicKey:
		return ed25519.Verify(pub, message, signature)
	case *ecdsa.PublicKey:
		return ecdsa.VerifyASN1(pub, digest[:], signature)
	}
	return false
}

// Signer signs canonical TUF role payloads.
type Signer interface {
	Key() Key
	Sign(message []byte) ([]byte, error)
}

type rsaSigner struct{ key *rsa.PrivateKey }

func (s rsaSigner) Key() Key {
	der, _ := x509.MarshalPKIXPublicKey(&s.key.PublicKey)
	return mustKey(keyValue(KeyTypeRSA, publicPEM(der), SchemeRSAPSS))
}

func (s rsaSigner) Sign(message []byte) ([]byte, error) {
	digest := sha256.Sum256(message)
	return rsa.SignPSS(rand.Reader, s.key, crypto.SHA256, digest[:], &rsa.PSSOptions{SaltLength: rsa.PSSSaltLengthEqualsHash})
}

type ed25519Signer struct{ key ed25519.PrivateKey }

func (s ed25519Signer) Key() Key {
	public := s.key.Public().(ed25519.PublicKey)
	return mustKey(keyValue(KeyTypeEd25519, hex.EncodeToString(public), SchemeEd25519))
}

func (s ed25519Signer) Sign(message []byte) ([]byte, error) {
	return ed25519.Sign(s.key, message), nil
}

type ecdsaSigner struct{ key *ecdsa.PrivateKey }

func (s ecdsaSigner) Key() Key {
	der, _ := x509.MarshalPKIXPublicKey(&s.key.PublicKey)
	return mustKey(keyValue(KeyTypeECDSA, publicPEM(der), SchemeECDSA))
}

func (s ecdsaSigner) Sign(message []byte) ([]byte, error) {
	digest := sha256.Sum256(message)
	return ecdsa.SignASN1(rand.Reader, s.key, digest[:])
}

func mustKey(value core.Value) Key {
	key, err := parseKey(value)
	if err != nil {
		panic(err)
	}
	return key
}

func publicPEM(der []byte) string {
	return strings.TrimSpace(string(pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: der})))
}

// PrivatePEM wraps a PKCS #8 DER private key in a PEM block.
func PrivatePEM(der []byte) []byte {
	return pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: der})
}

// LoadKey reads and parses a private key file.
func LoadKey(path string) (Signer, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, task.Errorf("Failed to read %s: %s", path, task.IOError(err))
	}
	return ParseKeyPair(data)
}

// ParseKeyPair accepts raw PKCS #8 DER (Ed25519 or ECDSA P-256) or PEM
// (PRIVATE KEY for RSA PKCS #8, RSA PRIVATE KEY for RSA PKCS #1), matching
// tough's parse_keypair.
func ParseKeyPair(data []byte) (Signer, error) {
	if key, err := x509.ParsePKCS8PrivateKey(data); err == nil {
		switch k := key.(type) {
		case ed25519.PrivateKey:
			return ed25519Signer{k}, nil
		case *ecdsa.PrivateKey:
			if k.Curve == elliptic.P256() {
				return ecdsaSigner{k}, nil
			}
		}
	}
	unrecognized := task.Error("Unrecognized private key format")
	block, _ := pem.Decode(data)
	if block == nil {
		return nil, unrecognized
	}
	switch block.Type {
	case "PRIVATE KEY":
		key, err := x509.ParsePKCS8PrivateKey(block.Bytes)
		if err != nil {
			return nil, unrecognized
		}
		k, ok := key.(*rsa.PrivateKey)
		if !ok || rsaRejected(k) != "" {
			return nil, unrecognized
		}
		return rsaSigner{k}, nil
	case "RSA PRIVATE KEY":
		k, err := x509.ParsePKCS1PrivateKey(block.Bytes)
		if err != nil {
			return nil, task.Error("Private key rejected: InvalidEncoding")
		}
		if reason := rsaRejected(k); reason != "" {
			return nil, task.Error("Private key rejected: " + reason)
		}
		return rsaSigner{k}, nil
	}
	return nil, unrecognized
}

// rsaRejected enforces the 2048 to 4096 bit RSA key size bounds the verifiers accept.
func rsaRejected(key *rsa.PrivateKey) string {
	switch bits := key.N.BitLen(); {
	case bits < 2048:
		return "TooSmall"
	case bits > 4096:
		return "TooLarge"
	}
	return ""
}

// GenerateRSA3072 generates a fresh RSA-3072 key encoded as PKCS #8 DER.
func GenerateRSA3072() ([]byte, error) {
	key, err := rsa.GenerateKey(rand.Reader, 3072)
	if err != nil {
		return nil, err
	}
	return x509.MarshalPKCS8PrivateKey(key)
}
