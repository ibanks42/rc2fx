package tuf

// Regression tests for metadata that carries a second, case-variant `signed`
// member next to the genuine one. Signatures are checked over the exact
// `signed` member, but go-tuf decodes with encoding/json, which matches
// field names case-insensitively and lets a later member win, so every
// decision would come from the unsigned shadow.

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/json"
	"strings"
	"testing"
	"time"

	sigsig "github.com/sigstore/sigstore/pkg/signature"
	"github.com/theupdateframework/go-tuf/v2/metadata"
)

type testSigner struct {
	key    *metadata.Key
	signer sigsig.Signer
}

func newTestSigner(t *testing.T) *testSigner {
	t.Helper()
	public, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	key, err := metadata.KeyFromPublicKey(public)
	if err != nil {
		t.Fatal(err)
	}
	signer, err := sigsig.LoadED25519Signer(private)
	if err != nil {
		t.Fatal(err)
	}
	return &testSigner{key: key, signer: signer}
}

func (s *testSigner) id(t *testing.T) string {
	t.Helper()
	id, err := s.key.ID()
	if err != nil {
		t.Fatal(err)
	}
	return id
}

var testExpires = time.Date(2099, 1, 1, 0, 0, 0, 0, time.UTC)

// testRoot is a version-1 root with every top-level role on signer's key.
func testRoot(t *testing.T, signer *testSigner) (*metadata.Metadata[metadata.RootType], []byte) {
	t.Helper()
	root := metadata.Root(testExpires)
	root.Signed.SpecVersion = "1.0.1"
	for _, role := range []string{metadata.ROOT, metadata.TARGETS, metadata.SNAPSHOT, metadata.TIMESTAMP} {
		if err := root.Signed.AddKey(signer.key, role); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := root.Sign(signer.signer); err != nil {
		t.Fatal(err)
	}
	data, err := root.ToBytes(false)
	if err != nil {
		t.Fatal(err)
	}
	return root, data
}

// shadow appends a member named key holding signed after the genuine
// members of a metadata file.
func shadow(t *testing.T, genuine []byte, key string, signed any) []byte {
	t.Helper()
	fake, err := json.Marshal(signed)
	if err != nil {
		t.Fatal(err)
	}
	trimmed := bytes.TrimRight(genuine, " \n")
	if !bytes.HasSuffix(trimmed, []byte("}")) {
		t.Fatalf("not an object: %s", genuine)
	}
	out := append([]byte{}, trimmed[:len(trimmed)-1]...)
	out = append(out, `,"`+key+`":`...)
	out = append(out, fake...)
	return append(out, '}')
}

var shadowKeys = []string{"SIGNED", "Signed", "signed"}

func TestShadowSignedMemberIsRefusedForTimestamp(t *testing.T) {
	signer := newTestSigner(t)
	_, rootData := testRoot(t, signer)
	now := time.Date(2030, 1, 1, 0, 0, 0, 0, time.UTC)

	ts := metadata.Timestamp(testExpires)
	if _, err := ts.Sign(signer.signer); err != nil {
		t.Fatal(err)
	}
	genuine, err := ts.ToBytes(false)
	if err != nil {
		t.Fatal(err)
	}
	trusted, err := NewTrusted(rootData)
	if err != nil {
		t.Fatal(err)
	}
	if err := trusted.UpdateTimestamp(genuine, &now); err != nil {
		t.Fatalf("genuine timestamp refused: %v", err)
	}

	fake := ts.Signed
	fake.Version = 99
	fake.Meta = map[string]*metadata.MetaFiles{"snapshot.json": {Version: 99}}
	for _, key := range shadowKeys {
		data := shadow(t, genuine, key, fake)
		// The attack's premise: go-tuf reads the unsigned shadow.
		if parsed, err := metadata.Timestamp().FromBytes(data); err != nil || parsed.Signed.Version != 99 {
			t.Fatalf("%s: go-tuf did not read the shadow (version %v, %v)", key, parsed, err)
		}
		trusted, err := NewTrusted(rootData)
		if err != nil {
			t.Fatal(err)
		}
		if err := trusted.UpdateTimestamp(data, &now); err == nil {
			t.Fatalf("%s: a timestamp with a shadow member was accepted at version %d", key, trusted.Timestamp().Version)
		}
		if trusted.Timestamp() != nil {
			t.Fatalf("%s: a refused timestamp became trusted", key)
		}
	}
}

func TestShadowSignedMemberIsRefusedForTargets(t *testing.T) {
	signer := newTestSigner(t)
	_, rootData := testRoot(t, signer)
	now := time.Date(2030, 1, 1, 0, 0, 0, 0, time.UTC)

	// Pins carry versions only, so the shadow is not caught by a hash pin
	// and the envelope check alone has to refuse it.
	ts := metadata.Timestamp(testExpires)
	if _, err := ts.Sign(signer.signer); err != nil {
		t.Fatal(err)
	}
	tsData, err := ts.ToBytes(false)
	if err != nil {
		t.Fatal(err)
	}
	snap := metadata.Snapshot(testExpires)
	if _, err := snap.Sign(signer.signer); err != nil {
		t.Fatal(err)
	}
	snapData, err := snap.ToBytes(false)
	if err != nil {
		t.Fatal(err)
	}
	targets := metadata.Targets(testExpires)
	if _, err := targets.Sign(signer.signer); err != nil {
		t.Fatal(err)
	}
	genuine, err := targets.ToBytes(false)
	if err != nil {
		t.Fatal(err)
	}
	load := func() *Trusted {
		t.Helper()
		trusted, err := NewTrusted(rootData)
		if err != nil {
			t.Fatal(err)
		}
		if err := trusted.UpdateTimestamp(tsData, &now); err != nil {
			t.Fatal(err)
		}
		if err := trusted.UpdateSnapshot(snapData, &now); err != nil {
			t.Fatal(err)
		}
		return trusted
	}
	if err := load().UpdateTargets(genuine, now); err != nil {
		t.Fatalf("genuine targets refused: %v", err)
	}

	fake := targets.Signed
	fake.Targets = map[string]*metadata.TargetFiles{
		"evil.bin": {Length: 4, Hashes: metadata.Hashes{"sha256": make([]byte, 32)}, Path: "evil.bin"},
	}
	for _, key := range shadowKeys {
		data := shadow(t, genuine, key, fake)
		if parsed, err := metadata.Targets().FromBytes(data); err != nil || parsed.Signed.Targets["evil.bin"] == nil {
			t.Fatalf("%s: go-tuf did not read the shadow (%v)", key, err)
		}
		trusted := load()
		if err := trusted.UpdateTargets(data, now); err == nil {
			t.Fatalf("%s: targets with a shadow member were accepted", key)
		}
		if trusted.TargetsRole(metadata.TARGETS) != nil {
			t.Fatalf("%s: refused targets became trusted", key)
		}
	}
}

// poisonedRoot is a genuine version-2 root, signed by the trusted root key
// and also by an attacker key, with a shadow member that hands every role to
// the attacker key.
func poisonedRoot(t *testing.T, root *metadata.Metadata[metadata.RootType], owner, attacker *testSigner, key string) []byte {
	t.Helper()
	data, err := root.ToBytes(false)
	if err != nil {
		t.Fatal(err)
	}
	next, err := metadata.Root().FromBytes(data)
	if err != nil {
		t.Fatal(err)
	}
	next.Signed.Version = 2
	next.ClearSignatures()
	if _, err := next.Sign(owner.signer); err != nil {
		t.Fatal(err)
	}
	if _, err := next.Sign(attacker.signer); err != nil {
		t.Fatal(err)
	}
	genuine, err := next.ToBytes(false)
	if err != nil {
		t.Fatal(err)
	}
	fakeMeta, err := metadata.Root().FromBytes(genuine)
	if err != nil {
		t.Fatal(err)
	}
	fake := fakeMeta.Signed
	fake.Keys = map[string]*metadata.Key{attacker.id(t): attacker.key}
	fake.Roles = map[string]*metadata.Role{}
	for _, role := range []string{metadata.ROOT, metadata.TARGETS, metadata.SNAPSHOT, metadata.TIMESTAMP} {
		fake.Roles[role] = &metadata.Role{KeyIDs: []string{attacker.id(t)}, Threshold: 1}
	}
	out := shadow(t, genuine, key, fake)
	parsed, err := metadata.Root().FromBytes(out)
	if err != nil || parsed.Signed.Keys[attacker.id(t)] == nil {
		t.Fatalf("%s: go-tuf did not read the shadow root (%v)", key, err)
	}
	return out
}

func TestShadowSignedMemberIsRefusedForRoot(t *testing.T) {
	owner, attacker := newTestSigner(t), newTestSigner(t)
	root, rootData := testRoot(t, owner)
	for _, key := range shadowKeys {
		data := poisonedRoot(t, root, owner, attacker, key)
		trusted, err := NewTrusted(rootData)
		if err != nil {
			t.Fatal(err)
		}
		if err := trusted.UpdateRoot(data); err == nil {
			t.Fatalf("%s: a root with a shadow member was accepted", key)
		}
		if trusted.Root().Version != 1 || trusted.Root().Keys[attacker.id(t)] != nil {
			t.Fatalf("%s: the trusted root changed after a refused update", key)
		}
	}
}

func TestCachedPoisonedRootIsIgnored(t *testing.T) {
	owner, attacker := newTestSigner(t), newTestSigner(t)
	root, rootData := testRoot(t, owner)
	store := FileStore{Dir: t.TempDir()}
	if err := store.Store("root_history/2.root.json", poisonedRoot(t, root, owner, attacker, "SIGNED")); err != nil {
		t.Fatal(err)
	}
	updater, err := NewUpdater(nil, rootData)
	if err != nil {
		t.Fatal(err)
	}
	updater.WithStore(store)
	got := updater.Trusted().Root()
	if got.Version != 1 || got.Keys[attacker.id(t)] != nil {
		t.Fatalf("the cached poisoned root was trusted: version %d", got.Version)
	}
	cached, found, err := store.Load("root.json")
	if err != nil || !found || !bytes.Equal(cached, rootData) {
		t.Fatalf("root.json is not the pinned root: found %v, %v", found, err)
	}
}

func TestEnvelopeAcceptsOnlySignedAndSignatures(t *testing.T) {
	for text, want := range map[string]string{
		`{"signed":{},"signatures":[]}`:                        "",
		`{"signatures":[],"signed":{}}`:                        "",
		`{"signed":{},"signatures":[],"SIGNED":{}}`:            "unknown field `SIGNED`",
		`{"signed":{},"signatures":[],"extra":1}`:              "unknown field `extra`",
		`{"signed":{},"signatures":[],"signed":{}}`:            "duplicate field `signed`",
		`{"signed":{},"signatures":[],"signatures":[]}`:        "duplicate field `signatures`",
		`{"Signatures":[],"signed":{},"signatures":[]}`:        "unknown field `Signatures`",
		`{"signatures":[]}`:                                    "missing field `signed`",
		`{"signed":{}}`:                                        "missing field `signatures`",
		`[]`:                                                   "invalid type",
		`{"signed":{},"signatures":[{"keyid":"a","sig":"b"}]}`: "",
	} {
		_, err := parseEnvelope([]byte(text))
		switch {
		case want == "" && err != nil:
			t.Errorf("%s: %v", text, err)
		case want != "" && (err == nil || !strings.Contains(err.Error(), want)):
			t.Errorf("%s: got %v, want %q", text, err, want)
		}
	}
}
