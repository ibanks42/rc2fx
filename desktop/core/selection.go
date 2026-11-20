package core

import (
	"slices"
	"strconv"
)

// Selection topology types; effects saves never modify path selection.

type SelectionPhase string

const (
	SelectionPhaseIdle     SelectionPhase = "idle"
	SelectionPhaseChanging SelectionPhase = "changing"
	SelectionPhaseError    SelectionPhase = "error"
)

var selectionPhases = []SelectionPhase{SelectionPhaseIdle, SelectionPhaseChanging, SelectionPhaseError}

func (p *SelectionPhase) UnmarshalText(text []byte) error {
	return enumUnmarshal(p, text, selectionPhases)
}

type SelectionStatus struct {
	ChannelCapacity    *uint32        `json:"channelCapacity"` // observed stock-graph channel cap, nil if unknown
	Status             SelectionPhase `json:"status"`
	Allowed            bool           `json:"allowed"`
	QualifiedTargetIDs []string       `json:"qualifiedTargetIds"`
	ActiveTargetIDs    []string       `json:"activeTargetIds"` // allocated by engine; Routing tracks wet edges separately
	PendingTargetIDs   []string       `json:"pendingTargetIds"`
	Message            string         `json:"message"`
}

func (s SelectionStatus) CheckCapacity(config Config, ids []string) (uint32, error) {
	if err := config.ValidateSelection(ids); err != nil {
		return 0, err
	}
	var channels uint32
	for _, target := range config.Targets {
		if slices.Contains(ids, target.ID) {
			channels += uint32(target.Channels)
		}
	}
	if s.ChannelCapacity != nil && channels > *s.ChannelCapacity {
		limit := *s.ChannelCapacity
		return 0, CoreError("These paths need " + strconv.FormatUint(uint64(channels), 10) +
			" audio channels; the mixer has room for " + strconv.FormatUint(uint64(limit), 10) +
			". Turn off another processing path first.")
	}
	return channels, nil
}

func (s SelectionStatus) Validate(config Config) error {
	if s.ChannelCapacity != nil && *s.ChannelCapacity > 128 {
		return CoreError("Invalid processing channel capacity")
	}
	if !ValidDisplayText(s.Message, MaxStatusText) {
		return CoreError("Invalid selection status message")
	}
	for _, ids := range [][]string{s.QualifiedTargetIDs, s.ActiveTargetIDs, s.PendingTargetIDs} {
		canonical := []string{}
		for _, target := range config.Targets {
			if slices.Contains(ids, target.ID) {
				canonical = append(canonical, target.ID)
			}
		}
		if !slices.Equal(canonical, ids) {
			return CoreError("Invalid processing path catalog or order")
		}
	}
	for _, ids := range [][]string{s.ActiveTargetIDs, s.PendingTargetIDs} {
		if err := config.ValidateSelection(ids); err != nil {
			return err
		}
	}
	// Allocation is observed, not a promise of physical qualification
	// (for example, a synthetic engine can allocate unqualified paths).
	for _, id := range s.PendingTargetIDs {
		if !slices.Contains(s.QualifiedTargetIDs, id) {
			return CoreError("Unqualified pending processing path")
		}
	}
	if s.Status == SelectionPhaseChanging && s.Allowed {
		return CoreError("A pending path change cannot allow another transition")
	}
	return nil
}

// Clone is a deep copy.
func (s SelectionStatus) Clone() SelectionStatus {
	if s.ChannelCapacity != nil {
		capacity := *s.ChannelCapacity
		s.ChannelCapacity = &capacity
	}
	s.QualifiedTargetIDs = slices.Clone(s.QualifiedTargetIDs)
	s.ActiveTargetIDs = slices.Clone(s.ActiveTargetIDs)
	s.PendingTargetIDs = slices.Clone(s.PendingTargetIDs)
	return s
}

// Equal compares field by field (deepEqual).
func (s SelectionStatus) Equal(other SelectionStatus) bool { return deepEqual(s, other) }

type SelectionRequest struct {
	ExpectedRevision   uint64   `json:"expectedRevision"`
	ExpectedGeneration uint64   `json:"expectedGeneration"`
	TargetIDs          []string `json:"targetIds"`
}

type PendingSelection struct {
	Request SelectionRequest
	config  Config
	mode    EngineMode
}

func (p PendingSelection) ValidateAcknowledgement(state DeviceState) error {
	if err := state.Validate(); err != nil {
		return err
	}
	native := p.mode == EngineModeNative
	expectedConfig := p.config.Clone()
	if native {
		if expectedConfig.Revision == maxUint64 {
			return CoreError("Settings revision exhausted")
		}
		expectedConfig.Revision++
		expectedConfig.SelectedTargets = slices.Clone(p.Request.TargetIDs)
	}
	expectedGeneration := optU64{value: p.Request.ExpectedGeneration, ok: true}
	if !native {
		expectedGeneration = checkedAddOne(p.Request.ExpectedGeneration)
	}
	expectedStatus := EnginePhaseRestarting
	if native {
		expectedStatus = EnginePhaseStarting
	}
	if !state.Config.Equal(expectedConfig) ||
		!expectedGeneration.is(state.Engine.Generation) ||
		state.Engine.Mode != p.mode ||
		state.Engine.Status != expectedStatus ||
		state.Engine.RestartAllowed ||
		state.Engine.AudioAdvancing ||
		(native && state.Engine.AppliedRevision != 0) ||
		state.Selection.Status != SelectionPhaseChanging ||
		!slices.Equal(state.Selection.PendingTargetIDs, p.Request.TargetIDs) {
		return CoreError("Path-change response does not match the requested selection and saved settings")
	}
	return nil
}
