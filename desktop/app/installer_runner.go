package app

import (
	"errors"
	"sync"
	"sync/atomic"

	"gpuikit.dev/gk"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
)

// installerOutcome is a finished request and its result.
type installerOutcome struct {
	request installer.Request
	reply   installer.Reply
	err     error
}

// helperSlot is the privileged helper, once access is granted, shared by
// the access goroutine (which fills it) and the request goroutine.
type helperSlot struct {
	mu     sync.Mutex
	helper *helper.Helper
}

// cancelerSlot stops a download in the helper, from any goroutine.
type cancelerSlot struct {
	mu       sync.Mutex
	canceler *helper.Canceler
}

func (c *cancelerSlot) set(canceler *helper.Canceler) {
	c.mu.Lock()
	c.canceler = canceler
	c.mu.Unlock()
}

// helperLink is the request goroutine's side of the privileged helper.
type helperLink struct {
	slot     *helperSlot
	canceler *cancelerSlot
	// lost reports a helper that ended.
	lost func(err error)
	// allowDuo is what the helper should be told about Duo flashing, and
	// helperDuo what it was last told (nil: nothing yet).
	allowDuo  bool
	helperDuo *bool
	selection installflow.Selection
}

// run brings the helper up to date, then has it handle request, if any (nil
// for none; the reply is then the zero Reply).
func (l *helperLink) run(request *installer.Request, report func(installer.Progress)) (installer.Reply, error) {
	l.slot.mu.Lock()
	defer l.slot.mu.Unlock()
	h := l.slot.helper
	if h == nil {
		return installer.Reply{}, errors.New("Grant administrator access first.")
	}
	var reply installer.Reply
	err := l.sync(h, report)
	if err == nil && request != nil {
		reply, err = h.Handle(*request, report)
	}
	if err != nil && !h.Alive() {
		l.slot.helper = nil
		h.Close()
		l.canceler.set(nil)
		l.helperDuo = nil
		l.selection.HelperCleared()
		l.lost(errors.New("Administrator access ended. Grant it again to continue."))
	}
	return reply, err
}

func (l *helperLink) sync(h *helper.Helper, report func(installer.Progress)) error {
	if l.helperDuo == nil || *l.helperDuo != l.allowDuo {
		if _, err := h.Handle(installflow.AllowDuoRequest(l.allowDuo), func(installer.Progress) {}); err != nil {
			return err
		}
		allow := l.allowDuo
		l.helperDuo = &allow
	}
	if request := l.selection.Pending(); request != nil {
		l.selection.HelperCleared()
		if _, err := h.Handle(*request, report); err != nil {
			return err
		}
		l.selection.HelperAnswered()
	}
	return nil
}

// installerRunner runs the engine on its own goroutine. Its
// results reach the UI thread through gk.Post into done and access, which
// only the UI thread touches; poll drains them.
type installerRunner struct {
	mu     sync.Mutex
	jobs   []installer.Request
	closed bool
	wake   chan struct{}

	progressMu sync.Mutex
	progress   *installer.Progress
	// cancel stops a download in progress, in this process or in the helper.
	cancel *atomic.Bool
	helper *cancelerSlot
	// privileged is the helper, once access is granted.
	privileged *helperSlot
	work       string

	// UI thread only.
	done []installerOutcome
	// access is the outcome of each grant attempt, and a lost helper (nil:
	// granted).
	access []error
}

// startInstallerRunner starts the runner's goroutine.
func startInstallerRunner(work string) *installerRunner {
	r := &installerRunner{
		wake:       make(chan struct{}, 1),
		cancel:     new(atomic.Bool),
		helper:     &cancelerSlot{},
		privileged: &helperSlot{},
		work:       work,
	}
	go r.serve()
	return r
}

func (r *installerRunner) serve() {
	report := func(p installer.Progress) {
		r.progressMu.Lock()
		r.progress = &p
		r.progressMu.Unlock()
	}
	engine := installerEngine(r.work)
	engine.Progress = report
	engine.Cancel = r.cancel
	separate := helper.Needed()
	link := &helperLink{
		slot:     r.privileged,
		canceler: r.helper,
		lost:     r.reportAccess,
	}
	defer r.closeHelper()
	for {
		request, ok := r.next()
		if !ok {
			return
		}
		r.cancel.Store(false)
		report(installer.Progress{})
		if allow, ok := installflow.AllowDuoOf(request); ok {
			link.allowDuo = allow
		}
		if installflow.IsStockPrepare(request) {
			link.selection.Clear()
		}
		var reply installer.Reply
		var err error
		if separate && installflow.Privileged(request) {
			reply, err = link.run(&request, report)
		} else {
			reply, err = engine.Handle(request)
			if installflow.IsSelection(request) {
				link.selection.EngineAnswered(request, err == nil)
			}
			// The helper fetches and verifies its own copy now, in the same
			// cancelable step, before the mixer is touched.
			if separate && installflow.IsSelection(request) && err == nil {
				if r.cancel.Load() {
					reply, err = installer.Reply{}, installer.Error(installer.Canceled)
				} else if _, lerr := link.run(nil, report); lerr != nil {
					reply, err = installer.Reply{}, lerr
				}
			}
		}
		if !r.deliver(installerOutcome{request: request, reply: reply, err: err}) {
			return
		}
	}
}

// next waits for the next request; false once the runner is closed.
func (r *installerRunner) next() (installer.Request, bool) {
	for {
		r.mu.Lock()
		if r.closed {
			r.mu.Unlock()
			return installer.Request{}, false
		}
		if len(r.jobs) > 0 {
			request := r.jobs[0]
			r.jobs = r.jobs[1:]
			r.mu.Unlock()
			return request, true
		}
		r.mu.Unlock()
		<-r.wake
	}
}

// deliver hands an outcome to the UI thread; false once the page is closed,
// which ends the goroutine.
func (r *installerRunner) deliver(o installerOutcome) bool {
	r.mu.Lock()
	closed := r.closed
	r.mu.Unlock()
	if closed {
		return false
	}
	gk.Post(func(cx *gk.Cx) { r.done = append(r.done, o) })
	return true
}

// reportAccess hands a grant outcome or a lost helper to the UI thread.
func (r *installerRunner) reportAccess(err error) {
	gk.Post(func(cx *gk.Cx) { r.access = append(r.access, err) })
}

// closeHelper ends a helper still held when the goroutine stops.
func (r *installerRunner) closeHelper() {
	r.privileged.mu.Lock()
	h := r.privileged.helper
	r.privileged.helper = nil
	r.privileged.mu.Unlock()
	r.helper.set(nil)
	if h != nil {
		h.Close()
	}
}

// send queues request; false when the goroutine is gone.
func (r *installerRunner) send(request installer.Request) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return false
	}
	r.jobs = append(r.jobs, request)
	select {
	case r.wake <- struct{}{}:
	default:
	}
	return true
}

// stopDownload stops a download in progress, here or in
// the helper.
func (r *installerRunner) stopDownload() {
	r.cancel.Store(true)
	r.helper.mu.Lock()
	c := r.helper.canceler
	r.helper.mu.Unlock()
	if c != nil {
		c.Cancel()
	}
}

// close runs when the page closes: it ends a release check rather than
// leaving it, and any helper, running until the network gives up.
func (r *installerRunner) close() {
	r.stopDownload()
	r.mu.Lock()
	r.closed = true
	r.jobs = nil
	r.mu.Unlock()
	select {
	case r.wake <- struct{}{}:
	default:
	}
}

// grant starts the helper, which opens the system's password prompt, and
// checks that it answers before handing it to the request goroutine.
func (r *installerRunner) grant() {
	go func() {
		h, err := startInstallerHelper(r.work)
		if err == nil {
			if _, err = h.Handle(installflow.ProbeRequest(), func(installer.Progress) {}); err != nil {
				h.Close()
			}
		}
		if err == nil {
			r.mu.Lock()
			closed := r.closed
			r.mu.Unlock()
			if closed {
				// The page went away while the prompt was open.
				h.Close()
				return
			}
			c := h.Canceler()
			r.helper.set(&c)
			r.privileged.mu.Lock()
			r.privileged.helper = h
			r.privileged.mu.Unlock()
		}
		r.reportAccess(err)
	}()
}

func (r *installerRunner) takeAccess() []error {
	out := r.access
	r.access = nil
	return out
}

func (r *installerRunner) takeOutcomes() []installerOutcome {
	out := r.done
	r.done = nil
	return out
}

// currentProgress is the last progress reported, nil before any.
func (r *installerRunner) currentProgress() *installer.Progress {
	r.progressMu.Lock()
	defer r.progressMu.Unlock()
	if r.progress == nil || r.progress.Phase == "" {
		return nil
	}
	p := *r.progress
	return &p
}
