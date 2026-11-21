//go:build !linux

package installer

// Host-side eMMC repair is currently Linux-only.

const RepairSupported = false

const repairUnavailable = Error("Repairing the mixer from this computer isn't available on this system yet")

func (e *Engine) repairInspect() (*RepairFindings, error) { return nil, repairUnavailable }

func (e *Engine) repair([]RepairTarget) (*RepairResult, error) { return nil, repairUnavailable }
