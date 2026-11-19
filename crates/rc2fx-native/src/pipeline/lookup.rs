//! The table that finds the path and channel of a buffer the mixer asks for.
use super::Port;

#[derive(Clone, Copy)]
pub(super) struct PinEntry {
    /// 0: an empty entry.
    pub(super) pointer: usize,
    pub(super) path: u32,
    pub(super) channel: u32,
}
/// How a table turns a port handle into its first entry: drop `drop` low
/// bits, multiply, and take the index from bit `shift` of the product up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct PinHash {
    pub(super) drop: u32,
    pub(super) shift: u32,
}
impl PinHash {
    /// What construction tries, keeping the first of those that cost least.
    ///
    /// JACK2 port handles are small consecutive port indices, not addresses.
    /// The first two hash the whole word, which spreads consecutive indices;
    /// they differ in which product bits they use, and so in which strides of
    /// aligned addresses they spread well. The last drops the low four bits
    /// first: the hash this table always had. It suits aligned allocations,
    /// and starts sixteen consecutive indices on one entry.
    pub(super) const CANDIDATES: [Self; 3] = [
        Self { drop: 0, shift: 32 },
        Self { drop: 0, shift: 48 },
        Self { drop: 4, shift: 20 },
    ];
}
/// Which path and channel a native port belongs to: an open-addressed table,
/// built at construction and at most half full, so a search always ends. The
/// mixer asks this for every buffer it fetches, most of them unbound.
pub(super) struct PinLookup {
    pub(super) entries: Box<[PinEntry]>,
    /// Chosen at construction for the handles this client really has. Any
    /// choice finds the same pins; they differ only in entries read.
    pub(super) hash: PinHash,
}
impl PinLookup {
    pub(super) fn new(ports: &[Port]) -> Self {
        let pins: Vec<PinEntry> = ports
            .iter()
            .enumerate()
            .flat_map(|(path, port)| {
                port.pins
                    .iter()
                    .enumerate()
                    .map(move |(channel, pin)| PinEntry {
                        pointer: pin.pointer,
                        path: path as u32,
                        channel: channel as u32,
                    })
            })
            .collect();
        Self::with_pins(&pins)
    }
    /// Control work: builds the table once per candidate hash and keeps the
    /// cheapest to search.
    pub(super) fn with_pins(pins: &[PinEntry]) -> Self {
        PinHash::CANDIDATES
            .into_iter()
            .map(|hash| Self::with_hash(pins, hash))
            .min_by_key(Self::cost)
            .expect("there are candidates")
    }
    pub(super) fn with_hash(pins: &[PinEntry], hash: PinHash) -> Self {
        let empty = PinEntry {
            pointer: 0,
            path: 0,
            channel: 0,
        };
        let size = (pins.len() * 2 + 1).next_power_of_two();
        let mut table = Self {
            entries: vec![empty; size].into_boxed_slice(),
            hash,
        };
        for pin in pins {
            table.insert(pin.pointer, pin.path, pin.channel);
        }
        table
    }
    /// The entries a search reads before it reaches an empty one, summed over
    /// every entry it could start at: what the table costs to search, for
    /// bound and unbound handles alike.
    pub(super) fn cost(&self) -> usize {
        (0..self.entries.len())
            .map(|start| {
                let mut index = start;
                let mut read = 1;
                while self.entries[index].pointer != 0 {
                    index = (index + 1) & self.mask();
                    read += 1;
                }
                read
            })
            .sum()
    }
    pub(super) fn start(&self, pointer: usize) -> usize {
        let product = ((pointer as u64) >> self.hash.drop).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        (product >> self.hash.shift) as usize & self.mask()
    }
    pub(super) fn mask(&self) -> usize {
        self.entries.len() - 1
    }
    /// The first path to name a port keeps it.
    pub(super) fn insert(&mut self, pointer: usize, path: u32, channel: u32) {
        let mut index = self.start(pointer);
        while self.entries[index].pointer != 0 {
            if self.entries[index].pointer == pointer {
                return;
            }
            index = (index + 1) & self.mask();
        }
        self.entries[index] = PinEntry {
            pointer,
            path,
            channel,
        };
    }
    pub(super) fn find(&self, pointer: usize) -> Option<(usize, usize)> {
        let mut index = self.start(pointer);
        loop {
            let entry = self.entries[index];
            if entry.pointer == pointer && pointer != 0 {
                return Some((entry.path as usize, entry.channel as usize));
            }
            if entry.pointer == 0 {
                return None;
            }
            index = (index + 1) & self.mask();
        }
    }
}
