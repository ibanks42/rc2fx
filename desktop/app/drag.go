package app

// Encodes drag-and-drop payloads (chain stages and EQ bands) into uint64
// gk.DragValue IDs with the kind in the upper 32 bits and index in the lower 32.

const (
	dragStage = 1
	dragBand  = 2
)

func dragID(kind, index int) uint64 { return uint64(kind)<<32 | uint64(uint32(index)) }

func dragKind(id uint64) (kind, index int) { return int(id >> 32), int(uint32(id)) }

func dragIDs(kind, n int) []uint64 {
	ids := make([]uint64, n)
	for i := range ids {
		ids[i] = dragID(kind, i)
	}
	return ids
}
