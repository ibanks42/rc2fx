use rc2fx_core::{Band, BandMode, Channel, Effect, EqSettings, Target};

pub fn active_channel(target: &Target, channel: Channel) -> Channel {
    if target.channels == 2 && target.eq.right_bands.is_some() {
        channel
    } else {
        Channel::Left
    }
}

pub fn split(target: &mut Target) {
    if target.channels == 2 && target.eq.right_bands.is_none() {
        target.eq.right_bands = Some(target.eq.bands.clone());
    }
}

pub fn link(target: &mut Target, keep: Channel) {
    if keep == Channel::Right
        && let Some(right) = target.eq.right_bands.take()
    {
        target.eq.bands = right;
    }
    target.eq.right_bands = None;
}

/// Move the band at `from` to `to`, shifting the bands between, as dropping
/// one band chip on another does; returns where it landed.
pub fn place_band(eq: &mut EqSettings, channel: Channel, from: usize, to: usize) -> usize {
    let bands = eq.channel_mut(channel);
    if from >= bands.len() || to >= bands.len() {
        return from;
    }
    let band = bands.remove(from);
    bands.insert(to, band);
    to
}

pub fn set_model(band: &mut Band, mode: BandMode) {
    band.mode = mode;
}

/// Move `effect` to where `onto` is in the visible chain, shifting the
/// stages between them, as dropping one stage card on another does. Stages
/// this target cannot use keep their places.
pub fn place_effect(target: &mut Target, effect: Effect, onto: Effect) {
    if target.is_chat_output() && (effect == Effect::Suppression || onto == Effect::Suppression) {
        return;
    }
    let slots: Vec<usize> = target
        .order
        .iter()
        .enumerate()
        .filter_map(|(i, e)| {
            ((target.supports_suppression() && !target.is_chat_output())
                || *e != Effect::Suppression)
                .then_some(i)
        })
        .collect();
    let mut visible: Vec<Effect> = slots.iter().map(|i| target.order[*i]).collect();
    let (Some(from), Some(to)) = (
        visible.iter().position(|e| *e == effect),
        visible.iter().position(|e| *e == onto),
    ) else {
        return;
    };
    let moved = visible.remove(from);
    visible.insert(to, moved);
    for (slot, effect) in slots.into_iter().zip(visible) {
        target.order[slot] = effect;
    }
}

/// Keep the single enabled destination invariant in the draft as well as the controller.
pub fn suppression_blocker<'a>(
    targets: &'a [Target],
    saved: Option<&'a [Target]>,
    id: &str,
) -> Option<&'a Target> {
    targets
        .iter()
        .chain(saved.unwrap_or(&[]))
        .find(|target| target.id != id && target.suppression.enabled)
}
/// Explicitly release a saved or hidden owner's slot in the local draft only.
/// The saved owner still blocks a new enable until this edit is applied.
pub fn release_suppression(targets: &mut [Target], owner_id: &str) -> bool {
    let Some(owner) = targets.iter_mut().find(|target| target.id == owner_id) else {
        return false;
    };
    let changed = owner.suppression.enabled;
    owner.suppression.enabled = false;
    changed
}

pub fn enable_effect(
    targets: &mut [Target],
    saved: Option<&[Target]>,
    selected: usize,
    effect: Effect,
    enabled: bool,
) -> bool {
    let Some(target) = targets.get(selected) else {
        return false;
    };
    if effect == Effect::Suppression
        && enabled
        && (!target.supports_suppression()
            || suppression_blocker(targets, saved, &target.id).is_some())
    {
        return false;
    }
    targets[selected].set_effect_enabled(effect, enabled);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use rc2fx_core::{BandType, Config, TargetKind};

    #[test]
    fn hidden_owner_needs_explicit_release_and_saved_ack_before_reassignment() {
        let mut config = Config {
            selected_targets: vec!["mic-2".into()],
            ..Config::default()
        };
        config
            .targets
            .push(Target::new("mic-2", "Mic 1", TargetKind::Mic, 1));
        let owner = config.targets.iter_mut().find(|t| t.id == "mic-1").unwrap();
        owner.suppression.enabled = true;
        let saved = config.targets.clone();
        let before = saved.iter().find(|t| t.id == "mic-1").unwrap().clone();
        let mut draft = saved.clone();
        // Identity, not the original index or a fader position, locates the owner.
        draft.reverse();
        let next = draft.iter().position(|t| t.id == "mic-2").unwrap();
        assert!(!config.is_selected("mic-1"));
        assert_eq!(
            suppression_blocker(&draft, Some(&saved), "mic-2")
                .unwrap()
                .id,
            "mic-1"
        );
        assert!(release_suppression(&mut draft, "mic-1"));
        let mut expected = before;
        expected.suppression.enabled = false;
        assert_eq!(draft.iter().find(|t| t.id == "mic-1").unwrap(), &expected);
        assert!(!release_suppression(&mut draft, "mic-1"));
        assert!(!release_suppression(&mut draft, "missing"));
        assert!(!enable_effect(
            &mut draft,
            Some(&saved),
            next,
            Effect::Suppression,
            true
        ));
        // A successful Save acknowledges the disabled owner.
        let acknowledged = draft.clone();
        assert!(enable_effect(
            &mut draft,
            Some(&acknowledged),
            next,
            Effect::Suppression,
            true
        ));
    }

    #[test]
    fn suppression_draft_has_one_owner_and_chat_voice_is_before_the_stereo_chain() {
        let mut targets = vec![
            Target::new("mic-1", "Mic", TargetKind::Mic, 1),
            Target::new("send-chat", "Chat", TargetKind::Output, 2),
            Target::new("headphones-1", "Phones", TargetKind::Output, 2),
        ];
        assert!(enable_effect(
            &mut targets,
            None,
            0,
            Effect::Suppression,
            true
        ));
        let saved = targets.clone();
        assert!(!enable_effect(
            &mut targets,
            None,
            1,
            Effect::Suppression,
            true
        ));
        assert!(enable_effect(
            &mut targets,
            Some(&saved),
            0,
            Effect::Suppression,
            false
        ));
        assert!(
            !enable_effect(&mut targets, Some(&saved), 1, Effect::Suppression, true),
            "apply the disable before claiming a new live destination"
        );
        let saved = targets.clone();
        assert!(enable_effect(
            &mut targets,
            Some(&saved),
            1,
            Effect::Suppression,
            true
        ));
        assert!(!enable_effect(
            &mut targets,
            None,
            2,
            Effect::Suppression,
            true
        ));
        let before = targets[1].order.clone();
        place_effect(&mut targets[1], Effect::Suppression, Effect::Limiter);
        assert_eq!(
            targets[1].order, before,
            "a pre-mix voice stage cannot be dragged into stereo output effects"
        );
    }

    #[test]
    fn split_edit_reorder_link_preserves_selected_channel_and_ids() {
        let mut target = Config::default().targets[4].clone();
        for _ in 0..3 {
            target.eq.add_band(Channel::Left).unwrap();
        }
        let original = target.eq.bands.clone();
        split(&mut target);
        target.eq.channel_mut(Channel::Right)[0].gain = -7.5;
        let selected = place_band(&mut target.eq, Channel::Right, 0, 1);
        assert_eq!(selected, 1);
        assert_eq!(target.eq.channel(Channel::Right)[1].id, original[0].id);
        assert_eq!(target.eq.bands, original);
        link(&mut target, Channel::Right);
        assert!(target.eq.right_bands.is_none());
        assert_eq!(target.eq.bands[1].gain, -7.5);
        assert_eq!(active_channel(&target, Channel::Right), Channel::Left);
        // Dropping across several bands shifts the ones between.
        let ids = |t: &Target| -> Vec<String> { t.eq.bands.iter().map(|b| b.id.clone()).collect() };
        let order = ids(&target);
        assert_eq!(place_band(&mut target.eq, Channel::Left, 0, 2), 2);
        assert_eq!(
            ids(&target),
            [order[1].clone(), order[2].clone(), order[0].clone()]
        );
        assert_eq!(place_band(&mut target.eq, Channel::Left, 2, 0), 0);
        assert_eq!(ids(&target), order, "and back");
        assert_eq!(place_band(&mut target.eq, Channel::Left, 0, 9), 0);
        assert_eq!(ids(&target), order, "out of range is ignored");
    }

    #[test]
    fn model_change_and_visible_effect_order_stay_valid() {
        let mut target = Config::default().targets[4].clone();
        let band = target.eq.add_band(Channel::Left).unwrap();
        band.band_type = BandType::HighShelf;
        set_model(band, BandMode::RlcBt);
        assert_eq!(band.mode, BandMode::RlcBt);
        place_effect(&mut target, Effect::Eq, Effect::Highpass);
        assert_eq!(target.order[0], Effect::Eq);
        assert_eq!(target.order[1], Effect::Suppression);
        assert_eq!(target.order[2], Effect::Highpass);
        target.validate().unwrap();
        // Dropping across several stages shifts the ones between.
        let before = target.order.clone();
        place_effect(&mut target, Effect::Limiter, Effect::Eq);
        assert_eq!(target.order[0], Effect::Limiter);
        assert_eq!(
            target.order[1],
            Effect::Suppression,
            "hidden stage stays put"
        );
        assert_eq!(target.order[2], Effect::Eq);
        let last = *target.order.last().unwrap();
        place_effect(&mut target, Effect::Limiter, last);
        assert_eq!(target.order, before, "and back");
        target.validate().unwrap();
        let mut mono = Config::default().targets[0].clone();
        split(&mut mono);
        assert!(mono.eq.right_bands.is_none());
    }
}
