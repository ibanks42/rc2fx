package installer

// What the two signed repositories (mixer releases and app releases) share:
// the HTTPS transport, the metadata store and a check of each trusted root.
// Verification itself follows sigstore-tuf, the library the mixer uses (see
// internal/tuf).

import (
	"encoding/hex"
	"fmt"
	"net/url"
	"sort"
	"strings"
	"time"

	"github.com/theupdateframework/go-tuf/v2/metadata"

	"github.com/ibanks42/rc2fx/desktop/installer/internal/tuf"
)

// tufHTTP is an HTTPS repository with metadata under metadataURL and
// targets under targetsURL. A stalled connection fails after a minute;
// total bounds the whole of any one request.
func tufHTTP(metadataURL, targetsURL *url.URL, total time.Duration) tuf.Repository {
	return tuf.NewHTTPRepository(metadataURL, targetsURL, total)
}

// tufConfig is the limits for a repository whose largest target is
// maxTarget bytes.
func tufConfig(maxTarget uint64) tuf.Config {
	c := tuf.DefaultConfig()
	c.TargetMaxLength = maxTarget
	return c
}

// metadataOnly is the metadata store for a repository's rollback floors:
// verified role files only. The client would also keep a copy of every
// downloaded target here; the mixer payloads have their own cache.
type metadataOnly struct{ tuf.FileStore }

func (m metadataOnly) Load(name string) ([]byte, bool, error) {
	if strings.HasPrefix(name, "targets/") {
		return nil, false, nil
	}
	return m.FileStore.Load(name)
}

func (m metadataOnly) Store(name string, data []byte) error {
	if strings.HasPrefix(name, "targets/") {
		return nil
	}
	return m.FileStore.Store(name, data)
}

// distinctKeys refuses a root that lists one key under two IDs for a role.
// The client counts signatures by key ID, so such a root could meet a
// threshold of two with a single key. RC2FX's roles need one signature each
// today; this keeps a future threshold honest. The mixer makes the same
// check.
func distinctKeys(root *metadata.RootType) error {
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

// targetSha256 is the SHA-256 a target is signed with, lowercase.
func targetSha256(target *metadata.TargetFiles) (string, bool) {
	h, ok := target.Hashes["sha256"]
	if !ok || len(h) != 32 {
		return "", false
	}
	return hex.EncodeToString(h), true
}
