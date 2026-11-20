package tuf

import (
	"encoding/hex"
	"fmt"
	"net/url"
	"sort"
	"strings"

	"github.com/theupdateframework/go-tuf/v2/metadata"

	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

// DistinctKeys refuses a root that lists one key under two IDs for a role.
// The client counts signatures by key ID, so such a root could meet a
// threshold of two with a single key. RC2FX's roles need one signature each
// today; this keeps a future threshold honest. The mixer makes the same
// check.
func DistinctKeys(root *metadata.RootType) error {
	roles := make([]string, 0, len(root.Roles))
	for role := range root.Roles {
		roles = append(roles, role)
	}
	sort.Strings(roles)
	for _, role := range roles {
		material := map[[2]string]bool{}
		for _, id := range root.Roles[role].KeyIDs {
			key := root.Keys[id]
			if key == nil {
				continue
			}
			m := [2]string{key.Type, strings.TrimSpace(key.Value.PublicKey)}
			if material[m] {
				return fmt.Errorf("the signed root lists one %s key under two IDs", role)
			}
			material[m] = true
		}
	}
	return nil
}

// TargetSHA256 is the SHA-256 a target is signed with, lowercase.
func TargetSHA256(target *metadata.TargetFiles) (string, bool) {
	h, ok := target.Hashes["sha256"]
	if !ok || len(h) != 32 {
		return "", false
	}
	return hex.EncodeToString(h), true
}

// OfficialURL is whether u is plain HTTPS on host with no credentials or
// custom port.
func OfficialURL(u *url.URL, host string) bool {
	return u.Scheme == "https" && u.Hostname() == host && u.Port() == "" && u.User == nil
}

// CustomValue is a target's custom field `key`, or nil.
func CustomValue(target *metadata.TargetFiles, key string) any {
	if target == nil || target.Custom == nil {
		return nil
	}
	value, err := sjson.Value(*target.Custom)
	if err != nil {
		return nil
	}
	object, _ := value.(map[string]any)
	return object[key]
}

// CustomString is a target's string custom field `key`, or "".
func CustomString(target *metadata.TargetFiles, key string) string {
	s, _ := CustomValue(target, key).(string)
	return s
}

// CustomHas reports whether a target's custom object contains `key`.
func CustomHas(target *metadata.TargetFiles, key string) bool {
	if target == nil || target.Custom == nil {
		return false
	}
	value, err := sjson.Value(*target.Custom)
	if err != nil {
		return false
	}
	object, ok := value.(map[string]any)
	if !ok {
		return false
	}
	_, ok = object[key]
	return ok
}
