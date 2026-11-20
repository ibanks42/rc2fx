//! Open-addressed lookup table mapping native JACK port handles to `(path, channel)`.
use super::Port;

#[derive(Clone, Copy)]
pub(super) struct PinEntry {
    /// 0 marks an empty slot.
    pub(super) pointer: usize,
    pub(super) path: u32,
    pub(super) channel: u32,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct PinHash {
    pub(super) drop: u32,
    pub(super) shift: u32,
}
impl PinHash {
    /// Candidate Fibonacci hashes evaluated at construction; we keep whichever
    /// yields the fewest total probe steps for the client's actual port handles.
    /// JACK2 port handles are small consecutive integers, so `drop: 0` hashes
    /// spread them cleanly, while `drop: 4` works well for aligned pointers.
    pub(super) const CANDIDATES: [Self; 3] = [
        Self { drop: 0, shift: 32 },
        Self { drop: 0, shift: 48 },
        Self { drop: 4, shift: 20 },
    ];
}
/// Open-addressed table (load factor <= 0.5) queried on every intercepted
/// `jack_port_get_buffer` call, most of which miss because the port isn't bound.
pub(super) struct PinLookup {
    pub(super) entries: Box<[PinEntry]>,
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
    /// Total probe length to reach an empty slot summed over every starting bucket.
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
