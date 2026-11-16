package mixer

// `mixer xruns [SECONDS]`: what the JACK error recorder saw while you
// watched.
//
// The recorder (`crates/rc2fx-native/jackerr`) is preloaded into the mixer's
// jackd at every RC2FX start and keeps a ring of every `jack_error` call in
// RAM. This command checks that it is the live server's, takes a snapshot of
// the ring with the bundle's `rc2fx_jackerr_snapshot`, waits SECONDS, takes
// another, and prints the records that arrived in between. `0` prints
// everything the ring still holds for this jackd instead. Nothing on the
// mixer is changed; the snapshots live in a private /run directory that is
// removed afterwards. It runs a program there, so it arms power recovery.

import (
	"archive/tar"
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"os"
	"sort"
	"strconv"
	"strings"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// DefaultSeconds is how long `mixer xruns` watches without an argument.
const DefaultSeconds = 60

const maxSeconds = 3600

func xruns(mixer *Mixer, args []string) error {
	const usage = "usage: mixer xruns [SECONDS] (0: everything recorded so far)"
	var seconds uint32
	switch len(args) {
	case 0:
		seconds = DefaultSeconds
	case 1:
		value, ok := task.ParseUint(args[0], 32)
		if !ok {
			return task.Error(usage)
		}
		seconds = uint32(value)
	default:
		return task.Error(usage)
	}
	if seconds > maxSeconds {
		return task.Errorf("SECONDS must be 0–%d", maxSeconds)
	}
	armed, err := mixer.ArmPowerRecovery("mixer xruns")
	if err != nil {
		return err
	}
	if seconds > 0 {
		fmt.Fprintf(os.Stderr, "Watching the JACK recorder for %d s…\n", seconds)
	}
	archive, err := armed.captureScript(remoteScript(seconds))
	if err != nil {
		return err
	}
	snapshots := map[string][]byte{}
	reader := tar.NewReader(bytes.NewReader(archive))
	for {
		header, err := reader.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			return task.Error(err.Error())
		}
		data, err := io.ReadAll(reader)
		if err != nil {
			return task.Error(err.Error())
		}
		snapshots[header.Name] = data
	}
	afterData, ok := snapshots["after"]
	if !ok {
		return task.Error("the mixer sent no snapshot")
	}
	after, err := ParseSnapshot(afterData)
	if err != nil {
		return err
	}
	var before *Snapshot
	if data, ok := snapshots["before"]; ok {
		before, err = ParseSnapshot(data)
		if err != nil {
			return err
		}
	}
	fmt.Print(Report(before, after, seconds))
	return nil
}

// remoteScript runs on the mixer. It writes a tar of `before` (unless
// SECONDS is 0) and `after` to stdout; everything else goes to stderr.
func remoteScript(seconds uint32) string {
	s := strconv.FormatUint(uint64(seconds), 10)
	return `set -e
root=/run/rc2fx-recorder
started_of() { local record rest; record=$(cat "/proc/$1/stat") || return 1; rest=${record##*) }; set -- $rest; printf '%s\n' "${20}"; }
if ! read -r attempt pid started < "$root/active" 2>/dev/null; then
  echo "The JACK recorder is not active: $(head -n 1 "$root/status" 2>/dev/null || echo 'RC2FX has not started it since boot')" >&2
  exit 3
fi
if test "$(started_of "$pid" 2>/dev/null)" != "$started"; then
  echo "The JACK recorder's jackd ($pid) is no longer running; the next RC2FX start records again" >&2
  exit 3
fi
ring=$(cat "$attempt/jackerr-dir")
snapshot=
for d in /run/rc2fx-dev /opt/rc2fx; do
  if test -x "$d/rc2fx_jackerr_snapshot"; then snapshot=$d/rc2fx_jackerr_snapshot; break; fi
done
test -n "$snapshot" || { echo 'No RC2FX bundle on this mixer has rc2fx_jackerr_snapshot' >&2; exit 3; }
out=$(mktemp -d /run/rc2fx-xruns.XXXXXX)
trap 'rm -rf "$out"' EXIT
echo "JACK recorder: jackd $pid, ring $ring" >&2
if test ` + s + ` -gt 0; then
  "$snapshot" "$ring" "$out/before" >&2
  sleep ` + s + `
  if test "$(started_of "$pid" 2>/dev/null)" != "$started"; then
    echo "jackd ($pid) restarted while watching; its records up to then are in the snapshot below" >&2
  fi
fi
"$snapshot" "$ring" "$out/after" >&2
cd "$out"
if test ` + s + ` -gt 0; then tar -cf - before after; else tar -cf - after; fi
`
}

const (
	snapshotMagic = "RC2FXJS1"
	headerSize    = 128
	recordSize    = 96
)

// Snapshot is a snapshot written by `rc2fx_jackerr_snapshot` (its header
// comment has the layout; `jackerr/tests/jackerr_ring.py` is the reference
// decoder).
type Snapshot struct {
	PID       uint64
	CreatedNs uint64
	Dropped   uint64
	// SnapshotNs is 0 when the mixer could not read its clock.
	SnapshotNs  uint64
	FirstIndex  uint64
	EndIndex    uint64
	Unpublished uint64
	Overwritten uint64
	Torn        uint64
	Records     []Record
}

// Record is one jack_error call.
type Record struct {
	Index uint64
	// TNs is CLOCK_MONOTONIC on the mixer; nil when the hook could not
	// read it.
	TNs     *uint64
	Kind    Kind
	Value1  int64
	Value2  int64
	String1 *string
	String2 *string
}

// Kind is what a record reports: jackd's format strings, by code.
type Kind struct {
	code uint16
}

// The kinds the recorder classifies; any other code is Other.
var (
	KindClientNotFinished   = Kind{1}
	KindClientFinishedLate  = Kind{2}
	KindTimedDriverXrun     = Kind{3}
	KindAlsaXrun            = Kind{4}
	KindAlsaWaitStatus      = Kind{5}
	KindInternalXrun        = Kind{6}
	KindPollFailed          = Kind{7}
	KindPollTimeoutExiting  = Kind{8}
	KindPollTimeoutRetrying = Kind{9}
	KindPollRecoveryFailed  = Kind{10}
	KindPollInterrupt       = Kind{11}
	KindUnknownFormat       = Kind{255}
)

func kindFromCode(code uint16) Kind { return Kind{code} }

// Name is the kind's name in reports.
func (k Kind) Name() string {
	switch k {
	case KindClientNotFinished:
		return "clientNotFinished"
	case KindClientFinishedLate:
		return "clientFinishedLate"
	case KindTimedDriverXrun:
		return "timedDriverXrun"
	case KindAlsaXrun:
		return "alsaXrun"
	case KindAlsaWaitStatus:
		return "alsaWaitStatus"
	case KindInternalXrun:
		return "internalXrun"
	case KindPollFailed:
		return "pollFailed"
	case KindPollTimeoutExiting:
		return "pollTimeoutExiting"
	case KindPollTimeoutRetrying:
		return "pollTimeoutRetrying"
	case KindPollRecoveryFailed:
		return "pollRecoveryFailed"
	case KindPollInterrupt:
		return "pollInterrupt"
	case KindUnknownFormat:
		return "unknownFormat"
	}
	return "kind" + strconv.Itoa(int(k.code))
}

// isXrun: the driver's own xrun reports, as opposed to a late client.
func (k Kind) isXrun() bool {
	return k == KindTimedDriverXrun || k == KindAlsaXrun || k == KindInternalXrun
}

// fields is what its two numbers and two strings mean (jackd's format
// strings).
func (k Kind) fields() ([]string, []string) {
	switch k {
	case KindClientNotFinished:
		return nil, []string{"client", "state"}
	case KindClientFinishedLate:
		return nil, []string{"client"}
	case KindTimedDriverXrun, KindAlsaXrun:
		return []string{"usecs"}, nil
	case KindAlsaWaitStatus, KindPollRecoveryFailed:
		return []string{"status"}, nil
	case KindInternalXrun:
		return []string{"frameOffset"}, nil
	case KindPollTimeoutExiting, KindPollTimeoutRetrying:
		return []string{"usecs", "retries"}, nil
	case KindPollFailed:
		return nil, []string{"reason"}
	case KindUnknownFormat:
		return nil, []string{"format", "formatContinued"}
	}
	return nil, nil
}

const (
	string1Truncated = 1
	string2Truncated = 2
	string1Null      = 4
	string2Null      = 8
	clockFailed      = 16
)

// ParseSnapshot decodes a snapshot.
func ParseSnapshot(data []byte) (*Snapshot, error) {
	if len(data) < headerSize || string(data[:8]) != snapshotMagic {
		return nil, task.Error("not an RC2FX jackd error ring snapshot")
	}
	u32 := func(at int) uint32 { return binary.LittleEndian.Uint32(data[at:]) }
	version, size := u32(8), u32(12)
	if version != 1 || size != recordSize {
		return nil, task.Errorf("unsupported snapshot version %d / record size %d", version, size)
	}
	// pid, created, claimed, dropped, unknown, calls, snapshot time,
	// first index, end index, stable, unpublished, overwritten, torn.
	field := func(n int) uint64 { return binary.LittleEndian.Uint64(data[16+8*n:]) }
	s := &Snapshot{
		PID: field(0), CreatedNs: field(1), Dropped: field(3),
		SnapshotNs: field(6), FirstIndex: field(7), EndIndex: field(8),
		Unpublished: field(10), Overwritten: field(11), Torn: field(12),
	}
	stable := field(9)
	if stable > uint64(len(data)) || uint64(len(data)) != headerSize+stable*recordSize {
		return nil, task.Error("snapshot length does not match its record count")
	}
	var accounted uint64
	ok := true
	for _, n := range []uint64{stable, s.Unpublished, s.Overwritten, s.Torn} {
		if accounted+n < accounted {
			ok = false
			break
		}
		accounted += n
	}
	if s.EndIndex < s.FirstIndex || !ok || s.EndIndex-s.FirstIndex != accounted {
		return nil, task.Error("snapshot accounting does not add up")
	}
	for at := headerSize; at+recordSize <= len(data); at += recordSize {
		s.Records = append(s.Records, record(data[at:at+recordSize]))
	}
	return s, nil
}

func record(bytes []byte) Record {
	u64 := func(at int) uint64 { return binary.LittleEndian.Uint64(bytes[at:]) }
	kindFlags := u64(16)
	flags := (kindFlags >> 16) & 0xffff
	lengths := [2]int{int((kindFlags >> 32) & 0xff), int((kindFlags >> 40) & 0xff)}
	text := func(raw []byte, length int, null uint64) *string {
		if flags&null != 0 {
			return nil
		}
		s := strings.ToValidUTF8(string(raw[:min(length, len(raw))]), "�")
		return &s
	}
	r := Record{
		Index:   u64(0),
		Kind:    kindFromCode(uint16(kindFlags & 0xffff)),
		Value1:  int64(u64(24)),
		Value2:  int64(u64(32)),
		String1: text(bytes[40:80], lengths[0], string1Null),
		String2: text(bytes[80:96], lengths[1], string2Null),
	}
	if flags&clockFailed == 0 {
		t := u64(8)
		r.TNs = &t
	}
	for _, s := range []struct {
		text *string
		bit  uint64
	}{{r.String1, string1Truncated}, {r.String2, string2Truncated}} {
		if flags&s.bit != 0 && s.text != nil {
			*s.text += "…"
		}
	}
	return r
}

func (r Record) describe() string {
	values, strs := r.Kind.fields()
	parts := []string{r.Kind.Name()}
	for i, name := range values {
		value := []int64{r.Value1, r.Value2}[i]
		parts = append(parts, fmt.Sprintf("%s=%d", name, value))
	}
	for i, name := range strs {
		value := []*string{r.String1, r.String2}[i]
		text := "(null)"
		if value != nil {
			text = *value
		}
		parts = append(parts, name+"="+text)
	}
	return strings.Join(parts, "  ")
}

func (r Record) client() *string {
	if r.Kind == KindClientNotFinished || r.Kind == KindClientFinishedLate {
		return r.String1
	}
	return nil
}

// Report is the records of after that arrived while watching, and how
// complete the ring's view of that window is.
func Report(before, after *Snapshot, seconds uint32) string {
	sameServer := before == nil || (before.PID == after.PID && before.CreatedNs == after.CreatedNs)
	from := after.FirstIndex
	if before != nil && sameServer {
		from = before.EndIndex
	}
	var origin *uint64
	if before != nil && sameServer && before.SnapshotNs != 0 {
		origin = &before.SnapshotNs
	}
	var records []Record
	for _, r := range after.Records {
		if r.Index >= from {
			records = append(records, r)
		}
	}
	var out strings.Builder
	if before != nil && !sameServer {
		out.WriteString("jackd was replaced while watching: showing everything the new server recorded\n")
	}
	xrunCount, late := 0, 0
	for _, r := range records {
		if r.Kind.isXrun() {
			xrunCount++
		}
		if r.client() != nil {
			late++
		}
	}
	if before != nil {
		fmt.Fprintf(&out, "Watched %d s: ", seconds)
	} else {
		out.WriteString("Recorded since this jackd started: ")
	}
	fmt.Fprintf(&out, "%d event(s): %d driver xrun(s), %d late client(s)\n", len(records), xrunCount, late)
	for _, r := range records {
		var at string
		switch {
		case r.TNs != nil && origin != nil:
			at = fmt.Sprintf("%+10.3f s", (float64(*r.TNs)-float64(*origin))/1e9)
		case r.TNs != nil:
			at = fmt.Sprintf("%10.3f s", float64(*r.TNs)/1e9)
		default:
			at = fmt.Sprintf("%12s", "(no clock)")
		}
		fmt.Fprintf(&out, "  %s  %s\n", at, r.describe())
	}
	if len(records) > 0 {
		kinds := map[string]int{}
		clients := map[string]int{}
		for _, r := range records {
			kinds[r.Kind.Name()]++
			if c := r.client(); c != nil {
				clients[*c]++
			}
		}
		list := func(m map[string]int) string {
			names := make([]string, 0, len(m))
			for name := range m {
				names = append(names, name)
			}
			sort.Strings(names)
			parts := make([]string, len(names))
			for i, name := range names {
				parts[i] = fmt.Sprintf("%s %d", name, m[name])
			}
			return strings.Join(parts, ", ")
		}
		fmt.Fprintf(&out, "By kind: %s\n", list(kinds))
		if len(clients) > 0 {
			fmt.Fprintf(&out, "Late clients: %s\n", list(clients))
		}
	}
	// Records of the window the ring could not show.
	saturating := func(a, b uint64) uint64 {
		if a < b {
			return 0
		}
		return a - b
	}
	missed := saturating(after.FirstIndex, from)
	dropped := after.Dropped
	if before != nil && sameServer {
		dropped = saturating(after.Dropped, before.Dropped)
	}
	unreadable := after.Unpublished + after.Torn
	if missed+dropped+unreadable > 0 {
		fmt.Fprintf(&out, "Incomplete: %d overwritten before the snapshot, %d dropped by the recorder, "+
			"%d unreadable at snapshot time\n", missed, dropped, unreadable)
	}
	return out.String()
}
