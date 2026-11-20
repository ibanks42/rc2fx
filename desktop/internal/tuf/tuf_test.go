package tuf

import (
	"crypto/sha256"
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
