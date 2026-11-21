package state

import "time"

// App-side connection intent, kept separate from observed transport state
// (session.Connected, streamConnected, streamError).

type linkPhase uint8

const (
	linkOff linkPhase = iota
	linkConnecting
	linkOn
	linkRetry
	linkPausedForFlash
)

const retryWait = 3 * time.Second

// LinkIntent tracks whether the app wants to connect and when to retry next.
// The zero value is linkOff.
type LinkIntent struct {
	phase   linkPhase
	retryAt time.Time
}

// Auto reports whether the app is actively maintaining or retrying a connection.
func (l LinkIntent) Auto() bool {
	return l.phase == linkConnecting || l.phase == linkOn || l.phase == linkRetry
}

// Connecting reports whether a connect command is currently in flight.
func (l LinkIntent) Connecting() bool { return l.phase == linkConnecting }

// Begin starts a connection attempt and cancels any scheduled retry.
func (l *LinkIntent) Begin(sent bool) {
	*l = LinkIntent{phase: linkOn}
	if sent {
		l.phase = linkConnecting
	}
}

// Stop disables auto-reconnect unless paused for an active flash (which must
// still resume afterward).
func (l *LinkIntent) Stop() {
	if l.phase != linkPausedForFlash {
		*l = LinkIntent{}
	}
}

// Connected records that the mixer answered a connect attempt.
func (l *LinkIntent) Connected() {
	if l.phase == linkConnecting || l.phase == linkRetry {
		*l = LinkIntent{phase: linkOn}
	}
}

// Failed records a transport failure, scheduling a retry if disconnected and
// auto-reconnect is active.
func (l *LinkIntent) Failed(connected bool, now time.Time) {
	switch {
	case l.Auto() && !connected:
		*l = LinkIntent{phase: linkRetry, retryAt: now.Add(retryWait)}
	case l.phase == linkConnecting:
		*l = LinkIntent{phase: linkOn}
	}
}

// RetrySoon brings a scheduled retry forward to at most wait from now.
func (l *LinkIntent) RetrySoon(now time.Time, wait time.Duration) {
	if at := now.Add(wait); l.phase == linkRetry && at.Before(l.retryAt) {
		l.retryAt = at
	}
}

// PauseForFlash suspends reconnect attempts until FlashDone is called.
func (l *LinkIntent) PauseForFlash() { *l = LinkIntent{phase: linkPausedForFlash} }

// FlashDone ends a flash pause and schedules an immediate reconnect attempt.
func (l *LinkIntent) FlashDone(now time.Time) bool {
	if l.phase != linkPausedForFlash {
		return false
	}
	*l = LinkIntent{phase: linkRetry, retryAt: now}
	return true
}

// Due reports whether a scheduled retry should run at now.
func (l LinkIntent) Due(connected bool, now time.Time) bool {
	return l.phase == linkRetry && !connected && !now.Before(l.retryAt)
}
