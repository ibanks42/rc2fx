package tufsign

import (
	"crypto/ed25519"
	"crypto/rand"
	"crypto/x509"
	"encoding/pem"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

func TestCanonicalJSONIsOLPCs(t *testing.T) {
	value, err := core.ParseJSON(`{"b":1,"a":[true,null,"q\"b\\n\né"],"ab":{},"":-2}`)
	if err != nil {
		t.Fatal(err)
	}
	got, err := Canonical(value)
	if err != nil {
		t.Fatal(err)
	}
	want := "{\"\":-2,\"a\":[true,null,\"q\\\"b\\\\n\né\"],\"ab\":{},\"b\":1}"
	if string(got) != want {
		t.Fatalf("%q", got)
	}
	float, _ := core.ParseJSON(`{"x":8.0}`)
	if _, err := Canonical(float); err == nil {
		t.Fatal("a float was encoded")
	}
}

// Round-trip cases from tough #950 covering every fractional precision.
func TestExpiresKeepsToughsPrecision(t *testing.T) {
	for _, text := range []string{
		"2026-07-13T18:08:28Z",
		"2026-07-13T18:08:28.756Z",
		"2026-07-13T18:08:28.750Z",
		"2026-07-13T18:08:28.756787Z",
		"2026-07-13T18:08:28.756780Z",
		"2026-07-13T18:08:28.756787215Z",
		"2026-07-13T18:08:28.756787210Z",
	} {
		at, err := ParseTimestamp(text)
		if err != nil {
			t.Fatal(err)
		}
		if got := FormatExpires(at); got != text {
			t.Errorf("%s came back as %s", text, got)
		}
	}
	at, _ := ParseTimestamp("2026-07-13T18:08:28.500Z")
	if got := DisplayTimestamp(at); got != "2026-07-13T18:08:28.5Z" {
		t.Errorf("jiff displays %s", got)
	}
}

// Verify the committed production root (RSA-3072 / RSA-PSS) end-to-end.
func TestTheProductionRootVerifies(t *testing.T) {
	data, err := os.ReadFile(filepath.Join(task.Root(), "config", "mixer-update-root.json"))
	if err != nil {
		t.Fatal(err)
	}
	if strings.TrimSpace(string(data)) == "null" {
		t.Skip("this checkout has no trust root")
	}
	root, err := ParseRoot(data)
	if err != nil {
		t.Fatal(err)
	}
	if err := root.VerifyRole(root.Document); err != nil {
		t.Fatal(err)
	}
	for id, key := range root.Keys {
		if key.Type != KeyTypeRSA || key.Scheme != SchemeRSAPSS {
			t.Errorf("%s: %s %s", id, key.Type, key.Scheme)
		}
	}
	// Flipping any signed byte must invalidate the signature.
	tampered := strings.Replace(string(data), `"consistent_snapshot": false`, `"consistent_snapshot": true`, 1)
	if tampered == string(data) {
		t.Fatal("no field to tamper with")
	}
	altered, err := ParseRoot([]byte(tampered))
	if err != nil {
		t.Fatal(err)
	}
	if err := root.VerifyRole(altered.Document); err == nil {
		t.Fatal("a tampered root verified")
	}
}

func TestKeysLoadAsToughLoadsThem(t *testing.T) {
	_, private, _ := ed25519.GenerateKey(rand.Reader)
	der, _ := x509.MarshalPKCS8PrivateKey(private)
	signer, err := ParseKeyPair(der)
	if err != nil {
		t.Fatal(err)
	}
	if key := signer.Key(); key.Type != KeyTypeEd25519 || len(key.material) != 32 {
		t.Fatalf("%+v", key)
	}
	// tough reads PEM `PRIVATE KEY` as RSA only.
	if _, err := ParseKeyPair(pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: der})); err == nil ||
		err.Error() != "Unrecognized private key format" {
		t.Fatalf("%v", err)
	}
	rsaDER, err := GenerateRSA3072()
	if err != nil {
		t.Fatal(err)
	}
	rsaSigner, err := ParseKeyPair(PrivatePEM(rsaDER))
	if err != nil {
		t.Fatal(err)
	}
	key := rsaSigner.Key()
	public, _ := key.Value.Get("keyval")
	text, _ := public.Get("public")
	if s, _ := text.AsString(); !strings.HasPrefix(s, "-----BEGIN PUBLIC KEY-----\n") || strings.HasSuffix(s, "\n") {
		t.Fatalf("%q", s)
	}
	if _, err := ParseKeyPair([]byte("not a key")); err == nil {
		t.Fatal("garbage loaded")
	}
	if _, err := LoadKey(filepath.Join(t.TempDir(), "missing.pem")); err == nil || !strings.HasPrefix(err.Error(), "Failed to read ") {
		t.Fatalf("%v", err)
	}
}

func TestSignedRolesVerifyAndTamperingFails(t *testing.T) {
	var signers []Signer
	var keys []Key
	roles := []string{RoleRoot, RoleTargets, RoleSnapshot, RoleTimestamp}
	for range roles {
		der, err := GenerateRSA3072()
		if err != nil {
			t.Fatal(err)
		}
		signer, err := ParseKeyPair(PrivatePEM(der))
		if err != nil {
			t.Fatal(err)
		}
		signers = append(signers, signer)
		keys = append(keys, signer.Key())
	}
	expires := time.Date(2099, 1, 1, 0, 0, 0, 583_715_000, time.UTC)
	signed, err := NewRoot(expires, roles, keys)
	if err != nil {
		t.Fatal(err)
	}
	unsigned, err := RootFromSigned(signed)
	if err != nil {
		t.Fatal(err)
	}
	file, err := unsigned.Sign(RoleRoot, signed, signers)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(file), `"expires": "2099-01-01T00:00:00.583715Z"`) || !strings.HasSuffix(string(file), "}\n") {
		t.Fatal(string(file))
	}
	root, err := ParseRoot(file)
	if err != nil {
		t.Fatal(err)
	}
	if err := root.VerifyRole(root.Document); err != nil {
		t.Fatal(err)
	}
	if len(root.Signatures) != 1 {
		t.Fatalf("%d signatures: only the root key signs the root", len(root.Signatures))
	}
	timestamp := RoleValue(RoleTimestamp, 3, expires)
	timestamp.Set("meta", core.ObjectValue(core.Member{Key: "snapshot.json", Value: MetafileValue(10, make([]byte, 32), 3)}))
	if _, err := root.Sign(RoleTimestamp, timestamp, signers[:1]); err == nil {
		t.Fatal("signed timestamp without its key")
	}
	data, err := root.Sign(RoleTimestamp, timestamp, signers)
	if err != nil {
		t.Fatal(err)
	}
	doc, err := ParseRole(data, RoleTimestamp)
	if err != nil {
		t.Fatal(err)
	}
	if err := root.VerifyRole(doc); err != nil {
		t.Fatal(err)
	}
	altered, err := ParseRole([]byte(strings.Replace(string(data), `"version": 3,`, `"version": 4,`, 1)), RoleTimestamp)
	if err != nil {
		t.Fatal(err)
	}
	if err := root.VerifyRole(altered); err == nil || !strings.Contains(err.Error(), "Signature threshold of 1 not met for role timestamp") {
		t.Fatalf("%v", err)
	}
}
