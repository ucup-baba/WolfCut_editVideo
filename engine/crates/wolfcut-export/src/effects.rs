//! Laying timeline effects over the finished picture.
//!
//! A clip's own `video_effects` run inside that clip's decoder, so they last
//! exactly as long as the clip and cannot reach across a cut. A
//! [`TimelineEffect`] is the other kind: it covers a span of *timeline*, so a
//! look can run across a cut, or across part of a clip, without anyone having
//! to chop the edit up to say so. That means it cannot live in a decoder at
//! all - it belongs after compositing, where `t` is timeline time.
//!
//! # Why a blend rather than the filter itself
//!
//! Fading an effect in would be easy if filters could be ramped. Almost none
//! can: `gblur` takes a sigma, `unsharp` an amount, `noise` a strength, and
//! all of them are fixed for the life of the graph. Only `eq` and `vignette`
//! in the whole catalogue accept an expression.
//!
//! So the picture is split in two. One branch is filtered, the other is left
//! alone, and the two are blended with a weight that *is* a function of time.
//! The filter never changes; what changes is how much of it you see. That
//! gets every effect in the catalogue an ease in and out for nothing, and
//! without touching a single filter string.
//!
//! # The escape guard
//!
//! `validate_chain` forbids `;` and `[..]`, because a clip's chain is dropped
//! into a slot it must not break out of. This module builds a graph out of
//! exactly those characters - so the guard moves down a level: every fragment
//! that comes from the catalogue is still validated, and the engine composes
//! the graph around fragments it has checked. Validated at the leaf, trusted
//! only where we wrote it ourselves.

use wolfcut_project::model::{AppliedFilter, TimelineEffect};

use crate::chains::video_effect_chain;

/// The `-vf` graph for `effects`, or `None` when none of them contribute.
///
/// Effects apply in order, each seeing what the ones before it left. The
/// graph has one input and one output, so it goes to `-vf` and the encoder's
/// mapping is untouched.
pub fn effect_graph(effects: &[TimelineEffect]) -> Option<String> {
    let mut stages: Vec<String> = Vec::new();
    // The label carrying the picture into the next stage. Empty means the
    // graph's own input, which `-vf` leaves unlabelled.
    let mut carried = String::new();

    for effect in effects {
        if !effect.enabled || effect.duration <= 0.0 {
            continue;
        }
        // Through the catalogue, so a timeline effect and a clip effect of
        // the same name are the same pixels, and an id the catalogue no
        // longer knows disappears here rather than breaking the graph.
        let chain = video_effect_chain(&[AppliedFilter {
            id: effect.effect_id.clone(),
            params: effect.params.clone(),
            enabled: true,
        }]);
        if chain.is_empty() || wolfcut_media::audio::validate_chain(&chain).is_err() {
            continue;
        }

        let index = stages.len();
        let (clean, source, filtered) =
            (format!("c{index}"), format!("s{index}"), format!("f{index}"));
        let out = format!("o{index}");

        stages.push(format!(
            "{carried}split[{clean}][{source}];\
             [{source}]{chain}[{filtered}];\
             [{clean}][{filtered}]blend=all_expr='{weight}':enable='{gate}'[{out}]",
            carried = if carried.is_empty() { String::new() } else { format!("[{carried}]") },
            weight = weight_expression(effect),
            gate = gate_expression(effect),
        ));
        carried = out;
    }

    if stages.is_empty() {
        return None;
    }
    // The last stage's output label has to go: `-vf` wants the graph to end
    // unlabelled, the same way it starts.
    let mut graph = stages.join(";");
    let tail = format!("[{carried}]");
    graph.truncate(graph.len() - tail.len());
    Some(graph)
}

/// The effect chains covering `time`, each with the weight to mix it at.
///
/// The timeline graph carries its ramps as expressions in `T`, which needs
/// FFmpeg to see a stream. The monitor has one frame, so the weight is worked
/// out here instead and handed back as a number - which is also what lets the
/// filter process stay running between frames, since its chain never changes.
///
/// Empty when nothing covers `time`, which is the common case and means the
/// frame needs no filtering at all.
pub fn effect_layers_at(effects: &[TimelineEffect], time: f64) -> Vec<(String, f32)> {
    let mut layers = Vec::new();
    for effect in effects {
        if !effect.enabled || time < effect.start || time > effect.end() {
            continue;
        }
        let weight = weight_at(effect, time);
        if weight <= 0.0 {
            continue;
        }
        let chain = video_effect_chain(&[AppliedFilter {
            id: effect.effect_id.clone(),
            params: effect.params.clone(),
            enabled: true,
        }]);
        if chain.is_empty() || wolfcut_media::audio::validate_chain(&chain).is_err() {
            continue;
        }
        layers.push((chain, weight as f32));
    }
    layers
}

/// The ramp weight at one instant, in `0.0..=1.0`.
///
/// The same shape the timeline graph's expression describes, evaluated in
/// Rust instead of by FFmpeg - the two have to agree, so this is the one
/// place worth reading twice.
fn weight_at(effect: &TimelineEffect, time: f64) -> f64 {
    let rise = if effect.ease_in > 0.0 {
        ((time - effect.start) / effect.ease_in).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let fall = if effect.ease_out > 0.0 {
        ((effect.end() - time) / effect.ease_out).clamp(0.0, 1.0)
    } else {
        1.0
    };
    rise.min(fall)
}

/// How much of the filtered branch to show, as an expression in `T`.
///
/// `A` is the clean picture and `B` the filtered one, so a weight of zero is
/// the picture untouched and one is the effect at full strength. The ramps
/// are linear, matching how the rest of the app fades: `Clip::video_fade_factor`
/// is linear too, and a dissolve that eases differently from an effect would
/// be a difference nobody asked for.
fn weight_expression(effect: &TimelineEffect) -> String {
    let start = effect.start;
    let end = effect.end();
    let rise = ramp(effect.ease_in, |ease| format!("clip((T-{:.6})/{:.6},0,1)", start, ease));
    let fall = ramp(effect.ease_out, |ease| format!("clip(({:.6}-T)/{:.6},0,1)", end, ease));

    let weight = match (rise, fall) {
        (None, None) => return "B".to_owned(),
        (Some(rise), None) => rise,
        (None, Some(fall)) => fall,
        // Both ramps at once: whichever is further from full wins, which is
        // what makes an effect shorter than its eases still peak in the
        // middle instead of jumping.
        (Some(rise), Some(fall)) => format!("min({rise},{fall})"),
    };
    format!("A*(1-{weight})+B*{weight}")
}

/// A ramp expression, or `None` for a hard edge - dividing by a zero ease
/// would put a NaN through every pixel.
fn ramp(ease: f64, build: impl Fn(f64) -> String) -> Option<String> {
    (ease > 0.0).then(|| build(ease))
}

/// When the blend runs at all.
///
/// Outside the span a disabled `blend` passes its first input through, which
/// is the clean branch - so this is both the correctness gate and the reason
/// an effect costs nothing over the rest of the timeline.
fn gate_expression(effect: &TimelineEffect) -> String {
    format!("between(t,{:.6},{:.6})", effect.start, effect.end())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn effect(start: f64, duration: f64) -> TimelineEffect {
        TimelineEffect {
            id: "e1".to_owned(),
            effect_id: "gaussian-blur".to_owned(),
            params: BTreeMap::new(),
            start,
            duration,
            ease_in: 0.0,
            ease_out: 0.0,
            enabled: true,
        }
    }

    #[test]
    fn nothing_to_lay_means_no_graph() {
        assert_eq!(effect_graph(&[]), None);
        assert_eq!(effect_graph(&[TimelineEffect { enabled: false, ..effect(0.0, 2.0) }]), None);
        assert_eq!(
            effect_graph(&[TimelineEffect { effect_id: "no-such-effect".to_owned(), ..effect(0.0, 2.0) }]),
            None,
            "an id the catalogue lost disappears here rather than breaking the graph"
        );
    }

    #[test]
    fn a_hard_effect_shows_the_filtered_branch_whole() {
        let graph = effect_graph(&[effect(2.0, 3.0)]).expect("a graph");
        assert_eq!(
            graph,
            "split[c0][s0];[s0]gblur=sigma=10.0[f0];\
             [c0][f0]blend=all_expr='B':enable='between(t,2.000000,5.000000)'"
        );
    }

    #[test]
    fn an_ease_in_ramps_the_weight_from_the_effects_own_start() {
        let graph = effect_graph(&[TimelineEffect { ease_in: 1.0, ..effect(2.0, 3.0) }])
            .expect("a graph");
        assert!(
            graph.contains("all_expr='A*(1-clip((T-2.000000)/1.000000,0,1))+B*clip((T-2.000000)/1.000000,0,1)'"),
            "{graph}"
        );
    }

    #[test]
    fn an_ease_out_counts_back_from_the_end() {
        let graph = effect_graph(&[TimelineEffect { ease_out: 0.5, ..effect(2.0, 3.0) }])
            .expect("a graph");
        assert!(graph.contains("clip((5.000000-T)/0.500000,0,1)"), "{graph}");
    }

    #[test]
    fn both_eases_take_whichever_is_further_from_full() {
        let graph =
            effect_graph(&[TimelineEffect { ease_in: 1.0, ease_out: 1.0, ..effect(0.0, 4.0) }])
                .expect("a graph");
        assert!(graph.contains("min(clip((T-0.000000)/1.000000,0,1),clip((4.000000-T)/1.000000,0,1))"), "{graph}");
    }

    #[test]
    fn effects_stack_in_order_each_seeing_the_last() {
        let graph = effect_graph(&[effect(0.0, 2.0), effect(1.0, 2.0)]).expect("a graph");
        // The first stage's output is the second stage's input, and the last
        // stage ends unlabelled so `-vf` will take it.
        assert!(graph.contains("[o0]split[c1][s1]"), "{graph}");
        assert!(!graph.ends_with("[o1]"), "the graph must end unlabelled: {graph}");
        assert_eq!(graph.matches("blend=").count(), 2);
    }

    #[test]
    fn an_instant_outside_every_effect_needs_no_filtering() {
        let laid = [effect(2.0, 2.0)];
        assert!(effect_layers_at(&laid, 1.0).is_empty(), "before it");
        assert!(effect_layers_at(&laid, 5.0).is_empty(), "after it");
        assert_eq!(effect_layers_at(&laid, 3.0).len(), 1, "inside it");
    }

    #[test]
    fn an_instant_hands_back_a_weight_rather_than_an_expression() {
        let laid = [TimelineEffect { ease_in: 2.0, ..effect(0.0, 4.0) }];
        let layers = effect_layers_at(&laid, 1.0);
        // One second into a two-second ramp is half way up. The chain is the
        // plain effect, so the process running it never has to change.
        assert_eq!(layers, vec![("gblur=sigma=10.0".to_owned(), 0.5)]);
    }

    #[test]
    fn the_instant_weight_matches_the_ramp_the_timeline_graph_describes() {
        let laid = TimelineEffect { ease_in: 1.0, ease_out: 1.0, ..effect(0.0, 4.0) };
        // The corners, where the two definitions are easiest to get wrong.
        assert_eq!(weight_at(&laid, 0.0), 0.0, "nothing at the very start");
        assert_eq!(weight_at(&laid, 0.5), 0.5, "half way up");
        assert_eq!(weight_at(&laid, 2.0), 1.0, "full across the middle");
        assert_eq!(weight_at(&laid, 3.5), 0.5, "half way down");
        assert_eq!(weight_at(&laid, 4.0), 0.0, "nothing at the very end");
    }

    #[test]
    fn a_zero_weight_instant_is_no_filtering_at_all() {
        let laid = [TimelineEffect { ease_in: 1.0, ..effect(0.0, 4.0) }];
        assert!(effect_layers_at(&laid, 0.0).is_empty(), "the ramp has not started");
    }

    #[test]
    fn the_graph_starts_unlabelled_so_vf_will_take_it() {
        let graph = effect_graph(&[effect(0.0, 2.0)]).expect("a graph");
        assert!(graph.starts_with("split["), "{graph}");
    }
}
