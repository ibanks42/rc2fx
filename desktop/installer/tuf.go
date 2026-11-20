package installer

// Shared TUF transport and metadata store helpers for mixer updates.
// Verification itself lives in desktop/internal/tuf.

import (
	"net/url"
	"strings"
	"time"

	"github.com/theupdateframework/go-tuf/v2/metadata"

	"github.com/ibanks42/rc2fx/desktop/internal/tuf"
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

func distinctKeys(root *metadata.RootType) error { return tuf.DistinctKeys(root) }

func targetSha256(target *metadata.TargetFiles) (string, bool) { return tuf.TargetSHA256(target) }
