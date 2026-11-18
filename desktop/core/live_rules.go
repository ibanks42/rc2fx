package core

// LiveNeedsSave reports whether moving the running audio from running to
// candidate needs the engine restarted, which a live edit cannot do: a
// channel's kind or channel count, suppression switched on or off, its
// lookahead, or the effect order while suppression runs. The mixer decides
// (PUT api/v1/live answers 422 liveRequiresSave); this lets the app
// save such a change straight away, after asking, instead of sending it live.
func LiveNeedsSave(running, candidate []Target) bool {
	byID := make(map[string]*Target, len(running))
	for i := range running {
		byID[running[i].ID] = &running[i]
	}
	for i := range candidate {
		next := &candidate[i]
		now, ok := byID[next.ID]
		if !ok || now.Kind != next.Kind || now.Channels != next.Channels {
			return true
		}
		if now.Suppression.Enabled != next.Suppression.Enabled {
			return true
		}
		if !next.Suppression.Enabled {
			continue
		}
		if now.Suppression.Lookahead != next.Suppression.Lookahead {
			return true
		}
		if len(now.Order) != len(next.Order) {
			return true
		}
		for j := range now.Order {
			if now.Order[j] != next.Order[j] {
				return true
			}
		}
	}
	return len(candidate) != len(running)
}
