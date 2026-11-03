use rc2fx_core::{SuppressionGeometry, Target, TargetKind};
use rc2fx_device::audio::{Chain, Group, PrepareError};
use rc2fx_dsp::{RetirementError, Suppression};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

struct Provider {
    geometry: SuppressionGeometry,
    budgets: Arc<Mutex<Vec<Duration>>>,
    hold: bool,
}
impl Suppression for Provider {
    fn geometry(&self) -> SuppressionGeometry {
        self.geometry
    }
    fn process_sample(&mut self, input: f32, _wet: f32) -> f32 {
        input
    }
    fn voice(&self) -> Option<f32> {
        None
    }
    fn drain(&mut self, timeout: Duration) -> Result<(), RetirementError> {
        self.budgets.lock().unwrap().push(timeout);
        if self.hold {
            std::thread::sleep(Duration::from_millis(20));
            Err(RetirementError::TimedOut)
        } else {
            Ok(())
        }
    }
}

#[test]
fn every_path_retires_under_one_budget_and_group_cannot_reopen() {
    for fail in [false, true] {
        let budgets = Arc::new(Mutex::new(Vec::new()));
        let chains = (0..2)
            .map(|index| {
                let mut target = Target::new(format!("mic-{index}"), "Mic", TargetKind::Mic, 1);
                target.suppression.enabled = true;
                let provider = Provider {
                    geometry: target.suppression.geometry().unwrap(),
                    budgets: budgets.clone(),
                    hold: fail && index == 0,
                };
                Chain::new(&target, [Some(Box::new(provider)), None]).unwrap()
            })
            .collect();
        let mut group = Group::new(chains, 1).unwrap();
        {
            let (_control, mut callbacks) = group.split().unwrap();
            for callback in &mut callbacks {
                callback
                    .process([&[0.1], &[]], [&mut [0.0], &mut []])
                    .unwrap();
            }
        }
        let result = group.drain_suppression(Duration::from_millis(10));
        assert_eq!(
            result,
            if fail {
                Err(RetirementError::TimedOut)
            } else {
                Ok(())
            }
        );
        let observed = budgets.lock().unwrap();
        assert_eq!(
            observed.len(),
            2,
            "later paths must retire after an earlier failure"
        );
        assert!(observed[1] <= observed[0]);
        if fail {
            assert_eq!(observed[1], Duration::ZERO);
        }
        drop(observed);
        assert!(group.is_retired());
        assert!(matches!(group.split(), Err(PrepareError::Retired)));
    }
}
