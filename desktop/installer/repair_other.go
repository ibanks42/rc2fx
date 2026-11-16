//go:build !linux

package installer

// Repairing the mixer from a computer is Linux only until libext2fs is
// built and tested on macOS and Windows.

const repairUnavailable = Error("Repairing the mixer from this computer isn't available on this system yet")

func (e *Engine) repairInspect() (*RepairFindings, error) { return nil, repairUnavailable }

func (e *Engine) repair([]RepairTarget) (*RepairResult, error) { return nil, repairUnavailable }
