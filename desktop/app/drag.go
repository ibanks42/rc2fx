package app

// The app's two drags, a chain stage and an EQ band, as gk.DragValue ids:
// one value type for every drag of the app, the kind in the top 32 bits and
// the stage's or band's index below. A drop target takes only its own kind
// (gk.DropAccept).

// The drag kinds.
const (
	dragStage = 1
	dragBand  = 2
)

// dragID is a drag value's id: its kind and its index.
func dragID(kind, index int) uint64 { return uint64(kind)<<32 | uint64(uint32(index)) }

// dragKind splits an id from dragID.
func dragKind(id uint64) (kind, index int) { return int(id >> 32), int(uint32(id)) }

// dragIDs is every id of a kind with indexes below n: what a drop target of
// that kind accepts.
func dragIDs(kind, n int) []uint64 {
	ids := make([]uint64, n)
	for i := range ids {
		ids[i] = dragID(kind, i)
	}
	return ids
}
