package edit

import (
	"fmt"
	"slices"

	"github.com/ibanks42/rc2fx/desktop/core"
)

// SmartPadsSource is the mixer source ID for SMART Pads, which is mixed
// internally on the RØDECaster and always hidden in RC2FX.
const SmartPadsSource = 11

// InventoryCache holds the last confirmed show inventory, any refresh error,
// and whether a refresh is currently in flight.
type InventoryCache struct {
	Inventory *core.StockInventory
	Error     *string
	Pending   bool
}

// VisibleSources returns the inventory sources shown in the UI, filtering out
// SMART Pads even for previously cached inventories.
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

// Receive applies a show refresh result and returns true if the cached inventory was updated.
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
	// Keep the last confirmed inventory on error; a failed read doesn't mean assignments changed.
	return false
}

// AssignmentLabel formats a source's fader assignment for display.
func AssignmentLabel(source *core.StockSource) string {
	if source.Fader == nil {
		return "Not assigned to a fader"
	}
	return fmt.Sprintf("Fader %d", *source.Fader)
}

// SourceDetail formats a source's subtitle line in the channel list, appending
// note when the source is mapped to an RC2FX target.
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

// ChangedPaths returns the symmetric difference between saved and draft
// (removed paths first, then added paths).
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
