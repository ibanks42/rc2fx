// Package report is what the command-line tool prints with --json or writes
// with --json-out, and what the window reads. One Report per run.
package report

// Verdict is what a check found.
type Verdict string

const (
	// NeedsFix: RØDE's factory layout, partition 1 under 512 MiB.
	NeedsFix Verdict = "needs-fix"
	// Fixed: partition 1 already has the grown size.
	Fixed Verdict = "fixed"
	// Damaged: sector 0 is not a table this tool knows, but the rest of the
	// disk shows it is a RØDECaster with the standard partition starts, so
	// `recover` can rebuild the table (Rebuild is what it would write).
	Damaged Verdict = "damaged"
	// Unrecognized: not a layout this tool knows; it is never written.
	Unrecognized Verdict = "unrecognized"
)

// Partition is one entry of the table, in 512-byte sectors.
type Partition struct {
	Number  int    `json:"number"`
	Type    byte   `json:"type"`
	Start   uint32 `json:"start"`
	Sectors uint32 `json:"sectors"`
}

// Evidence is one sign, looked for on a disk whose sector 0 is not a known
// table, that the disk is a RØDECaster with the standard layout.
type Evidence struct {
	// What is a short description for people ("ext4 filesystem at the
	// start of partition 1").
	What string `json:"what"`
	// Sector is where it was looked for.
	Sector uint64 `json:"sector"`
	Found  bool   `json:"found"`
}

// Report is the result of `check`, `fix`, `restore` or `recover`.
type Report struct {
	// Command is "check", "fix", "restore" or "recover".
	Command string `json:"command"`
	// OK is false when the command failed; Error then always says why, in
	// words for the person. Code is a stable word for programs: "no-mixer",
	// "several-mixers", "permission", "unsupported", "unrecognized",
	// "damaged" (fix or restore on a damaged table: `recover` is the
	// command for it), "not-a-mixer" (sector 0 is not a known table and the
	// disk does not show the evidence of a RØDECaster; Evidence says what
	// was missing), "changed" (the disk is not what --expect named),
	// "not-empty" (the space partition 1 would grow into holds data),
	// "read-failed" (reading the mixer's storage failed), "write-failed",
	// "verify-failed", "bad-backup", "internal".
	OK    bool   `json:"ok"`
	Code  string `json:"code,omitempty"`
	Error string `json:"error,omitempty"`

	Disk    string `json:"disk,omitempty"`
	Sectors uint64 `json:"sectors,omitempty"`

	// Verdict and the table as read at the start of the command.
	Verdict    Verdict     `json:"verdict,omitempty"`
	Reason     string      `json:"reason,omitempty"` // why Unrecognized or Damaged
	Partitions []Partition `json:"partitions,omitempty"`
	// Sector0 is the SHA-256 (hex) of sector 0 as read at the start; `fix
	// --expect` and `recover --expect` take it.
	Sector0 string `json:"sector0,omitempty"`

	// Evidence is what was looked for when sector 0 was not a known table.
	Evidence []Evidence `json:"evidence,omitempty"`
	// Rebuild is the table `recover` would write, when Damaged.
	Rebuild []Partition `json:"rebuild,omitempty"`
	// P1Filesystem is the size, in sectors, of the ext4 filesystem on
	// partition 1, when it was read.
	P1Filesystem uint64 `json:"p1filesystem,omitempty"`

	// After a fix, restore or recover that wrote: the table as read back,
	// where the copy of the old sector 0 was saved, and whether the mixer's
	// storage was changed at all (false when there was nothing to do).
	Changed bool        `json:"changed"`
	After   []Partition `json:"after,omitempty"`
	Backup  string      `json:"backup,omitempty"`
	// Source is the file `recover --backup` wrote, when it did.
	Source string `json:"source,omitempty"`
}
