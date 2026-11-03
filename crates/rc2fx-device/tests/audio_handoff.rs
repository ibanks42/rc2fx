use rc2fx_core::{Band, BandType, Target, TargetKind};
use rc2fx_device::audio::{Chain, Group, PrepareError};
use rc2fx_dsp::{Dsp, Prepared};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

thread_local! { static ALLOCATIONS: Cell<Option<[usize; 3]>> = const { Cell::new(None) }; }
struct Counting;
fn count(index: usize) {
    ALLOCATIONS.with(|state| {
        if let Some(mut counts) = state.get() {
            counts[index] += 1;
            state.set(Some(counts));
        }
    });
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(0);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        count(1);
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count(2);
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn target() -> Target {
    let mut target = Target::new("usb-main", "USB main", TargetKind::Playback, 2);
    target.compressor.enabled = true;
    target.highpass.enabled = true;
    target.eq.enabled = true;
    let mut band = Band::new("bell");
    band.band_type = BandType::Bell;
    band.frequency = 1510.0;
    band.gain = 7.0;
    target.eq.bands.push(band);
    target
}

#[test]
fn concurrent_paths_preserve_exact_progress_across_endpoint_reopening() {
    let targets: Vec<_> = (0..4)
        .map(|i| {
            let mut value = target();
            value.id = format!("path-{i}");
            value
        })
        .collect();
    let chains = targets
        .iter()
        .map(|t| Chain::new(t, [None, None]).unwrap())
        .collect();
    let mut group = Group::new(chains, 1).unwrap();
    for round in 1..=2 {
        let (control, callbacks) = group.split().unwrap();
        std::thread::scope(|scope| {
            for (index, mut callback) in callbacks.into_iter().enumerate() {
                scope.spawn(move || {
                    let input = [0.1; 112];
                    let mut left = [0.0; 112];
                    let mut right = [0.0; 112];
                    for _ in 0..(index + 1) * 1000 {
                        callback
                            .process([&input, &input], [&mut left, &mut right])
                            .unwrap();
                    }
                });
            }
        });
        for index in 0..4 {
            let calls = round * (index as u64 + 1) * 1000;
            assert_eq!(control.callbacks(index), Some(calls));
            assert_eq!(control.processed_frames(index), Some(calls * 112));
        }
        assert_eq!(control.applied_revision(), 1);
    }
}

#[test]
fn one_slow_path_delays_group_ack_and_reclamation_but_not_other_audio() {
    let mut targets = vec![target(), target()];
    targets[1].id = "send-chat".into();
    targets[1].kind = TargetKind::Output;
    let chains = targets
        .iter()
        .map(|target| Chain::new(target, [None, None]).unwrap())
        .collect();
    let mut group = Group::new(chains, 1).unwrap();
    let (mut control, mut callbacks) = group.split().unwrap();
    // Catalog order changes must not change callback-to-target identity.
    targets.reverse();
    targets[0].gain.db = -10.0;
    control.reserve(&targets, 2).unwrap().commit();
    for _ in 0..100 {
        callbacks[0]
            .process([&[0.1], &[0.1]], [&mut [0.0], &mut [0.0]])
            .unwrap();
    }
    assert_eq!(control.applied_revision(), 0);
    assert_eq!(control.processed_frames(0), Some(100));
    assert_eq!(control.processed_frames(1), Some(0));
    assert!(!control.reclaim());
    assert!(matches!(
        control.reserve(&targets, 3),
        Err(PrepareError::Busy)
    ));
    callbacks[1]
        .process([&[], &[]], [&mut [], &mut []])
        .unwrap();
    assert!(control.reclaim()); // Empty block is quiescence, not audio proof.
    assert_eq!(control.applied_revision(), 0);
    callbacks[1]
        .process([&[0.1], &[0.1]], [&mut [0.0], &mut [0.0]])
        .unwrap();
    assert_eq!(control.applied_revision(), 2);
    control.reserve(&targets, 3).unwrap().commit();
    assert_eq!(control.published_revision(), 3);
    assert_eq!(control.applied_revision(), 0);
}

#[test]
fn concurrent_callbacks_and_control_publish_reclaim_without_callback_allocator_work() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct StopOnDrop<'a>(&'a AtomicBool);
    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    let mut targets = vec![target(), target()];
    targets[1].id = "send-chat".into();
    targets[1].kind = TargetKind::Output;
    let chains = targets
        .iter()
        .map(|target| Chain::new(target, [None, None]).unwrap())
        .collect();
    let mut group = Group::new(chains, 1).unwrap();
    let (mut control, callbacks) = group.split().unwrap();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let mut tasks = Vec::new();
        for mut callback in callbacks {
            let stop = &stop;
            tasks.push(scope.spawn(move || {
                let input = [0.1; 32];
                let mut left = [0.0; 32];
                let mut right = [0.0; 32];
                ALLOCATIONS.with(|state| state.set(Some([0; 3])));
                while !stop.load(Ordering::Relaxed) {
                    callback
                        .process([&input, &input], [&mut left, &mut right])
                        .unwrap();
                    std::thread::yield_now();
                }
                ALLOCATIONS.with(|state| state.replace(None).unwrap())
            }));
        }
        let _stop_on_unwind = StopOnDrop(&stop);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        for revision in 2..=5000 {
            targets[0].gain.db = -((revision % 12) as f64);
            targets[0].order.rotate_left(1);
            targets[1].eq.bands[0].gain = (revision % 4) as f64;
            targets[1].suppression.wet = if revision % 2 == 0 { 0.5 } else { 1.0 };
            loop {
                assert!(std::time::Instant::now() < deadline, "publication stalled");
                match control.reserve(&targets, revision) {
                    Ok(reservation) => {
                        reservation.commit();
                        break;
                    }
                    Err(PrepareError::Busy) => std::thread::yield_now(),
                    Err(error) => panic!("{error}"),
                }
            }
        }
        while control.applied_revision() != 5000 {
            assert!(
                std::time::Instant::now() < deadline,
                "audio acknowledgement stalled"
            );
            std::thread::yield_now();
        }
        stop.store(true, Ordering::Relaxed);
        for task in tasks {
            assert_eq!(task.join().unwrap(), [0; 3]);
        }
    });
    assert!(control.reclaim());
    assert_eq!(control.applied_revision(), 5000);
    assert!(control.callbacks(0).unwrap() > 0);
    assert!(control.callbacks(1).unwrap() > 0);
}

#[test]
fn callback_handoff_preserves_dsp_history_without_allocation_or_free() {
    let mut target = target();
    let mut chain = Group::new(vec![Chain::new(&target, [None, None]).unwrap()], 1).unwrap();
    let (mut control, mut callbacks) = chain.split().unwrap();
    let mut callback = callbacks.remove(0);
    let mut prepared = Prepared::new(&target, 48_000).unwrap();
    let mut reference = Dsp::without_suppression(&prepared).unwrap();
    let left = std::array::from_fn::<_, 112, _>(|i| ((i * 37 % 112) as f32 - 56.0) / 180.0);
    let right = std::array::from_fn::<_, 112, _>(|i| ((i * 19 % 112) as f32 - 56.0) / 180.0);
    let mut actual = [[0.0; 112]; 2];
    let mut expected = actual;
    assert_eq!(control.applied_revision(), 0);
    for block in 0..120 {
        if block == 30 || block == 65 || block == 90 {
            target.eq.bands[0].frequency += 350.0;
            target.gain.db -= 4.0;
            if block == 65 {
                target.eq.bands.push(Band::new("second"));
            }
            if block == 90 {
                target.order.swap(0, 4);
            }
            let revision = match block {
                30 => 2,
                65 => 3,
                _ => 4,
            };
            control
                .reserve(std::slice::from_ref(&target), revision)
                .unwrap()
                .commit();
            assert_eq!(control.applied_revision(), 0);
            prepared = Prepared::new(&target, 48_000).unwrap();
        }
        let [a, b] = &mut actual;
        ALLOCATIONS.with(|state| state.set(Some([0; 3])));
        let result = callback.process([&left, &right], [a, b]);
        let allocations = ALLOCATIONS.with(|state| state.replace(None).unwrap());
        assert_eq!(
            allocations, [0; 3],
            "allocation/free/reallocation in callback"
        );
        let meter = result.unwrap();
        let [a, b] = &mut expected;
        let expected_meter = reference
            .process(&prepared, [&left, &right], [a, b])
            .unwrap();
        assert_eq!(meter, expected_meter);
        for channel in 0..2 {
            for sample in 0..112 {
                assert_eq!(
                    actual[channel][sample].to_bits(),
                    expected[channel][sample].to_bits()
                );
            }
        }
        control.reclaim();
    }
    assert_eq!(control.applied_revision(), 4);
    assert_eq!(control.callbacks(0).unwrap(), 120);
    assert_eq!(control.processed_frames(0).unwrap(), 120 * 112);
}

#[test]
fn reservation_cancellation_and_structural_changes_do_not_publish() {
    let mut target = target();
    let mut chain = Group::new(vec![Chain::new(&target, [None, None]).unwrap()], 9).unwrap();
    let (mut control, mut callbacks) = chain.split().unwrap();
    let mut callback = callbacks.remove(0);
    drop(control.reserve(std::slice::from_ref(&target), 10).unwrap()); // failed durable save
    assert!(matches!(
        control.reserve(std::slice::from_ref(&target), 9),
        Err(PrepareError::StaleRevision)
    ));
    target.id = "another".into();
    assert!(matches!(
        control.reserve(std::slice::from_ref(&target), 10),
        Err(PrepareError::WrongTargets)
    ));
    target.id = "usb-main".into();
    target.channels = 1;
    assert!(matches!(
        control.reserve(std::slice::from_ref(&target), 10),
        Err(PrepareError::RequiresRestart)
    ));
    target.channels = 2;
    control
        .reserve(std::slice::from_ref(&target), 10)
        .unwrap()
        .commit();
    assert!(matches!(
        control.reserve(std::slice::from_ref(&target), 11),
        Err(PrepareError::Busy)
    ));
    assert_eq!(control.applied_revision(), 0);
    // Consuming the snapshot alone is not an audio acknowledgement.
    callback.process([&[], &[]], [&mut [], &mut []]).unwrap();
    assert_eq!(control.applied_revision(), 0);
    assert!(
        callback
            .process([&[0.1], &[]], [&mut [0.0], &mut []])
            .is_err()
    );
    assert_eq!(control.applied_revision(), 0);
    callback
        .process([&[0.1], &[0.1]], [&mut [0.0], &mut [0.0]])
        .unwrap();
    assert_eq!(control.applied_revision(), 10);
    assert_eq!(control.processed_frames(0).unwrap(), 1);
}
