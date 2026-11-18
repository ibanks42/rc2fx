package app

import "time"

// What the app is doing about its connection to the mixer: the app's side of
// the connection lifecycle. What the transport observes (session.Connected,
// streamConnected, streamError) is kept apart, as observations.

// linkPhase is the app's intent for the connection.
type linkPhase uint8

const (
	// linkOff: not trying to connect. Before the first Connect, and after
	// Disconnect.
	linkOff linkPhase = iota
	// linkConnecting: a connect command is out and has not been answered.
	linkConnecting
	// linkOn: connecting is wanted and nothing is out or scheduled: the
	// mixer answered (connected), or the connect command could not be sent.
	linkOn
	// linkRetry: connecting is wanted and failed; the next attempt starts
	// at linkIntent.retryAt.
	linkRetry
	// linkPausedForFlash: the connection was dropped while the installer
	// flashes the mixer; once the flash is over the app looks for the mixer
	// again at once.
	linkPausedForFlash
)

// retryWait is how long after a failed attempt the next one starts.
const retryWait = 3 * time.Second

// linkIntent is the connection intent, with the time of the next attempt
// while one is scheduled. The zero value is linkOff.
type linkIntent struct {
	phase linkPhase
	// retryAt is when linkRetry tries again; zero in every other phase.
	retryAt time.Time
}

// auto is whether the app keeps trying to connect (the connection screen's
// "Listening…" rather than "Disconnected").
func (l linkIntent) auto() bool {
	return l.phase == linkConnecting || l.phase == linkOn || l.phase == linkRetry
}

// connecting is whether a connect command is out.
func (l linkIntent) connecting() bool { return l.phase == linkConnecting }

// pausedForFlash is whether the connection waits for a flash to finish.
func (l linkIntent) pausedForFlash() bool { return l.phase == linkPausedForFlash }

// begin starts an attempt, canceling any scheduled one; sent is whether the
// connect command went out.
func (l *linkIntent) begin(sent bool) {
	*l = linkIntent{phase: linkOn}
	if sent {
		l.phase = linkConnecting
	}
}

// stop is Disconnect: no more attempts. A pause for a flash stays, so the
// app still looks for the mixer once the flash is over.
func (l *linkIntent) stop() {
	if l.phase != linkPausedForFlash {
		*l = linkIntent{}
	}
}

// connected notes that the mixer answered the connect command.
func (l *linkIntent) connected() {
	if l.phase == linkConnecting || l.phase == linkRetry {
		*l = linkIntent{phase: linkOn}
	}
}

// failed notes a failure event at now. Any connect command out is over;
// while the app wants a connection and has none, it tries again after
// retryWait.
func (l *linkIntent) failed(connected bool, now time.Time) {
	switch {
	case l.auto() && !connected:
		*l = linkIntent{phase: linkRetry, retryAt: now.Add(retryWait)}
	case l.phase == linkConnecting:
		*l = linkIntent{phase: linkOn}
	}
}

// pauseForFlash drops the intent to connect until flashDone.
func (l *linkIntent) pauseForFlash() { *l = linkIntent{phase: linkPausedForFlash} }

// flashDone ends a pause for a flash: the next attempt starts at now. It
// reports whether there was a pause to end.
func (l *linkIntent) flashDone(now time.Time) bool {
	if l.phase != linkPausedForFlash {
		return false
	}
	*l = linkIntent{phase: linkRetry, retryAt: now}
	return true
}

// due is whether a scheduled attempt should start at now, with the session
// connected or not.
func (l linkIntent) due(connected bool, now time.Time) bool {
	return l.phase == linkRetry && !connected && !now.Before(l.retryAt)
}
