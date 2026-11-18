//go:build !linux

package installer

// Repairing the mixer from a computer is Linux only until libext2fs is
// built and tested on macOS and Windows.

// RepairSupported: this system can check and repair a mixer in storage
// mode. The app offers Repair mixer only where it is.
const RepairSupported = false

const repairUnavailable = Error("Repairing the mixer from this computer isn't available on this system yet")

func (e *Engine) repairInspect() (*RepairFindings, error) { return nil, repairUnavailable }

func (e *Engine) repair([]RepairTarget) (*RepairResult, error) { return nil, repairUnavailable }
