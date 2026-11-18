package embed

// The firmware catalog's shape: the profiles a payload supports and the
// runtime contract each pins.
//
// The payload's catalog is checked with the same typed decoding when it is
// staged for a build (see cmd/stage-payload) as the installer uses at run
// time. A catalog that a run-time reader would reject must never reach a
// binary.

// JobLifetime is a runtime profile's job lifetime contract.
type JobLifetime struct {
	_   struct{} `serde:"default"`
	ABI string   `json:"abi"`
}

// RuntimeProfile is the runtime contract a profile pins.
type RuntimeProfile struct {
	_           struct{}    `serde:"default"`
	CallbackABI string      `json:"callbackAbi"`
	RouteLayout string      `json:"routeLayout"`
	SourceEnum  string      `json:"sourceEnum"`
	SampleRate  uint32      `json:"sampleRate"`
	BlockFrames uint32      `json:"blockFrames"`
	JobLifetime JobLifetime `json:"jobLifetime"`
}

// Profile is one qualified RØDE firmware release.
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

// Catalog is `firmware-profiles.json`.
type Catalog struct {
	_        struct{}  `serde:"default"`
	Schema   uint32    `json:"schemaVersion"`
	Profiles []Profile `json:"profiles"`
}

// SameRuntime is whether a and b are both nil or equal runtimes.
func SameRuntime(a, b *RuntimeProfile) bool {
	if a == nil || b == nil {
		return a == nil && b == nil
	}
	return *a == *b
}

// Clone copies a profile, including its runtime.
func (p Profile) Clone() Profile {
	if p.Runtime != nil {
		r := *p.Runtime
		p.Runtime = &r
	}
	return p
}

// Clone copies a catalog.
func (c Catalog) Clone() Catalog {
	out := Catalog{Schema: c.Schema, Profiles: make([]Profile, len(c.Profiles))}
	for i, p := range c.Profiles {
		out.Profiles[i] = p.Clone()
	}
	return out
}
