package app

import (
	"errors"
	"sync"
	"sync/atomic"

	"github.com/ibanks42/goui-kit/gk"

	"github.com/ibanks42/rc2fx/desktop/app/installflow"
	"github.com/ibanks42/rc2fx/desktop/installer"
	"github.com/ibanks42/rc2fx/desktop/installer/helper"
)

type installerOutcome struct {
	request installer.Request
	reply   installer.Reply
	err     error
}

// helperSlot shares the privileged helper between the grant and worker goroutines.
type helperSlot struct {
	mu     sync.Mutex
	helper *helper.Helper
}

type cancelerSlot struct {
	mu       sync.Mutex
	canceler *helper.Canceler
}

func (c *cancelerSlot) set(canceler *helper.Canceler) {
	c.mu.Lock()
	c.canceler = canceler
	c.mu.Unlock()
}

type helperLink struct {
	slot      *helperSlot
	canceler  *cancelerSlot
	lost      func(err error)
	allowDuo  bool
	helperDuo *bool
	selection installflow.Selection
}

// run syncs state to the privileged helper and optionally executes request.
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

// installerRunner executes installer requests on a background goroutine and
// posts results to the UI thread via gk.Post for pollInstaller to drain.
type installerRunner struct {
	mu     sync.Mutex
	jobs   []installer.Request
	closed bool
	wake   chan struct{}

	progressMu sync.Mutex
	progress   *installer.Progress
	cancel     *atomic.Bool
	helper     *cancelerSlot
	privileged *helperSlot
	work       string

	// UI thread only.
	done   []installerOutcome
	access []error
}

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
			// Have the helper fetch and verify its own copy during this cancelable
			// step, before the mixer is touched.
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

func (r *installerRunner) reportAccess(err error) {
	gk.Post(func(cx *gk.Cx) { r.access = append(r.access, err) })
}

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

func (r *installerRunner) stopDownload() {
	r.cancel.Store(true)
	r.helper.mu.Lock()
	c := r.helper.canceler
	r.helper.mu.Unlock()
	if c != nil {
		c.Cancel()
	}
}

// close cancels any active download and signals the worker goroutine to exit.
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

// grant spawns the privileged helper (triggering the OS auth prompt) and
// verifies it responds before attaching it to the worker.
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

func (r *installerRunner) currentProgress() *installer.Progress {
	r.progressMu.Lock()
	defer r.progressMu.Unlock()
	if r.progress == nil || r.progress.Phase == "" {
		return nil
	}
	p := *r.progress
	return &p
}
