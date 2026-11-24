package tuf

import (
	"crypto/ed25519"
	"crypto/rand"
	"crypto/rsa"
	"crypto/sha256"
	"crypto/x509"
	"encoding/hex"
	"encoding/pem"
	"math/big"
	"strings"
	"testing"

	"github.com/theupdateframework/go-tuf/v2/metadata"

	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

func TestCanonicalJSONIsSecureSystemsLibStyle(t *testing.T) {
	value, err := sjson.Value([]byte(`{"b":1,"a":"line1\nline2 \"q\" \\","c":[true,null,-3]}`))
	if err != nil {
		t.Fatal(err)
	}
	got, err := Canonical(value)
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != "{\"a\":\"line1\nline2 \\\"q\\\" \\\\\",\"b\":1,\"c\":[true,null,-3]}" {
		t.Fatalf("%s", got)
	}
	float, _ := sjson.Value([]byte(`{"a":1.5}`))
	if _, err := Canonical(float); err == nil {
		t.Fatal("a float in canonical JSON")
	}
}

func TestIntegrityNeedsALengthMatchAndASupportedHash(t *testing.T) {
	data := []byte("data")
	digest := sha256.Sum256(data)
	length := int64(len(data))
	if err := CheckIntegrity(data, &length, metadata.Hashes{"sha256": digest[:]}, "x"); err != nil {
		t.Fatal(err)
	}
	wrong := length + 1
	if err := CheckIntegrity(data, &wrong, nil, "x"); err == nil {
		t.Fatal("wrong length")
	}
	if err := CheckIntegrity(data, nil, metadata.Hashes{"md5": []byte{1}}, "x"); err == nil || !strings.Contains(err.Error(), "no supported hash") {
		t.Fatal(err)
	}
	if err := CheckIntegrity([]byte("other"), nil, metadata.Hashes{"sha256": digest[:]}, "x"); err == nil {
		t.Fatal("wrong hash")
	}
	if err := CheckIntegrity(data, nil, nil, "x"); err != nil {
		t.Fatal("nothing pinned")
	}
}

func TestFileStoreRefusesNamesOutsideItsFolder(t *testing.T) {
	store := FileStore{Dir: t.TempDir()}
	if err := store.Store("../escape.json", []byte("x")); err == nil {
		t.Fatal("escaped")
	}
	if _, _, err := store.Load("../escape.json"); err == nil {
		t.Fatal("escaped")
	}
	if err := store.Store("root_history/2.root.json", []byte("root")); err != nil {
		t.Fatal(err)
	}
	data, found, err := store.Load("root_history/2.root.json")
	if err != nil || !found || string(data) != "root" {
		t.Fatal(err)
	}
	if _, found, _ := store.Load("absent.json"); found {
		t.Fatal("absent found")
	}
}

func TestDistinctKeysRejectsRewrappedAndCrossEncodedKeys(t *testing.T) {
	// 1. Re-wrapped RSA PEM (different whitespace/headers) under two key IDs.
	rsaPriv, err := rsa.GenerateKey(rand.Reader, 2048)
	if err != nil {
		t.Fatal(err)
	}
	spki, err := x509.MarshalPKIXPublicKey(&rsaPriv.PublicKey)
	if err != nil {
		t.Fatal(err)
	}
	pemA := string(pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: spki}))
	pemB := strings.ReplaceAll(pemA, "\n", "\r\n")
	rootRSA := &metadata.RootType{
		Keys: map[string]*metadata.Key{
			"k1": {Type: "rsa", Scheme: "rsassa-pss-sha256", Value: metadata.KeyVal{PublicKey: pemA}},
			"k2": {Type: "rsa", Scheme: "rsassa-pss-sha256", Value: metadata.KeyVal{PublicKey: pemB}},
		},
		Roles: map[string]*metadata.Role{
			"root": {KeyIDs: []string{"k1", "k2"}, Threshold: 2},
		},
	}
	if err := DistinctKeys(rootRSA); err == nil {
		t.Fatal("DistinctKeys accepted re-wrapped RSA PEM under two IDs")
	}

	// 2. Hex vs SPKI PEM Ed25519 under two key IDs, even with mismatched keytype.
	edPub, edPriv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	edSPKI, err := x509.MarshalPKIXPublicKey(edPub)
	if err != nil {
		t.Fatal(err)
	}
	edPEM := string(pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: edSPKI}))
	edHex := hex.EncodeToString(edPub)
	rootEd := &metadata.RootType{
		Keys: map[string]*metadata.Key{
			"k1": {Type: "ed25519", Scheme: "ed25519", Value: metadata.KeyVal{PublicKey: edHex}},
			"k2": {Type: "alias", Scheme: "ed25519", Value: metadata.KeyVal{PublicKey: edPEM}},
		},
		Roles: map[string]*metadata.Role{
			"targets": {KeyIDs: []string{"k1", "k2"}, Threshold: 2},
		},
	}
	if err := DistinctKeys(rootEd); err == nil {
		t.Fatal("DistinctKeys accepted hex and PEM forms of the same Ed25519 key")
	}

	// 3. verifyKey rejects mismatched keytype vs scheme even for a valid signature.
	msg := []byte("hello")
	sig := ed25519.Sign(edPriv, msg)
	badKey := &metadata.Key{Type: "rsa", Scheme: "ed25519", Value: metadata.KeyVal{PublicKey: edHex}}
	if err := verifyKey(badKey, msg, sig); err == nil {
		t.Fatal("verifyKey accepted keytype/scheme mismatch")
	}
}

func TestVerifyKeyCapsRSAKeySizeAt4096Bits(t *testing.T) {
	rsaKeyForBits := func(bits uint) *metadata.Key {
		t.Helper()
		n := new(big.Int).Lsh(big.NewInt(1), bits-1)
		n.SetBit(n, 0, 1)
		spki, err := x509.MarshalPKIXPublicKey(&rsa.PublicKey{N: n, E: 65537})
		if err != nil {
			t.Fatal(err)
		}
		pemText := string(pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: spki}))
		return &metadata.Key{Type: "rsa", Scheme: "rsassa-pss-sha256", Value: metadata.KeyVal{PublicKey: pemText}}
	}

	msg := []byte("hello")
	for _, bits := range []uint{1024, 2047, 4097, 8192} {
		err := verifyKey(rsaKeyForBits(bits), msg, make([]byte, (bits+7)/8))
		if err == nil || !strings.Contains(err.Error(), "outside 2048-4096 bits") {
			t.Errorf("%d-bit RSA key: got %v, want size rejection", bits, err)
		}
	}

	// A 4096-bit key passes the size check and fails only on signature verification.
	err := verifyKey(rsaKeyForBits(4096), msg, make([]byte, 512))
	if err == nil || strings.Contains(err.Error(), "outside 2048-4096 bits") {
		t.Errorf("4096-bit RSA key: got %v, want signature error rather than size rejection", err)
	}
}
