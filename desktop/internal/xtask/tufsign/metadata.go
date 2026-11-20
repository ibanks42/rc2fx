package tufsign

import (
	"encoding/hex"
	"errors"
	"fmt"
	"slices"
	"sort"
	"time"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

const (
	RoleRoot      = "root"
	RoleTargets   = "targets"
	RoleSnapshot  = "snapshot"
	RoleTimestamp = "timestamp"
)

// RoleDebug formats role like Rust's Debug for tough's RoleType.
func RoleDebug(role string) string {
	switch role {
	case RoleRoot:
		return "Root"
	case RoleTargets:
		return "Targets"
	case RoleSnapshot:
		return "Snapshot"
	case RoleTimestamp:
		return "Timestamp"
	}
	return role
}

const SpecVersion = "1.0.0"

const (
	maxDocumentBytes = 64 << 20
	maxDocumentNodes = 4_000_000
)

type Signature struct {
	KeyID string // lowercase hex
	Sig   []byte
}

// Document is a parsed signed TUF metadata file.
type Document struct {
	Role        string
	Signed      core.Value
	Signatures  []Signature
	SpecVersion string
	Version     uint64
	Expires     time.Time
}

type RoleKeys struct {
	KeyIDs    []string // lowercase hex, in document order
	Threshold uint64
}

type Root struct {
	*Document
	ConsistentSnapshot bool
	Keys               map[string]Key // keyed by lowercase hex ID
	KeyOrder           []string
	Roles              map[string]RoleKeys
}

type Metafile struct {
	Length    *uint64
	SHA256    []byte // nil when hashes are absent
	HasHashes bool
	Version   uint64
}

type Targets struct {
	*Document
	Entries        map[string]TargetEntry
	DelegatedRoles int
}

type TargetEntry struct {
	Length uint64
	SHA256 []byte
	Custom core.Value // empty object when absent
}

func parseValue(data []byte) (core.Value, error) {
	return core.ParseJSONBytes(data, maxDocumentBytes, maxDocumentNodes)
}

func stringField(v core.Value, key string) (string, error) {
	field, ok := v.Get(key)
	if !ok {
		return "", fmt.Errorf("missing field `%s`", key)
	}
	text, ok := field.AsString()
	if !ok {
		return "", fmt.Errorf("invalid type for `%s`, expected a string", key)
	}
	return text, nil
}

func u64Field(v core.Value, key string, nonZero bool) (uint64, error) {
	field, ok := v.Get(key)
	if !ok {
		return 0, fmt.Errorf("missing field `%s`", key)
	}
	n, ok := field.AsNumber()
	if !ok || n.IsF64() {
		return 0, fmt.Errorf("invalid type for `%s`, expected an integer", key)
	}
	value, ok := n.AsU64()
	if !ok {
		return 0, fmt.Errorf("invalid value for `%s`, expected u64", key)
	}
	if nonZero && value == 0 {
		return 0, fmt.Errorf("invalid value for `%s`, expected a nonzero u64", key)
	}
	return value, nil
}

func hexField(v core.Value, key string) ([]byte, string, error) {
	text, err := stringField(v, key)
	if err != nil {
		return nil, "", err
	}
	raw, err := hex.DecodeString(text)
	if err != nil {
		return nil, "", fmt.Errorf("invalid hex in `%s`: %w", key, err)
	}
	return raw, hex.EncodeToString(raw), nil
}

func parseDocument(data []byte, role string) (*Document, error) {
	value, err := parseValue(data)
	if err != nil {
		return nil, err
	}
	return documentFrom(value, role)
}

func documentFrom(value core.Value, role string) (*Document, error) {
	if _, ok := value.AsObject(); !ok {
		return nil, errors.New("invalid type: expected a signed metadata object")
	}
	signed, ok := value.Get("signed")
	if !ok {
		return nil, errors.New("missing field `signed`")
	}
	rawSignatures, ok := value.Get("signatures")
	if !ok {
		return nil, errors.New("missing field `signatures`")
	}
	items, ok := rawSignatures.AsArray()
	if !ok {
		return nil, errors.New("invalid type for `signatures`, expected a sequence")
	}
	doc, err := signedFrom(signed, role)
	if err != nil {
		return nil, err
	}
	for _, item := range items {
		_, id, err := hexField(item, "keyid")
		if err != nil {
			return nil, err
		}
		sig, _, err := hexField(item, "sig")
		if err != nil {
			return nil, err
		}
		doc.Signatures = append(doc.Signatures, Signature{KeyID: id, Sig: sig})
	}
	return doc, nil
}

func signedFrom(signed core.Value, role string) (*Document, error) {
	if _, ok := signed.AsObject(); !ok {
		return nil, fmt.Errorf("invalid type: expected the %s role object", role)
	}
	kind, err := stringField(signed, "_type")
	if err != nil {
		return nil, err
	}
	if kind != role {
		return nil, fmt.Errorf("invalid value: `_type` is %q, expected %q", kind, role)
	}
	spec, err := stringField(signed, "spec_version")
	if err != nil {
		return nil, err
	}
	version, err := u64Field(signed, "version", true)
	if err != nil {
		return nil, err
	}
	expiresText, err := stringField(signed, "expires")
	if err != nil {
		return nil, err
	}
	expires, err := ParseTimestamp(expiresText)
	if err != nil {
		return nil, fmt.Errorf("invalid `expires`: %w", err)
	}
	return &Document{Role: role, Signed: signed.Clone(), SpecVersion: spec, Version: version, Expires: expires}, nil
}

// ParseRoot parses a signed root.json and verifies that each key ID matches
// its canonical key hash.
func ParseRoot(data []byte) (*Root, error) {
	value, err := parseValue(data)
	if err != nil {
		return nil, err
	}
	doc, err := documentFrom(value, RoleRoot)
	if err != nil {
		return nil, err
	}
	return rootFrom(doc)
}

// RootFromSigned wraps an unsigned root object so it can be signed.
func RootFromSigned(signed core.Value) (*Root, error) {
	doc, err := signedFrom(signed, RoleRoot)
	if err != nil {
		return nil, err
	}
	return rootFrom(doc)
}

func rootFrom(doc *Document) (*Root, error) {
	signed := doc.Signed
	field, ok := signed.Get("consistent_snapshot")
	if !ok {
		return nil, errors.New("missing field `consistent_snapshot`")
	}
	consistent, ok := field.AsBool()
	if !ok {
		return nil, errors.New("invalid type for `consistent_snapshot`, expected a boolean")
	}
	root := &Root{Document: doc, ConsistentSnapshot: consistent, Keys: map[string]Key{}, Roles: map[string]RoleKeys{}}
	keys, ok := signed.Get("keys")
	if !ok {
		return nil, errors.New("missing field `keys`")
	}
	keyMembers, ok := keys.AsObject()
	if !ok {
		return nil, errors.New("invalid type for `keys`, expected a map")
	}
	for _, m := range keyMembers {
		raw, err := hex.DecodeString(m.Key)
		if err != nil {
			return nil, fmt.Errorf("invalid key ID %q: %w", m.Key, err)
		}
		id := hex.EncodeToString(raw)
		key, err := parseKey(m.Value)
		if err != nil {
			return nil, err
		}
		calculated, err := key.ID()
		if err != nil {
			return nil, err
		}
		if calculated != id {
			return nil, fmt.Errorf("Invalid key ID %s: calculated %s", id, calculated)
		}
		if _, dup := root.Keys[id]; dup {
			return nil, fmt.Errorf("Duplicate key ID: %s", id)
		}
		root.Keys[id] = key
		root.KeyOrder = append(root.KeyOrder, id)
	}
	roles, ok := signed.Get("roles")
	if !ok {
		return nil, errors.New("missing field `roles`")
	}
	roleMembers, ok := roles.AsObject()
	if !ok {
		return nil, errors.New("invalid type for `roles`, expected a map")
	}
	for _, m := range roleMembers {
		switch m.Key {
		case RoleRoot, RoleTargets, RoleSnapshot, RoleTimestamp, "delegated-targets":
		default:
			return nil, fmt.Errorf("unknown variant `%s`, expected one of `root`, `snapshot`, `targets`, `timestamp`, `delegated-targets`", m.Key)
		}
		ids, ok := m.Value.Get("keyids")
		if !ok {
			return nil, errors.New("missing field `keyids`")
		}
		items, ok := ids.AsArray()
		if !ok {
			return nil, errors.New("invalid type for `keyids`, expected a sequence")
		}
		var keyIDs []string
		for _, item := range items {
			text, ok := item.AsString()
			if !ok {
				return nil, errors.New("invalid type for a key ID, expected a string")
			}
			raw, err := hex.DecodeString(text)
			if err != nil {
				return nil, fmt.Errorf("invalid key ID %q: %w", text, err)
			}
			keyIDs = append(keyIDs, hex.EncodeToString(raw))
		}
		threshold, err := u64Field(m.Value, "threshold", true)
		if err != nil {
			return nil, err
		}
		root.Roles[m.Key] = RoleKeys{KeyIDs: keyIDs, Threshold: threshold}
	}
	return root, nil
}

// ParseRole parses a targets, snapshot, or timestamp metadata file.
func ParseRole(data []byte, role string) (*Document, error) {
	doc, err := parseDocument(data, role)
	if err != nil {
		return nil, err
	}
	switch role {
	case RoleSnapshot, RoleTimestamp:
		if _, err := Meta(doc); err != nil {
			return nil, err
		}
	case RoleTargets:
		if _, err := TargetsOf(doc); err != nil {
			return nil, err
		}
	}
	return doc, nil
}

// Meta extracts the `meta` map from a snapshot or timestamp document.
func Meta(doc *Document) (map[string]Metafile, error) {
	meta, ok := doc.Signed.Get("meta")
	if !ok {
		return nil, errors.New("missing field `meta`")
	}
	members, ok := meta.AsObject()
	if !ok {
		return nil, errors.New("invalid type for `meta`, expected a map")
	}
	out := make(map[string]Metafile, len(members))
	for _, m := range members {
		var file Metafile
		if _, ok := m.Value.AsObject(); !ok {
			return nil, errors.New("invalid type: expected a metafile object")
		}
		if _, ok := m.Value.Get("length"); ok {
			length, err := u64Field(m.Value, "length", false)
			if err != nil {
				return nil, err
			}
			file.Length = &length
		}
		if hashes, ok := m.Value.Get("hashes"); ok {
			sum, _, err := hexField(hashes, "sha256")
			if err != nil {
				return nil, err
			}
			file.SHA256 = sum
			file.HasHashes = true
		}
		version, err := u64Field(m.Value, "version", true)
		if err != nil {
			return nil, err
		}
		file.Version = version
		out[m.Key] = file
	}
	return out, nil
}

// TargetsOf extracts targets and delegation counts from a targets document.
func TargetsOf(doc *Document) (*Targets, error) {
	out := &Targets{Document: doc, Entries: map[string]TargetEntry{}}
	targets, ok := doc.Signed.Get("targets")
	if !ok {
		return nil, errors.New("missing field `targets`")
	}
	members, ok := targets.AsObject()
	if !ok {
		return nil, errors.New("invalid type for `targets`, expected a map")
	}
	for _, m := range members {
		if m.Key == "" || m.Key == ".." {
			return nil, fmt.Errorf("unsafe target name %q", m.Key)
		}
		length, err := u64Field(m.Value, "length", false)
		if err != nil {
			return nil, err
		}
		hashes, ok := m.Value.Get("hashes")
		if !ok {
			return nil, errors.New("missing field `hashes`")
		}
		sum, _, err := hexField(hashes, "sha256")
		if err != nil {
			return nil, err
		}
		custom := core.ObjectValue()
		if c, ok := m.Value.Get("custom"); ok {
			if _, ok := c.AsObject(); !ok {
				return nil, errors.New("invalid type for `custom`, expected a map")
			}
			custom = c.Clone()
		}
		out.Entries[m.Key] = TargetEntry{Length: length, SHA256: sum, Custom: custom}
	}
	if delegations, ok := doc.Signed.Get("delegations"); ok && !delegations.IsNull() {
		keys, ok := delegations.Get("keys")
		if !ok {
			return nil, errors.New("missing field `keys`")
		}
		keyMembers, ok := keys.AsObject()
		if !ok {
			return nil, errors.New("invalid type for `keys`, expected a map")
		}
		for _, m := range keyMembers {
			key, err := parseKey(m.Value)
			if err != nil {
				return nil, err
			}
			id, err := key.ID()
			if err != nil {
				return nil, err
			}
			if raw, err := hex.DecodeString(m.Key); err != nil || hex.EncodeToString(raw) != id {
				return nil, fmt.Errorf("Invalid key ID %s: calculated %s", m.Key, id)
			}
		}
		roles, ok := delegations.Get("roles")
		if !ok {
			return nil, errors.New("missing field `roles`")
		}
		items, ok := roles.AsArray()
		if !ok {
			return nil, errors.New("invalid type for `roles`, expected a sequence")
		}
		out.DelegatedRoles = len(items)
	}
	return out, nil
}

// toughForm normalizes signed the way tough re-serializes it before verifying
// signatures (normalizing `expires` precision and dropping empty `custom` or
// null `delegations`).
func toughForm(role string, signed core.Value) core.Value {
	out := signed.Clone()
	if text, ok := out.Get("expires"); ok {
		if s, ok := text.AsString(); ok {
			if t, err := ParseTimestamp(s); err == nil {
				out.Set("expires", core.StringValue(FormatExpires(t)))
			}
		}
	}
	if role == RoleTargets {
		if d, ok := out.Get("delegations"); ok && d.IsNull() {
			out.Remove("delegations")
		}
		if targets := out.GetMut("targets"); targets != nil {
			for _, name := range targets.Keys() {
				target := targets.GetMut(name)
				if c, ok := target.Get("custom"); ok && c.Len() == 0 {
					if _, isObject := c.AsObject(); isObject {
						target.Remove("custom")
					}
				}
			}
		}
	}
	return out
}

// VerifyRole checks that doc meets its role's signature threshold over
// canonicalized metadata.
func (r *Root) VerifyRole(doc *Document) error {
	roleKeys, ok := r.Roles[doc.Role]
	if !ok {
		return task.Errorf("Role %s missing from root metadata", doc.Role)
	}
	data, err := Canonical(toughForm(doc.Role, doc.Signed))
	if err != nil {
		return task.Errorf("Failed to serialize %s role as JSON: %s", doc.Role, err)
	}
	seen := map[string]bool{}
	var valid uint64
	for _, sig := range doc.Signatures {
		if seen[sig.KeyID] {
			return task.Errorf("Duplicate key ID: %s", sig.KeyID)
		}
		seen[sig.KeyID] = true
		if !slices.Contains(roleKeys.KeyIDs, sig.KeyID) {
			continue
		}
		if key, ok := r.Keys[sig.KeyID]; ok && key.verify(data, sig.Sig) {
			valid++
		}
	}
	if valid < roleKeys.Threshold {
		return task.Errorf("Signature threshold of %d not met for role %s (%d valid signatures)", roleKeys.Threshold, doc.Role, valid)
	}
	return nil
}

// KeyIDOf looks up signer's matching key ID in the root.
func (r *Root) KeyIDOf(signer Signer) (string, bool) {
	key := signer.Key()
	for _, id := range r.KeyOrder {
		if r.Keys[id].equal(key) {
			return id, true
		}
	}
	return "", false
}

// RoleHas reports whether role authorizes key ID id.
func (r *Root) RoleHas(role, id string) bool {
	keys, ok := r.Roles[role]
	return ok && slices.Contains(keys.KeyIDs, id)
}

// SignerID returns the canonical key ID of signer's public key.
func SignerID(signer Signer) (string, error) { return signer.Key().ID() }

// Sign signs signed with every matching signer authorized for role and returns
// the pretty-printed metadata file bytes.
func (r *Root) Sign(role string, signed core.Value, signers []Signer) ([]byte, error) {
	keys := map[string]Signer{}
	for _, signer := range signers {
		if id, ok := r.KeyIDOf(signer); ok {
			keys[id] = signer
		}
	}
	if len(keys) == 0 {
		return nil, task.Error("Unable to match any of the provided keys with root.json")
	}
	roleKeys, ok := r.Roles[role]
	if !ok {
		return nil, task.Errorf("Unable to find signing keys for role '%s'", role)
	}
	data, err := Canonical(signed)
	if err != nil {
		return nil, task.Errorf("Failed to serialize role '%s' for signing: %s", role, err)
	}
	ids := make([]string, 0, len(keys))
	for id := range keys {
		if slices.Contains(roleKeys.KeyIDs, id) {
			ids = append(ids, id)
		}
	}
	sort.Strings(ids)
	signatures := []core.Value{}
	for _, id := range ids {
		sig, err := keys[id].Sign(data)
		if err != nil {
			return nil, task.Errorf("Failed to sign message: %s", err)
		}
		signatures = append(signatures, core.ObjectValue(
			core.Member{Key: "keyid", Value: core.StringValue(id)},
			core.Member{Key: "sig", Value: core.StringValue(hex.EncodeToString(sig))},
		))
	}
	if role != RoleRoot && roleKeys.Threshold > uint64(len(signatures)) {
		return nil, task.Errorf("Unable to find signing keys for role '%s'", role)
	}
	return Pretty(core.ObjectValue(
		core.Member{Key: "signed", Value: signed},
		core.Member{Key: "signatures", Value: core.ArrayValue(signatures...)},
	)), nil
}

// CheckStable verifies that every role has at least threshold keys (error
// wording matches tough's RepositoryEditor::new).
func (r *Root) CheckStable() error {
	for _, role := range sortedRoles(r.Roles) {
		keys := r.Roles[role]
		if keys.Threshold > uint64(len(keys.KeyIDs)) {
			return task.Errorf("Unstable root; found %s keys for role %d, threshold is %d", role, keys.Threshold, len(keys.KeyIDs))
		}
	}
	return nil
}

func sortedRoles(roles map[string]RoleKeys) []string {
	out := make([]string, 0, len(roles))
	for role := range roles {
		out = append(out, role)
	}
	sort.Strings(out)
	return out
}

// MetafileValue builds a snapshot/timestamp meta entry in tough's field order.
func MetafileValue(length uint64, sha256 []byte, version uint64) core.Value {
	return core.ObjectValue(
		core.Member{Key: "length", Value: core.U64Value(length)},
		core.Member{Key: "hashes", Value: core.ObjectValue(core.Member{Key: "sha256", Value: core.StringValue(hex.EncodeToString(sha256))})},
		core.Member{Key: "version", Value: core.U64Value(version)},
	)
}

// RoleValue creates the base fields of a role object in tough's field order.
func RoleValue(role string, version uint64, expires time.Time) core.Value {
	return core.ObjectValue(
		core.Member{Key: "_type", Value: core.StringValue(role)},
		core.Member{Key: "spec_version", Value: core.StringValue(SpecVersion)},
		core.Member{Key: "version", Value: core.U64Value(version)},
		core.Member{Key: "expires", Value: core.StringValue(FormatExpires(expires))},
	)
}

// Renewed rebuilds a snapshot or timestamp role with updated version, expiry,
// and meta while preserving any unknown extra fields.
func Renewed(signed core.Value, version uint64, expires time.Time, meta core.Value) core.Value {
	role, _ := stringField(signed, "_type")
	spec, _ := stringField(signed, "spec_version")
	out := core.ObjectValue(
		core.Member{Key: "_type", Value: core.StringValue(role)},
		core.Member{Key: "spec_version", Value: core.StringValue(spec)},
		core.Member{Key: "version", Value: core.U64Value(version)},
		core.Member{Key: "expires", Value: core.StringValue(FormatExpires(expires))},
		core.Member{Key: "meta", Value: meta},
	)
	members, _ := signed.AsObject()
	for _, m := range members {
		switch m.Key {
		case "_type", "spec_version", "version", "expires", "meta":
		default:
			out.Set(m.Key, m.Value.Clone())
		}
	}
	return out
}

// ToughMeta normalizes a `meta` object into tough's field order (length,
// hashes with sha256 first, version, then any unknown fields).
func ToughMeta(meta core.Value) core.Value {
	out := core.ObjectValue()
	members, _ := meta.AsObject()
	for _, m := range members {
		entry := core.ObjectValue()
		if length, ok := m.Value.Get("length"); ok {
			entry.Set("length", length.Clone())
		}
		if hashes, ok := m.Value.Get("hashes"); ok {
			h := core.ObjectValue()
			if sum, ok := hashes.Get("sha256"); ok {
				h.Set("sha256", sum.Clone())
			}
			hashMembers, _ := hashes.AsObject()
			for _, hm := range hashMembers {
				if hm.Key != "sha256" {
					h.Set(hm.Key, hm.Value.Clone())
				}
			}
			entry.Set("hashes", h)
		}
		if version, ok := m.Value.Get("version"); ok {
			entry.Set("version", version.Clone())
		}
		entryMembers, _ := m.Value.AsObject()
		for _, em := range entryMembers {
			switch em.Key {
			case "length", "hashes", "version":
			default:
				entry.Set(em.Key, em.Value.Clone())
			}
		}
		out.Set(m.Key, entry)
	}
	return out
}
