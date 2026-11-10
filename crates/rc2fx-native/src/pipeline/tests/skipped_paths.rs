//! The invariant: a callback that leaves paths alone changes nothing about
//! them. Settings, modes, faults, frame counts and meters behave as if every
//! path had been visited on every callback.
use super::direct_output::{Frame, build, hardware_plan, run};
use super::*;

const OUTPUT: usize = 1;

fn front(pipeline: &Pipeline) -> &Front {
    unsafe { &*pipeline.front.get() }
}
/// Runs frames until the next `begin` would start a sparse cycle in which
/// `meters` is what the output path's stride says.
fn run_until(pipeline: &Pipeline, buffers: &Buffers, index: &mut usize, due: bool) {
    loop {
        let frame = front(pipeline);
        if frame.full_left == 0 && meters(frame.cycle, OUTPUT) == due {
            return;
        }
        run(pipeline, buffers, *index, [true, true], true);
        *index += 1;
    }
}
fn publish(control: &mut parameters::Control, plan: &Plan, revision: u64, mode: PathMode) {
    let target = &plan.paths[OUTPUT].target;
    let staged = control.prepare_mode(target, revision, mode).unwrap();
    control.reserve(staged).unwrap().commit();
}
fn same(frame: &Frame) -> bool {
    frame.out == frame.raw
}

#[test]
fn a_settled_cycle_visits_what_is_processed_and_what_is_due_to_meter() {
    let plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    let buffers = Buffers::new();
    let (pipeline, _controls) = build(&plan, &buffers, true);
    let mut index = 0;
    run_until(&pipeline, &buffers, &mut index, false);
    run(&pipeline, &buffers, index, [true, true], true);
    assert!(!front(&pipeline).full);
    for (phase, visits) in front(&pipeline).visits.iter().enumerate() {
        let expected: &[usize] = if meters(phase as u32, OUTPUT) {
            &[0, OUTPUT]
        } else {
            &[0]
        };
        assert_eq!(visits, expected, "phase {phase}");
    }
}

#[test]
fn a_path_left_alone_is_not_held_to_its_last_visit() {
    let plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    let buffers = Buffers::new();
    let (pipeline, _controls) = build(&plan, &buffers, true);
    let mut index = 0;
    run_until(&pipeline, &buffers, &mut index, true);
    // Visited, and so active, on this frame.
    run(&pipeline, &buffers, index, [true, true], true);
    index += 1;
    // The next frame leaves it alone. JACK may hand out any buffer for it now:
    // comparing with the visit's pointer would fail a healthy path.
    unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
    let elsewhere = buffers.input(1).cast::<c_void>();
    let port = buffers.output(0) as usize;
    assert_eq!(pipeline.buffer(port, FRAMES, elsewhere), elsewhere);
    unsafe { pipeline.end(FRAMES, true) };
    pipeline.settle();
    run(&pipeline, &buffers, index, [true, true], true);
    assert_eq!(pipeline.counters().faults, 0);
    assert!(!front(&pipeline).ports[OUTPUT].failed);
}

#[test]
fn an_unbound_port_is_handed_straight_back() {
    let plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    let buffers = Buffers::new();
    let (pipeline, _controls) = build(&plan, &buffers, true);
    let mut index = 0;
    run_until(&pipeline, &buffers, &mut index, false);
    unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
    let native = buffers.input(1).cast::<c_void>();
    assert_eq!(pipeline.buffer(0xdead_0000, FRAMES, native), native);
    unsafe { pipeline.end(FRAMES, true) };
    pipeline.settle();
    assert_eq!(pipeline.counters().faults, 0);
}

#[test]
fn frames_left_alone_count_exactly_as_frames_counted_every_cycle() {
    let plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    let buffers = Buffers::new();
    let (helper, mut helper_controls) = build(&plan, &buffers, false);
    let (direct, mut direct_controls) = build(&plan, &buffers, true);
    let meter = |controls: &mut Vec<parameters::Control>| {
        let value = controls[OUTPUT].status().unwrap().meter.unwrap();
        (value.callbacks, value.processed_frames, value.rms.to_bits())
    };
    // The mixer fails some callbacks, on and off the output's meter cycles.
    // Reading as control does drains the windows; 104 frames is a whole number
    // of strides, so each reading falls just after a visit.
    for index in 0..1040 {
        let success = index % 7 != 3 && index % 37 != 0;
        run(&helper, &buffers, index, [true, true], success);
        run(&direct, &buffers, index, [true, true], success);
        if (index + 1) % 104 == 0 {
            assert_eq!(
                meter(&mut helper_controls),
                meter(&mut direct_controls),
                "frame {index}"
            );
        }
    }
    assert!(meter(&mut direct_controls).0 > 800);
    assert!(!front(&direct).full, "the comparison ran on sparse cycles");
}

#[test]
fn a_callback_owned_mode_change_takes_effect_on_the_next_frame() {
    let plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    let buffers = Buffers::new();
    let (pipeline, mut controls) = build(&plan, &buffers, true);
    let mut index = 0;
    run_until(&pipeline, &buffers, &mut index, false);
    assert!(same(&run(&pipeline, &buffers, index, [true, true], true)));
    index += 1;
    run_until(&pipeline, &buffers, &mut index, false);
    publish(&mut controls[OUTPUT], &plan, 2, PathMode::Processed);
    assert!(!same(&run(&pipeline, &buffers, index, [true, true], true)));
    index += 1;
    // Well into sparse cycles, it stays processed: on every frame.
    for _ in 0..40 {
        assert!(!same(&run(&pipeline, &buffers, index, [true, true], true)));
        index += 1;
    }
    assert!(!front(&pipeline).full);
    publish(&mut controls[OUTPUT], &plan, 3, PathMode::Passthrough);
    assert!(same(&run(&pipeline, &buffers, index, [true, true], true)));
    assert_eq!(pipeline.counters().faults, 0);
}

#[test]
fn a_mode_seen_before_its_publication_is_counted_is_never_a_lone_frame() {
    let plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    let buffers = Buffers::new();
    let (pipeline, mut controls) = build(&plan, &buffers, true);
    let mut index = 0;
    run_until(&pipeline, &buffers, &mut index, false);
    publish(&mut controls[OUTPUT], &plan, 2, PathMode::Processed);
    // As if the control thread stopped between the store and the count.
    pipeline.publications.fetch_sub(1, Ordering::Release);
    let mut processed = false;
    for _ in 0..3 * METER_STRIDE as usize {
        let frame = run(&pipeline, &buffers, index, [true, true], true);
        index += 1;
        assert!(
            !(processed && same(&frame)),
            "frame {index} went back to unprocessed"
        );
        processed |= !same(&frame);
    }
    assert!(processed);
}

/// Frames from a Passthrough-to-Processed publication until the mixer is first
/// handed something other than its own source buffer.
fn frames_until_processed(pipeline: &Pipeline, buffers: &Buffers) -> usize {
    for offset in 0..16 {
        unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
        let raw = buffers.input(0);
        let source = pipeline.buffer(raw as usize, FRAMES, raw.cast());
        let output = buffers.output(0);
        pipeline.buffer(output as usize, FRAMES, output.cast());
        let output = buffers.output(1);
        pipeline.buffer(output as usize, FRAMES, output.cast());
        unsafe { pipeline.end(FRAMES, true) };
        pipeline.settle();
        if source != raw.cast() {
            return offset;
        }
    }
    panic!("the source never became processed");
}

#[test]
fn a_helper_owned_mode_change_arrives_as_soon_as_when_every_path_is_visited() {
    let mut plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    plan.paths[0].mode = PathMode::Passthrough;
    let commit = |control: &mut parameters::Control| {
        let target = &plan.paths[0].target;
        let staged = control
            .prepare_mode(target, 2, PathMode::Processed)
            .unwrap();
        control.reserve(staged).unwrap().commit();
    };
    let buffers = Buffers::new();
    let (visiting, mut controls) = build(&plan, &buffers, true);
    commit(&mut controls[0]);
    let expected = frames_until_processed(&visiting, &buffers);
    let (sparse, mut controls) = build(&plan, &buffers, true);
    let mut index = 0;
    run_until(&sparse, &buffers, &mut index, false);
    assert_eq!(front(&sparse).visits.iter().flatten().count(), 1 + 1);
    commit(&mut controls[0]);
    assert_eq!(frames_until_processed(&sparse, &buffers), expected);
}

#[test]
fn a_path_the_mixer_never_requests_still_acknowledges_a_setting() {
    let plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    let buffers = Buffers::new();
    let (pipeline, mut controls) = build(&plan, &buffers, true);
    let mut index = 0;
    run_until(&pipeline, &buffers, &mut index, true);
    // Published on its meter cycle, when an unrequested path cannot apply: it
    // acknowledges on one of the stride's other cycles, as it always did.
    publish(&mut controls[OUTPUT], &plan, 2, PathMode::Passthrough);
    for offset in 0..METER_STRIDE as usize {
        run(&pipeline, &buffers, index + offset, [false, false], true);
    }
    let status = controls[OUTPUT].status().unwrap();
    assert_eq!((status.seen, status.applied), (2, 2));
}

#[test]
fn a_mixer_failure_does_not_lose_a_settings_acknowledgement() {
    let plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    let buffers = Buffers::new();
    let (pipeline, mut controls) = build(&plan, &buffers, true);
    for index in 0..40 {
        run(&pipeline, &buffers, index, [true, true], true);
    }
    publish(&mut controls[OUTPUT], &plan, 2, PathMode::Passthrough);
    for index in 40..80 {
        run(&pipeline, &buffers, index, [true, true], false);
    }
    let status = controls[OUTPUT].status().unwrap();
    assert_eq!((status.seen, status.applied), (2, 1));
    run(&pipeline, &buffers, 80, [true, true], true);
    assert_eq!(controls[OUTPUT].status().unwrap().applied, 2);
}

#[test]
fn a_failed_path_stays_visited_and_stops_counting() {
    let plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    let buffers = Buffers::new();
    let (pipeline, mut controls) = build(&plan, &buffers, true);
    let mut index = 0;
    run_until(&pipeline, &buffers, &mut index, true);
    // A wrong buffer on its visit fails the path.
    unsafe { pipeline.begin(FRAMES, test_buffer, 0) };
    let elsewhere = buffers.input(1).cast::<c_void>();
    pipeline.buffer(buffers.output(0) as usize, FRAMES, elsewhere);
    unsafe { pipeline.end(FRAMES, true) };
    pipeline.settle();
    assert_eq!(pipeline.counters().faults, 1);
    let mut settled = None;
    for _ in 0..200 {
        run(&pipeline, &buffers, index, [true, true], true);
        index += 1;
        if settled.is_none() && !front(&pipeline).full {
            settled = controls[OUTPUT].status().unwrap().meter;
        }
    }
    assert!(!front(&pipeline).full);
    assert!(
        front(&pipeline)
            .visits
            .iter()
            .all(|visits| visits.contains(&OUTPUT))
    );
    let meter = controls[OUTPUT].status().unwrap().meter;
    assert_eq!(
        meter.map(|value| value.processed_frames),
        settled.map(|value| value.processed_frames),
        "a failed path must look stalled to the liveness check"
    );
}

#[test]
fn sparse_cycles_and_replanning_allocate_nothing_in_the_callback() {
    use super::super::super::{
        CALLBACK_ALLOCATIONS, CALLBACK_FREES, CALLBACK_REALLOCATIONS, CallbackFrame, INSIDE,
    };
    let plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    let buffers = Buffers::new();
    let (pipeline, mut controls) = build(&plan, &buffers, true);
    let counts = || {
        [
            CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
            CALLBACK_FREES.load(Ordering::Relaxed),
            CALLBACK_REALLOCATIONS.load(Ordering::Relaxed),
        ]
    };
    let inside = CallbackFrame {
        owner: 1,
        processing: true,
        pipeline: 0,
        started: 0,
    };
    let before = counts();
    for index in 0..80 {
        if index == 40 {
            publish(&mut controls[OUTPUT], &plan, 2, PathMode::Passthrough);
        }
        let previous = INSIDE.with(|value| value.replace(inside));
        run(&pipeline, &buffers, index, [true, true], true);
        INSIDE.with(|value| value.set(previous));
    }
    assert_eq!(counts(), before);
}

#[test]
fn the_port_lookup_finds_every_pin_and_nothing_else() {
    let plan = hardware_plan(2, PathMode::Passthrough, 0.5);
    let buffers = Buffers::new();
    let (pipeline, _controls) = build(&plan, &buffers, true);
    let ports = &front(&pipeline).ports;
    let lookup = PinLookup::new(ports);
    for (path, port) in ports.iter().enumerate() {
        for (channel, pin) in port.pins.iter().enumerate() {
            assert_eq!(lookup.find(pin.pointer), Some((path, channel)));
        }
    }
    assert_eq!(lookup.find(0), None);
    assert_eq!(lookup.find(0x1000), None);
    // Colliding, adjacent and repeated pointers at the table's real load.
    let pointers: Vec<usize> = (1..=50).map(|n| 0x7f00_0000_0000 + n * 0x40).collect();
    for hash in PinHash::CANDIDATES {
        let mut table = PinLookup {
            entries: vec![
                PinEntry {
                    pointer: 0,
                    path: 0,
                    channel: 0,
                };
                128
            ]
            .into_boxed_slice(),
            hash,
        };
        for (path, pointer) in pointers.iter().enumerate() {
            table.insert(*pointer, path as u32, 1);
        }
        table.insert(pointers[3], 99, 0);
        for (path, pointer) in pointers.iter().enumerate() {
            assert_eq!(
                table.find(*pointer),
                Some((path, 1)),
                "the first path keeps it"
            );
        }
        assert_eq!(table.find(0x7f00_0000_0000), None);
    }
}

/// The table as it was before: the low four bits dropped, bits 20 up.
const OLD_HASH: PinHash = PinHash { drop: 4, shift: 20 };
fn pin_entries(pins: &[usize]) -> Vec<PinEntry> {
    pins.iter()
        .enumerate()
        .map(|(path, &pointer)| PinEntry {
            pointer,
            path: path as u32,
            channel: 1,
        })
        .collect()
}
/// Entries a search for `pointer` reads.
fn probes(table: &PinLookup, pointer: usize) -> usize {
    let mut index = table.start(pointer);
    let mut count = 1;
    while table.entries[index].pointer != 0 && table.entries[index].pointer != pointer {
        index = (index + 1) & table.mask();
        count += 1;
    }
    count
}
/// Longest and total search over `asked`.
fn search_cost(table: &PinLookup, asked: &[usize]) -> (usize, usize) {
    asked
        .iter()
        .map(|&pointer| probes(table, pointer))
        .fold((0, 0), |(longest, total), length| {
            (longest.max(length), total + length)
        })
}
/// Every candidate hash, and the chosen table, answer exactly as a plain
/// search of the pins does, for every port asked about.
fn assert_same_answers(pins: &[usize], asked: &[usize]) -> PinLookup {
    let entries = pin_entries(pins);
    let chosen = PinLookup::with_pins(&entries);
    let tables = PinHash::CANDIDATES.map(|hash| PinLookup::with_hash(&entries, hash));
    assert!(tables.iter().all(|table| chosen.cost() <= table.cost()));
    for &port in asked {
        let expected = pins
            .iter()
            .position(|&pin| pin == port && port != 0)
            .map(|path| (path, 1));
        assert_eq!(chosen.find(port), expected, "{port:#x}");
        for table in &tables {
            assert_eq!(table.find(port), expected, "{port:#x}, {:?}", table.hash);
        }
    }
    chosen
}
/// The Pro II plan's pins among a client's 122 consecutive handles: sources
/// in_1..20 and in_29..36 (as many as there are inputs) and 16 outputs.
/// `inputs` and `inputs_first` vary which handles they get.
fn pro_pins(base: usize, inputs: usize, inputs_first: bool) -> (Vec<usize>, Vec<usize>) {
    let outputs = 122 - inputs;
    let (first_input, first_output) = if inputs_first {
        (base, base + inputs)
    } else {
        (base + outputs, base)
    };
    let mut pins: Vec<usize> = (1..=20)
        .chain(29..=36)
        .filter(|n| *n <= inputs)
        .map(|n| first_input + n - 1)
        .collect();
    pins.extend((0..16).map(|n| first_output + n));
    (pins, (base..base + 122).collect())
}

/// JACK2 hands out small consecutive port indices as handles. Every one of a
/// client's 122 ports, bound or not, is found or ruled out within three
/// entries, where the old hash read dozens.
#[test]
fn consecutive_port_handles_are_found_within_a_few_entries() {
    let (mut new_total, mut old_total, mut old_longest) = (0, 0, 0);
    for base in (1..300).chain([500, 1000, 2047, 3000, 3900, 4000]) {
        for inputs in [20, 36, 61, 86, 106] {
            for inputs_first in [true, false] {
                let (pins, ports) = pro_pins(base, inputs, inputs_first);
                let chosen = assert_same_answers(&pins, &ports);
                assert_eq!(chosen.hash.drop, 0, "the whole handle is hashed");
                let (longest, total) = search_cost(&chosen, &ports);
                assert!(
                    longest <= 3,
                    "base {base}, {inputs} inputs, first {inputs_first}: {longest} entries"
                );
                new_total += total;
                let old = PinLookup::with_hash(&pin_entries(&pins), OLD_HASH);
                let (longest, total) = search_cost(&old, &ports);
                old_longest = old_longest.max(longest);
                old_total += total;
            }
        }
    }
    // The real plan: 44 pins in a 128-entry table.
    let (pins, _) = pro_pins(1, 36, true);
    assert_eq!(pins.len(), 44);
    assert_eq!(PinLookup::with_pins(&pin_entries(&pins)).entries.len(), 128);
    // On the same layouts the old hash read over three times as many entries.
    assert!(
        old_total > 3 * new_total && old_longest > 20,
        "old {old_total} entries (longest {old_longest}), new {new_total}"
    );
}

/// Handles that are aligned addresses are served as well as before: the old
/// hash is one of the candidates, and the choice is the cheapest table.
#[test]
fn aligned_pointer_handles_are_found_within_a_few_entries() {
    let (mut new_total, mut old_total) = (0, 0);
    for stride in (0x10..=0x160).step_by(0x10) {
        for base in [
            0x7f00_0000_0000usize,
            0x5555_5555_0000,
            0xaaaa_bbb0_0040,
            0x7f3a_1c2d_4000,
            0xffff_8000_a000,
            0x55d0_c0de_0010,
        ] {
            let ports: Vec<usize> = (0..122).map(|n| base + n * stride).collect();
            let layouts = [
                ports
                    .iter()
                    .copied()
                    .step_by(3)
                    .take(44)
                    .collect::<Vec<_>>(),
                [&ports[..20], &ports[28..36], &ports[40..56]].concat(),
                ports[78..].to_vec(),
            ];
            for pins in layouts {
                // 41 or 44 pins: a 128-entry table, as the real plan's.
                assert!((41..=44).contains(&pins.len()));
                let chosen = assert_same_answers(&pins, &ports);
                let (longest, total) = search_cost(&chosen, &ports);
                assert!(longest <= 4, "stride {stride:#x}: {longest} entries");
                new_total += total;
                let old = PinLookup::with_hash(&pin_entries(&pins), OLD_HASH);
                old_total += search_cost(&old, &ports).1;
            }
        }
    }
    assert!(
        new_total <= old_total,
        "old {old_total} entries, new {new_total}"
    );
}

/// Handles of any form, not only the two expected ones: random words, small
/// numbers, handles that differ only in their high bits, handles that every
/// candidate hash starts on one entry (a full collision), repeats, zero and
/// the largest word. Every candidate table and the chosen one answer exactly
/// as a plain search of the pins does, for bound and unbound handles, and
/// every search ends.
#[test]
fn any_handles_are_answered_as_a_plain_search_answers_them() {
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut random = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut collided = 0;
    for round in 0..600 {
        let count = match round % 6 {
            0 => 0,
            1 => 1,
            2 => 44,
            _ => (random() % 300) as usize,
        };
        let family = (round / 6) % 7;
        let mut handle = |n: usize| -> usize {
            let n = n as u64 + 1;
            (match family {
                // Any word at all.
                0 => random(),
                // Small numbers in any order, with repeats.
                1 => random() % 97,
                // Only the high bits differ.
                2 => n << 32,
                3 => n << 48,
                // Every candidate starts all of these on entry 0, in a table
                // of up to 1024 entries. Only 63 are distinct, and one is 0.
                4 => n << 58,
                // Addresses a few bytes apart, unaligned.
                5 => 0x7f3a_1c2d_4001 + n * 3,
                // Zero and the extremes among ordinary handles.
                _ => match random() % 5 {
                    0 => 0,
                    1 => u64::MAX,
                    2 => u64::MAX - (random() % 4),
                    _ => 1 + random() % 4096,
                },
            }) as usize
        };
        let pins: Vec<usize> = (0..count).map(&mut handle).collect();
        // Every pin, handles beside each pin, and handles that are not pins.
        let mut asked = pins.clone();
        asked.extend(pins.iter().flat_map(|pin| {
            [
                pin.wrapping_add(1),
                pin.wrapping_sub(1),
                pin ^ 0x10,
                pin ^ (1 << 40),
            ]
        }));
        asked.extend((0..64).map(&mut handle));
        asked.extend([0, 1, usize::MAX]);
        let chosen = assert_same_answers(&pins, &asked);
        // At most half full, so that a search always reaches an empty entry.
        let used = chosen
            .entries
            .iter()
            .filter(|entry| entry.pointer != 0)
            .count();
        assert!(used * 2 < chosen.entries.len() + 1, "round {round}");
        if family == 4 && count > 1 {
            for hash in PinHash::CANDIDATES {
                let table = PinLookup::with_hash(&pin_entries(&pins), hash);
                assert!(pins.iter().all(|pin| table.start(*pin) == 0), "{hash:?}");
                collided += 1;
            }
        }
    }
    assert!(collided > 0, "the full collision was exercised");
}

/// A plan with no pins has a one-entry table: nothing is found, and no index
/// leaves the table.
#[test]
fn a_lookup_with_no_pins_finds_nothing() {
    for lookup in [PinLookup::new(&[]), PinLookup::with_pins(&[])] {
        assert_eq!(lookup.entries.len(), 1);
        assert_eq!(lookup.cost(), 1);
        for pointer in [0, 1, 2, 0x2b, 0x1000, usize::MAX, 0x7f00_0000_0040] {
            assert_eq!(lookup.start(pointer), 0);
            assert_eq!(lookup.find(pointer), None);
        }
    }
    for hash in PinHash::CANDIDATES {
        let lookup = PinLookup::with_hash(&[], hash);
        assert_eq!(lookup.find(usize::MAX), None);
    }
}
