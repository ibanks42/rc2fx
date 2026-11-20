//go:build darwin && !cgo

package platform

const noCgo = platformError("this build has no macOS USB access (built without cgo)")

type host struct{}

// Host is this system's USB access.
func Host() Platform { return host{} }

func (host) Inventory() ([]HID, error)          { return nil, noCgo }
func (host) Open(*HID) (Reports, error)         { return nil, noCgo }
func (host) OpenControl(*HID) (Reports, error)  { return nil, noCgo }
func (host) Volumes() ([]Volume, error)         { return nil, noCgo }
func (host) Mount(Volume) (Volume, error)       { return Volume{}, noCgo }
func (host) MediaPresent(*Volume) (bool, error) { return false, noCgo }
func (host) Eject(*Volume) error                { return noCgo }
