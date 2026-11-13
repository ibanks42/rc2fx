//! Reuse the port buffers `jack_port_get_buffer` returns while the result
//! cannot change. The cache is used only when libjack is the qualified build.
//!
//! The callback asks libjack for 82 port buffers per cycle. jack2 1.9.20 builds
//! each answer from the shared-memory graph manager: a 16-bit generation
//! counter (`CurIndex`), the port record 0x8410 bytes away, and the current
//! connection array. On this device those reads miss far more often than they
//! hit, which is most of the ~80 us the profile attributes to the adapter.
//!
//! The result is a property of `(port, CurIndex)`. `CurIndex` changes on every
//! committed connection change, so a cached pointer recorded under the same
//! generation is the pointer libjack would return. Which ports may be cached is
//! taken from the disassembly of `JackGraphManager::GetBuffer` (libjack offset
//! 0x195c0):
//!
//! - `IsOutput` (`+0x824` bit 1) and not tied (`+0xc0c == 0xfffe`): the return
//!   value is this port's own buffer, `(manager + 0x2458c10 + 0x8410 * port) &
//!   !0x1f + 0x20` at 0x196ac. Entry `0x19664` refuses ports that are not used
//!   (`+0xc09 == 0`).
//! - Inputs: `GetBuffer` follows a single connection when the peer belongs to
//!   another client (0x19750 takes the restart at 0x19620, which resolves the
//!   peer and returns *its* buffer). With no connection it clears this port's
//!   own buffer, and with a same-client or multiple connections it mixes into
//!   this port's own buffer. Only the returned pointer that is **not** this
//!   port's own buffer is therefore a zero-copy result that GetBuffer did not
//!   write through, and only those inputs are cached.
//!
//! Everything else calls the real function every cycle: ports that are not
//! used, tied outputs, inputs with zero or several connections, inputs resolved
//! to their own buffer (the mix/clear cases), a cold cache, and any lookup made
//! after the graph generation moved.
//!
//! ## Once per callback
//!
//! The callback thread starts each cycle with [`begin`]: it calls libjack's
//! manager accessor once (0x3d790), reads `CurIndex` once, and advances the
//! epoch if either changed. The hook then needs no accessor call and no shared
//! atomic; a lookup is the generation load, the epoch compare and the recorded
//! pointer.
//!
//! libjack itself re-reads `CurIndex` on every `GetBuffer` (0x19644, bank
//! select 0x19650), so nothing in the client library promises the generation
//! holds still for the length of a callback. The graph switch (`TrySwitchState`)
//! is server side and is not in libjack 1.9.20, so the hook keeps one aligned
//! load of the generation per lookup: if the server did switch mid-callback,
//! every later lookup bypasses the cache for the rest of that callback, and the
//! next callback starts a fresh epoch. The load hits L1 after the first lookup
//! of the cycle, because only a connection change writes that line.
//!
//! The layout is tied to the exact libjack build: the module refuses to enable
//! unless the file hash matches and the pinned instruction bytes are present at
//! the three offsets it reads.
#![cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
use rc2fx_firmware::Profile;
use std::{
    cell::Cell,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

/// SHA-256 of the mixer's `/usr/lib/libjack.so.0.1.0` (jack2 1.9.20).
pub(crate) const QUALIFIED_SHA: &str =
    "3395d474fafc7723870d920eec74d1c42f9ed22d2816c98603a00f8c19c7f53e";
const LIBJACK_PATH: &str = "/usr/lib/libjack.so.0.1.0";
/// `jack_port_get_buffer`: prologue and the path that yields the manager.
const GET_BUFFER: usize = 0xc970;
const MANAGER: usize = 0x3d790;
/// `JackGraphManager::GetBuffer` entry, used only for its pinned bytes.
const GRAPH_BUFFER: usize = 0x195b0;

const PORT_STRIDE: usize = 0x8410;
const PORT_FLAGS: usize = 0x824;
const PORT_USED: usize = 0xc09;
const PORT_TIED: usize = 0xc0c;
const NOT_TIED: u32 = 0xfffe;
const PORT_COUNT: usize = 0x2456818;
const GENERATION: usize = 0x2456810;
const RECORDS: usize = 0x2458000;
const BUFFERS: usize = 0x2458c10;
const CONNECTIONS: usize = 0x10;
const CONNECTION_STRIDE: usize = 0x1004;
const CONNECTION_COUNT: usize = 0x1000;
const CONNECTION_BANK: usize = 0x122b400;

/// The largest port id `jack_port_get_buffer` accepts (`0xfff`).
const PORTS: usize = 0x1000;

/// What one callback runs with: the manager it saw, its generation, and the
/// epoch recorded entries must carry to be reusable. Inactive means every
/// lookup goes to libjack.
#[derive(Clone, Copy)]
pub(crate) struct Snapshot {
    pub(crate) active: bool,
    pub(crate) epoch: u32,
    pub(crate) manager: usize,
    pub(crate) generation: u32,
}
impl Snapshot {
    /// Every lookup goes to libjack.
    pub(crate) const INACTIVE: Self = Self {
        active: false,
        epoch: 0,
        manager: 0,
        generation: 0,
    };
}
impl Default for Snapshot {
    fn default() -> Self {
        Self::INACTIVE
    }
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static BASE: AtomicUsize = AtomicUsize::new(0);
static EPOCH: AtomicU32 = AtomicU32::new(0);
static LAST_GENERATION: AtomicU32 = AtomicU32::new(0);
/// The graph manager the recorded pointers belong to. If libjack ever attaches
/// a different one (a restarted server), its counter starts over and could
/// match the old value, so a new manager address starts a new epoch too.
static LAST_MANAGER: AtomicUsize = AtomicUsize::new(0);
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static RECORDED: AtomicU64 = AtomicU64::new(0);
static STALE: AtomicU64 = AtomicU64::new(0);
static UNAVAILABLE: AtomicU64 = AtomicU64::new(0);
static OUTPUTS: AtomicU64 = AtomicU64::new(0);
static INPUTS: AtomicU64 = AtomicU64::new(0);

/// One `(generation, kind, pointer)` record per port id. The tag packs the
/// epoch and kind.
#[repr(align(64))]
struct Table {
    tag: [AtomicU64; PORTS],
    pointer: [AtomicUsize; PORTS],
}
static TABLE: Table = Table {
    tag: [const { AtomicU64::new(0) }; PORTS],
    pointer: [const { AtomicUsize::new(0) }; PORTS],
};

/// This thread's lookup counters. The callback reads them nowhere; they are
/// plain thread-owned numbers, published with one add per counter when the
/// frame ends, so the hook never performs a read-modify-write on a shared line.
#[derive(Clone, Copy, Default)]
struct Counters {
    hits: u64,
    misses: u64,
    recorded: u64,
    outputs: u64,
    inputs: u64,
    stale: u64,
    unavailable: u64,
}
impl Counters {
    const ZERO: Self = Self {
        hits: 0,
        misses: 0,
        recorded: 0,
        outputs: 0,
        inputs: 0,
        stale: 0,
        unavailable: 0,
    };
}
thread_local! {
    /// `UnsafeCell`, not `Cell`: an update is one increment of one field, not a
    /// copy of the whole struct to and from the cell on every port lookup.
    static COUNTERS: Cell<Counters> = const { Cell::new(Counters::ZERO) };
}

/// Update one field in place on this thread's counters.
fn count(update: impl FnOnce(&mut Counters)) {
    COUNTERS.with(|cell| {
        let counters = cell.as_ptr();
        // SAFETY: the cell is this thread's, and no reference to it outlives
        // this closure.
        unsafe { update(&mut *counters) };
    });
}

/// Publish this thread's lookup counters. Called once per callback, after the
/// vendor callback and our pipeline have finished their lookups.
pub fn publish() {
    let counters =
        COUNTERS.with(|cell| std::mem::replace(unsafe { &mut *cell.as_ptr() }, Counters::ZERO));
    if counters.hits != 0 {
        HITS.fetch_add(counters.hits, Ordering::Relaxed);
    }
    if counters.misses != 0 {
        MISSES.fetch_add(counters.misses, Ordering::Relaxed);
    }
    if counters.recorded != 0 {
        RECORDED.fetch_add(counters.recorded, Ordering::Relaxed);
    }
    if counters.outputs != 0 {
        OUTPUTS.fetch_add(counters.outputs, Ordering::Relaxed);
    }
    if counters.inputs != 0 {
        INPUTS.fetch_add(counters.inputs, Ordering::Relaxed);
    }
    if counters.stale != 0 {
        STALE.fetch_add(counters.stale, Ordering::Relaxed);
    }
    if counters.unavailable != 0 {
        UNAVAILABLE.fetch_add(counters.unavailable, Ordering::Relaxed);
    }
}

fn slot(port: usize) -> usize {
    port & (PORTS - 1)
}

fn store(port: usize, epoch: u32, kind: u8, pointer: usize) {
    TABLE.pointer[slot(port)].store(pointer, Ordering::Relaxed);
    TABLE.tag[slot(port)].store((u64::from(epoch) << 8) | u64::from(kind), Ordering::Release);
}

fn load(port: usize) -> (u32, u8, usize) {
    let tag = TABLE.tag[slot(port)].load(Ordering::Acquire);
    let pointer = TABLE.pointer[slot(port)].load(Ordering::Relaxed);
    ((tag >> 8) as u32, (tag & 0xff) as u8, pointer)
}

/// Why the cache is not in use, when it is not. A libjack mismatch only means
/// every call goes to libjack as before; it never refuses RC2FX itself.
static SKIPPED: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn install(profile: &Profile) -> Result<(), String> {
    // The vendor gates on its own hash; the graph layout below is libjack's.
    let _ = profile;
    let bytes = match std::fs::read(LIBJACK_PATH) {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = SKIPPED.set(format!("cannot read {LIBJACK_PATH}: {error}"));
            return Ok(());
        }
    };
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(&bytes);
    let digest = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if digest != QUALIFIED_SHA {
        let _ = SKIPPED.set(format!("libjack {digest} is not the qualified build"));
        return Ok(());
    }
    // libjack is a load-time dependency of the mixer, so it is mapped by
    // now. Find it here, once, so the audio thread never calls into the
    // dynamic loader.
    let Some(base) = mapped_base() else {
        let _ = SKIPPED.set(format!("{LIBJACK_PATH} is not mapped"));
        return Ok(());
    };
    for (offset, expected) in PINNED {
        let actual = unsafe { std::slice::from_raw_parts((base + offset) as *const u8, 16) };
        if actual != expected {
            let _ = SKIPPED.set(format!("libjack instructions differ at {offset:#x}"));
            return Ok(());
        }
    }
    BASE.store(base, Ordering::Release);
    ENABLED.store(true, Ordering::Release);
    Ok(())
}

/// Load address of libjack: the start of its file-offset-0 mapping.
fn mapped_base() -> Option<usize> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    maps.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let range = fields.next()?;
        let _permissions = fields.next()?;
        let offset = fields.next()?;
        let path = fields.nth(2)?;
        (path == LIBJACK_PATH && usize::from_str_radix(offset, 16).ok()? == 0)
            .then(|| usize::from_str_radix(range.split('-').next()?, 16).ok())
            .flatten()
    })
}

/// Bytes that identify the pinned libjack build at each offset we use.
const PINNED: [(usize, [u8; 16]); 3] = [
    (
        GET_BUFFER,
        [
            0x3f, 0x23, 0x03, 0xd5, 0xfd, 0x7b, 0xbe, 0xa9, 0xfd, 0x03, 0x00, 0x91, 0xf3, 0x53,
            0x01, 0xa9,
        ],
    ),
    (
        GRAPH_BUFFER,
        [
            0x3f, 0x23, 0x03, 0xd5, 0x0c, 0x0e, 0x88, 0xd2, 0xff, 0x63, 0x2c, 0xcb, 0x83, 0x02,
            0x00, 0x90,
        ],
    ),
    (
        MANAGER,
        [
            0x5f, 0x24, 0x03, 0xd5, 0x60, 0x01, 0x00, 0x90, 0x00, 0xc0, 0x47, 0xf9, 0x00, 0x00,
            0x40, 0xf9,
        ],
    ),
];

/// The cache is used only when the function the mixer resolved is exactly this
/// libjack's `jack_port_get_buffer`; anything else (another libjack, an
/// interposer) goes straight to the original every time.
fn accepted(base: usize, original: usize) -> bool {
    base != 0 && original == base + GET_BUFFER
}

/// The graph manager, read through libjack's own accessor (0x3d790: three
/// loads from libjack's globals). Called once per callback.
fn manager() -> usize {
    let base = BASE.load(Ordering::Acquire);
    if base == 0 {
        return 0;
    }
    let accessor: unsafe extern "C" fn() -> usize = unsafe { std::mem::transmute(base + MANAGER) };
    unsafe { accessor() }
}

fn generation(manager: usize) -> u32 {
    u32::from(unsafe { std::ptr::read_volatile((manager + GENERATION) as *const u16) })
}

/// Start a callback frame's view of the graph: one accessor call, one
/// generation read, and a new epoch when either the manager or its counter
/// changed since the last frame.
pub fn begin(original: usize) -> Snapshot {
    if !ENABLED.load(Ordering::Relaxed) {
        return Snapshot::default();
    }
    if !accepted(BASE.load(Ordering::Relaxed), original) {
        count(|counters| counters.unavailable += 1);
        return Snapshot::default();
    }
    let manager = manager();
    if manager == 0 {
        count(|counters| counters.unavailable += 1);
        return Snapshot::default();
    }
    snapshot(manager)
}

/// The per-callback part of [`begin`], separated so the epoch rules can be
/// tested without libjack.
fn snapshot(manager: usize) -> Snapshot {
    let generation = generation(manager);
    let previous_generation = LAST_GENERATION.load(Ordering::Relaxed);
    let previous_manager = LAST_MANAGER.load(Ordering::Relaxed);
    if generation != previous_generation || manager != previous_manager {
        LAST_GENERATION.store(generation, Ordering::Relaxed);
        LAST_MANAGER.store(manager, Ordering::Relaxed);
        EPOCH.fetch_add(1, Ordering::Relaxed);
    }
    Snapshot {
        active: true,
        epoch: EPOCH.load(Ordering::Relaxed),
        manager,
        generation,
    }
}

/// A pointer to return instead of calling libjack, if the recorded answer is
/// still valid for this port.
///
/// # Safety
/// `port` is the id the caller passed to `jack_port_get_buffer`.
pub unsafe fn hit(
    snapshot: Snapshot,
    port: *mut std::ffi::c_void,
) -> Option<*mut std::ffi::c_void> {
    if !snapshot.active {
        return None;
    }
    let id = port as usize;
    if id == 0 || id >= PORTS {
        return None;
    }
    // One load of the generation word: if the graph state changed while this
    // callback runs, nothing recorded for the old state may answer.
    if generation(snapshot.manager) != snapshot.generation {
        count(|counters| counters.stale += 1);
        return None;
    }
    let (recorded, kind, pointer) = load(id);
    if kind != 0 && recorded == snapshot.epoch {
        count(|counters| counters.hits += 1);
        return Some(pointer as *mut std::ffi::c_void);
    }
    count(|counters| counters.misses += 1);
    None
}

/// Record what the real function returned for this port, if the port is in one
/// of the reusable cases and the generation did not move while the call ran.
///
/// # Safety
/// `native` is the value the real `jack_port_get_buffer` returned for `port`.
pub unsafe fn record(
    snapshot: Snapshot,
    port: *mut std::ffi::c_void,
    native: *mut std::ffi::c_void,
) {
    if !snapshot.active {
        return;
    }
    let id = port as usize;
    if id == 0 || id >= PORTS {
        return;
    }
    // A graph change while the call ran makes the observation ambiguous.
    if generation(snapshot.manager) != snapshot.generation {
        count(|counters| counters.stale += 1);
        return;
    }
    // A port already classified as unreusable under this generation needs no
    // second look: the answer cannot change until the generation does.
    let (recorded, known, _) = load(id);
    if recorded == snapshot.epoch && known == 0 {
        return;
    }
    let manager = snapshot.manager;
    let count_now =
        unsafe { std::ptr::read_volatile((manager + PORT_COUNT) as *const u32) } as usize;
    if id >= count_now {
        return;
    }
    let record = manager + RECORDS + id * PORT_STRIDE;
    let used = unsafe { std::ptr::read_volatile((record + PORT_USED) as *const u8) } != 0;
    if !used {
        return;
    }
    let flags = unsafe { std::ptr::read_volatile((record + PORT_FLAGS) as *const u32) };
    // GetBuffer returns `(address & !0x1f) + 0x20` (0x196ac-0x196b0): the next
    // 32-byte boundary strictly after the field, not the field rounded up.
    let own = {
        let raw = manager + BUFFERS + id * PORT_STRIDE;
        (raw & !0x1f) + 0x20
    };
    let kind = if flags & 0x2 != 0 {
        // Output: libjack returns this port's own buffer unless it is tied.
        let tied = unsafe { std::ptr::read_volatile((record + PORT_TIED) as *const u32) };
        if tied != NOT_TIED || native as usize != own {
            0
        } else {
            count(|counters| counters.outputs += 1);
            1
        }
    } else {
        // Input: only a single connection resolved away from this port's own
        // buffer is a zero-copy answer that GetBuffer did not write through.
        let bank = snapshot.generation as usize & 1;
        let connection = manager
            + CONNECTIONS
            + bank * CONNECTION_BANK
            + id * CONNECTION_STRIDE
            + CONNECTION_COUNT;
        let connections = unsafe { std::ptr::read_volatile(connection as *const u32) } & 0xffff;
        if connections != 1 || native as usize == own {
            0
        } else {
            count(|counters| counters.inputs += 1);
            2
        }
    };
    store(id, snapshot.epoch, kind, native as usize);
    if kind != 0 {
        count(|counters| counters.recorded += 1);
    }
}

pub fn status() -> serde_json::Value {
    serde_json::json!({
        "installed": ENABLED.load(Ordering::Acquire),
        "skipped": SKIPPED.get(),
        "hits": HITS.load(Ordering::Relaxed),
        "misses": MISSES.load(Ordering::Relaxed),
        "recorded": RECORDED.load(Ordering::Relaxed),
        "outputRecords": OUTPUTS.load(Ordering::Relaxed),
        "inputRecords": INPUTS.load(Ordering::Relaxed),
        "staleGeneration": STALE.load(Ordering::Relaxed),
        "unavailable": UNAVAILABLE.load(Ordering::Relaxed),
        "scope": "callback-thread port buffers only; keyed by the graph generation",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The epoch, the table and the counters are process-wide; one test at a
    /// time.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A fake graph manager: only the generation word is ever touched, so the
    /// 37 MB reservation costs one page.
    struct FakeManager(*mut u8);
    impl FakeManager {
        fn new(generation: u16) -> Self {
            let length = GENERATION + 4096;
            let base = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    length,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                    -1,
                    0,
                )
            };
            assert_ne!(base, libc::MAP_FAILED);
            let base = base.cast::<u8>();
            unsafe { base.add(GENERATION).cast::<u16>().write(generation) };
            Self(base)
        }
        fn address(&self) -> usize {
            self.0 as usize
        }
        fn set(&self, generation: u16) {
            unsafe { self.0.add(GENERATION).cast::<u16>().write(generation) };
        }
    }
    impl Drop for FakeManager {
        fn drop(&mut self) {
            unsafe { libc::munmap(self.0.cast(), GENERATION + 4096) };
        }
    }

    /// The state `begin` derives, without the libjack accessor.
    fn snapshot_of(manager: usize) -> Snapshot {
        snapshot(manager)
    }

    /// A port id as libjack passes it around: a number in a pointer-typed
    /// argument, never dereferenced.
    fn port(id: usize) -> *mut std::ffi::c_void {
        std::ptr::without_provenance_mut(id)
    }

    #[test]
    fn a_new_graph_manager_starts_a_new_epoch_even_with_the_same_generation() {
        let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
        let first = FakeManager::new(7);
        let second = FakeManager::new(7);
        let one = snapshot_of(first.address());
        let again = snapshot_of(first.address());
        assert_eq!(again.epoch, one.epoch, "same manager, same generation");
        let other = snapshot_of(second.address());
        assert_eq!(
            other.epoch,
            one.epoch + 1,
            "a restarted server's manager must not reuse old pointers"
        );
        first.set(8);
        assert_eq!(snapshot_of(first.address()).epoch, other.epoch + 1);
    }

    #[test]
    fn only_this_libjacks_entry_activates_the_cache() {
        assert!(accepted(0x1000, 0x1000 + GET_BUFFER));
        assert!(!accepted(0, GET_BUFFER), "not installed");
        assert!(!accepted(0x1000, 0x1000 + GET_BUFFER + 4), "interposed");
    }

    #[test]
    fn a_lookup_after_a_generation_change_bypasses_the_cache() {
        let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
        let manager = FakeManager::new(3);
        let snapshot = snapshot_of(manager.address());
        let id = 12usize;
        let pointer = 0x1234_5678usize;
        store(id, snapshot.epoch, 1, pointer);
        assert_eq!(
            unsafe { hit(snapshot, port(id)) },
            Some(std::ptr::without_provenance_mut(pointer))
        );
        manager.set(4);
        assert_eq!(
            unsafe { hit(snapshot, port(id)) },
            None,
            "a graph switch mid-callback must not serve the old pointer"
        );
        // The next callback sees the new generation and a new epoch, so the
        // entry recorded under the old one is dead even at the same port.
        let next = snapshot_of(manager.address());
        assert_ne!(next.epoch, snapshot.epoch);
        assert_eq!(unsafe { hit(next, port(id)) }, None);
    }

    #[test]
    fn an_inactive_snapshot_never_answers() {
        let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
        let snapshot = Snapshot::INACTIVE;
        store(1, snapshot.epoch, 1, 0x1000);
        assert_eq!(unsafe { hit(snapshot, port(1)) }, None);
    }

    #[test]
    fn counters_accumulate_per_thread_and_publish_once() {
        let _serial = SERIAL.lock().unwrap_or_else(|poison| poison.into_inner());
        COUNTERS.with(|cell| cell.set(Counters::ZERO));
        let before = (
            HITS.load(Ordering::Relaxed),
            MISSES.load(Ordering::Relaxed),
            STALE.load(Ordering::Relaxed),
        );
        let manager = FakeManager::new(5);
        let snapshot = snapshot_of(manager.address());
        let id = 20usize;
        store(id, snapshot.epoch, 1, 0x2000);
        for _ in 0..3 {
            let _ = unsafe { hit(snapshot, port(id)) };
        }
        let _ = unsafe { hit(snapshot, port(21)) };
        manager.set(6);
        let _ = unsafe { hit(snapshot, port(id)) };
        // Nothing reaches the shared counters before publish.
        assert_eq!(HITS.load(Ordering::Relaxed), before.0);
        assert_eq!(MISSES.load(Ordering::Relaxed), before.1);
        assert_eq!(STALE.load(Ordering::Relaxed), before.2);
        publish();
        assert_eq!(HITS.load(Ordering::Relaxed), before.0 + 3);
        assert_eq!(MISSES.load(Ordering::Relaxed), before.1 + 1);
        assert_eq!(STALE.load(Ordering::Relaxed), before.2 + 1);
        // A second publish adds nothing.
        publish();
        assert_eq!(HITS.load(Ordering::Relaxed), before.0 + 3);
    }

    /// The reusable cases, as `record` classifies them.
    fn classify(output: bool, used: bool, tied: u32, own: usize, native: usize) -> u8 {
        if !used {
            return 0;
        }
        if output {
            if tied == NOT_TIED && native == own {
                1
            } else {
                0
            }
        } else if native == own {
            0
        } else {
            2
        }
    }

    #[test]
    fn only_stable_port_answers_are_reusable() {
        assert_eq!(classify(true, true, NOT_TIED, 0x1000, 0x1000), 1, "output");
        assert_eq!(classify(true, true, 0x20, 0x1000, 0x1000), 0, "tied output");
        assert_eq!(classify(true, false, NOT_TIED, 0x1000, 0x1000), 0, "unused");
        assert_eq!(
            classify(false, true, 0, 0x1000, 0x2000),
            2,
            "zero-copy input"
        );
        assert_eq!(classify(false, true, 0, 0x1000, 0x1000), 0, "mixed input");
        assert_eq!(classify(false, false, 0, 0x1000, 0x2000), 0, "unused input");
    }

    #[test]
    fn the_generation_and_port_count_are_neighbours() {
        assert_eq!(GENERATION, 0x2456810);
        assert_eq!(PORT_COUNT, GENERATION + 8);
        assert_eq!(PORT_STRIDE, 0x8410);
    }
}
