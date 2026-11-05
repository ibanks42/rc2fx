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
    };
    let pointers: Vec<usize> = (1..=50).map(|n| 0x7f00_0000_0000 + n * 0x40).collect();
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
