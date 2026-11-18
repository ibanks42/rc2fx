//go:build !linux && !darwin && !windows

package disk

func find() ([]Info, error) { return nil, ErrUnsupported }

func open(Info, bool) (Disk, error) { return nil, ErrUnsupported }
