package embed

// Schema for firmware-profiles.json. Shared between stage-payload at build
// time and the installer at runtime so malformed catalogs fail the build.

type JobLifetime struct {
	_   struct{} `serde:"default"`
	ABI string   `json:"abi"`
}

type RuntimeProfile struct {
	_           struct{}    `serde:"default"`
	CallbackABI string      `json:"callbackAbi"`
	RouteLayout string      `json:"routeLayout"`
	SourceEnum  string      `json:"sourceEnum"`
	SampleRate  uint32      `json:"sampleRate"`
	BlockFrames uint32      `json:"blockFrames"`
	JobLifetime JobLifetime `json:"jobLifetime"`
}

type Profile struct {
	_                 struct{}        `serde:"default"`
	MinimumAppVersion string          `json:"minimumAppVersion"`
	ID                string          `json:"id"`
	Model             string          `json:"model"`
	Version           string          `json:"version"`
	MixerSHA          string          `json:"mixerSha256"`
	LauncherSHA       string          `json:"launcherSha256"`
	ArchiveSHA        string          `json:"archiveSha256"`
	ArchiveMD5        string          `json:"archiveMd5"`
	Runtime           *RuntimeProfile `json:"runtime"`
}

type Catalog struct {
	_        struct{}  `serde:"default"`
	Schema   uint32    `json:"schemaVersion"`
	Profiles []Profile `json:"profiles"`
}

func SameRuntime(a, b *RuntimeProfile) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return *a == *b
}

func (p Profile) Clone() Profile {
	if p.Runtime != nil {
		r := *p.Runtime
		p.Runtime = &r
	}
	return p
}

func (c Catalog) Clone() Catalog {
	out := Catalog{Schema: c.Schema, Profiles: make([]Profile, len(c.Profiles))}
	for i, p := range c.Profiles {
		out.Profiles[i] = p.Clone()
	}
	return out
}
