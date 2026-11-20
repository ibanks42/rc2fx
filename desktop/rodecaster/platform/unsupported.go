//go:build !linux && !windows && !darwin

package platform

const unsupported = platformError("USB installation is not supported on this system")

type host struct{}

// Host is this system's USB access.
func Host() Platform { return host{} }

func (host) Inventory() ([]HID, error)          { return nil, unsupported }
func (host) Open(*HID) (Reports, error)         { return nil, unsupported }
func (host) OpenControl(*HID) (Reports, error)  { return nil, unsupported }
func (host) Volumes() ([]Volume, error)         { return nil, unsupported }
func (host) Mount(Volume) (Volume, error)       { return Volume{}, unsupported }
func (host) MediaPresent(*Volume) (bool, error) { return false, unsupported }
func (host) Eject(*Volume) error                { return unsupported }
