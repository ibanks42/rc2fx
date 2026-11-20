package core

// LiveNeedsSave reports whether transitioning from running to candidate
// requires an engine restart (changing target kind/channels, toggling
// suppression, changing lookahead, or reordering effects while suppression is
// on). The mixer enforces this authoritatively (422 liveRequiresSave), but
// checking here lets the app prompt and save directly.
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
