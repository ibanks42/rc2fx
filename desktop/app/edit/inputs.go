package edit

// inputs.rs's show cache, selection.rs's changed paths and restart.rs's
// restart candidate.

import (
	"fmt"
	"math"
	"slices"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// SmartPadsSource is the show source id of SMART Pads, which RC2FX lists
// nowhere: the RØDECaster mixes it internally.
const SmartPadsSource = 11

// InventoryCache is the part of inputs.rs ShowInputs that the show
// refreshes: the last confirmed inventory, the last refresh's failure, and
// whether a refresh is out.
type InventoryCache struct {
	Inventory *core.StockInventory
	Error     *string
	Pending   bool
}

// VisibleSources is the inventory's sources the app lists. Filtered on
// every access, including inventory cached before this UI policy.
func (c *InventoryCache) VisibleSources() []*core.StockSource {
	if c.Inventory == nil {
		return nil
	}
	var sources []*core.StockSource
	for i := range c.Inventory.Inputs {
		if c.Inventory.Inputs[i].SourceID != SmartPadsSource {
			sources = append(sources, &c.Inventory.Inputs[i])
		}
	}
	return sources
}

// Receive takes a refresh's result; true when it replaced the inventory.
func (c *InventoryCache) Receive(inventory core.StockInventory, err error) bool {
	c.Pending = false
	if err == nil && inventory.Available {
		c.Error = nil
		c.Inventory = &inventory
		return true
	}
	message := inventory.Message
	if err != nil {
		message = err.Error()
	}
	c.Error = &message
	// Preserve the last confirmed names/selection, with an explicit stale
	// label. A failed read does not prove assignments changed.
	return false
}

// AssignmentLabel is where a source sits: its fader, or that it has none.
func AssignmentLabel(source *core.StockSource) string {
	if source.Fader == nil {
		return "Not assigned to a fader"
	}
	return fmt.Sprintf("Fader %d", *source.Fader)
}

// SourceDetail is a source's line in the channel list: its fader, then note
// ("" for none) when the source has an RC2FX path.
func SourceDetail(source *core.StockSource, note string) string {
	fader := "Unassigned"
	if source.Fader != nil {
		fader = fmt.Sprintf("Fader %d", *source.Fader)
	}
	if note != "" && source.TargetID != "" {
		return fader + " · " + note
	}
	return fader
}

// ChangedPaths is the paths in saved and not in draft, then those in draft
// and not in saved.
func ChangedPaths(saved, draft []string) []string {
	var changed []string
	for _, id := range saved {
		if !slices.Contains(draft, id) {
			changed = append(changed, id)
		}
	}
	for _, id := range draft {
		if !slices.Contains(saved, id) {
			changed = append(changed, id)
		}
	}
	return changed
}

// RestartCandidate is whether the mixer allows a processing restart of this
// state's engine.
func RestartCandidate(state *core.DeviceState) bool {
	return state.Engine.RestartEligible() && state.Engine.Generation < math.MaxUint64
}
