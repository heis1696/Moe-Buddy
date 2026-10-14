use std::error::Error;

use moe_agent_runtime::animation::*;

fn node(id: &str, mode: PlaybackMode, edges: Vec<AnimationEdge>) -> AnimationNode {
    AnimationNode {
        id: id.into(),
        clip: "clip".into(),
        mode,
        edges,
    }
}

fn edge(condition: EdgeCondition, target: AnimationTarget) -> AnimationEdge {
    AnimationEdge {
        priority: 0,
        condition,
        target,
    }
}

fn to_node(condition: EdgeCondition, target: &str) -> AnimationEdge {
    edge(condition, AnimationTarget::Node(target.into()))
}

fn to_finish(condition: EdgeCondition) -> AnimationEdge {
    edge(condition, AnimationTarget::Finish)
}

fn parameter(value: bool) -> EdgeCondition {
    EdgeCondition::Parameter {
        name: "flag".into(),
        value,
    }
}

fn minimal_spec() -> AnimationSpec {
    AnimationSpec {
        version: 1,
        clips: vec![AnimationClip::new("clip", 1, 100).unwrap()],
        actions: vec![AnimationAction {
            id: "action".into(),
            entry: "entry".into(),
            exit: None,
            priority: 0,
            preemption: PreemptionPolicy::AfterExit,
            nodes: vec![node("entry", PlaybackMode::Once, vec![])],
        }],
        parameters: vec![],
        triggers: vec![],
        bindings: vec![],
    }
}

fn binding(state: &str) -> AnimationBinding {
    AnimationBinding {
        state: state.into(),
        parameter: None,
        action: "action".into(),
        priority: 0,
    }
}

fn with_exit(exit_nodes: Vec<AnimationNode>) -> AnimationSpec {
    let mut spec = minimal_spec();
    spec.actions[0].exit = Some(exit_nodes[0].id.clone());
    spec.actions[0].nodes.extend(exit_nodes);
    spec
}

fn assert_valid(spec: AnimationSpec) -> AnimationDefinition {
    assert_eq!(validate_animation(&spec), Ok(()));
    AnimationDefinition::new(spec).unwrap()
}

fn assert_invalid(spec: AnimationSpec, expected: AnimationValidationError) {
    assert_eq!(validate_animation(&spec), Err(expected.clone()));
    assert_eq!(AnimationDefinition::new(spec).unwrap_err(), expected);
}

#[test]
fn clip_retains_trusted_metadata_without_interpreting_assets() {
    let clip = AnimationClip::new("Clip_0-V2", 9, 321).unwrap();
    assert_eq!(clip.id(), "Clip_0-V2");
    assert_eq!(clip.asset_version(), 9);
    assert_eq!(clip.duration_ms(), 321);
    assert_eq!(clip.clone(), clip);
}

#[test]
fn clip_rejects_zero_asset_version() {
    assert_eq!(
        AnimationClip::new("clip", 0, 1).unwrap_err(),
        AnimationValidationError::InvalidVersion
    );
}

#[test]
fn clip_rejects_zero_duration() {
    assert_eq!(
        AnimationClip::new("clip", 1, 0).unwrap_err(),
        AnimationValidationError::InvalidDuration
    );
}

#[test]
fn identifier_limits_are_ascii_bytes_not_paths_or_unicode() {
    for identifier in [
        "",
        " ",
        "clip ",
        "clip.name",
        "clip/name",
        "clip\\name",
        "clip:name",
        "clip\n",
        "clip\0",
        "睡眠",
        "é",
        "💤",
    ] {
        assert_eq!(
            AnimationClip::new(identifier, 1, 1).unwrap_err(),
            AnimationValidationError::InvalidId,
            "{identifier:?}"
        );
    }
    assert!(AnimationClip::new("A".repeat(128), 1, 1).is_ok());
    assert_eq!(
        AnimationClip::new("A".repeat(129), 1, 1).unwrap_err(),
        AnimationValidationError::InvalidId
    );
    assert!(
        AnimationClip::new(
            "_-0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ",
            1,
            1
        )
        .is_ok()
    );
}

#[test]
fn all_identifier_namespaces_and_references_enforce_the_same_alphabet() {
    let mutations: [fn(&mut AnimationSpec); 14] = [
        |spec| spec.actions[0].id = "bad.id".into(),
        |spec| spec.actions[0].entry = "bad.id".into(),
        |spec| spec.actions[0].exit = Some("bad.id".into()),
        |spec| spec.actions[0].nodes[0].id = "bad.id".into(),
        |spec| spec.actions[0].nodes[0].clip = "bad.id".into(),
        |spec| spec.parameters[0].0 = "bad.id".into(),
        |spec| spec.triggers[0] = "bad.id".into(),
        |spec| spec.bindings[0].state = "bad.id".into(),
        |spec| spec.bindings[0].action = "bad.id".into(),
        |spec| spec.bindings[0].parameter = Some(("bad.id".into(), true)),
        |spec| {
            spec.actions[0].nodes[0].edges = vec![to_finish(EdgeCondition::Parameter {
                name: "bad.id".into(),
                value: true,
            })]
        },
        |spec| {
            spec.actions[0].nodes[0].edges =
                vec![to_finish(EdgeCondition::Trigger("bad.id".into()))]
        },
        |spec| spec.actions[0].nodes[0].edges = vec![to_node(EdgeCondition::Completed, "bad.id")],
        |spec| spec.bindings[0].state = "S".repeat(129),
    ];
    for mutate in mutations {
        let mut spec = minimal_spec();
        spec.parameters.push(("flag".into(), false));
        spec.triggers.push("go".into());
        spec.bindings.push(binding("ready"));
        mutate(&mut spec);
        assert_invalid(spec, AnimationValidationError::InvalidId);
    }
}

#[test]
fn definition_requires_a_nonzero_version() {
    let mut spec = minimal_spec();
    spec.version = 0;
    assert_invalid(spec, AnimationValidationError::InvalidVersion);
}

#[test]
fn maximum_u64_versions_and_durations_are_not_silently_restricted() {
    let mut spec = minimal_spec();
    spec.version = u64::MAX;
    spec.clips = vec![AnimationClip::new("clip", u64::MAX, u64::MAX).unwrap()];
    let definition = assert_valid(spec);
    assert_eq!(definition.version(), u64::MAX);
    assert_eq!(definition.clips()[0].duration_ms(), u64::MAX);
    assert_eq!(definition.clips()[0].asset_version(), u64::MAX);
}

#[test]
fn completely_empty_definition_is_valid() {
    let definition = assert_valid(AnimationSpec {
        version: 1,
        clips: vec![],
        actions: vec![],
        parameters: vec![],
        triggers: vec![],
        bindings: vec![],
    });
    assert_eq!(definition.version(), 1);
    assert!(definition.clips().is_empty());
    assert!(definition.actions().is_empty());
    assert!(definition.parameters().is_empty());
    assert!(definition.triggers().is_empty());
    assert!(definition.bindings().is_empty());
}

#[test]
fn empty_action_catalog_allows_unused_assets_and_signal_declarations() {
    let mut spec = minimal_spec();
    spec.actions.clear();
    spec.parameters.push(("flag".into(), false));
    spec.triggers.push("go".into());
    let definition = assert_valid(spec);
    assert!(definition.actions().is_empty());
    assert_eq!(definition.clips().len(), 1);
    assert_eq!(definition.parameters().len(), 1);
    assert_eq!(definition.triggers().len(), 1);
}

#[test]
fn declared_actions_require_nodes() {
    let mut spec = minimal_spec();
    spec.actions[0].nodes.clear();
    assert_invalid(spec, AnimationValidationError::InvalidDefinition);
}

#[test]
fn single_once_without_optional_catalogs_or_stages_is_valid() {
    let definition = assert_valid(minimal_spec());
    assert!(definition.parameters().is_empty());
    assert!(definition.triggers().is_empty());
    assert!(definition.bindings().is_empty());
    assert!(definition.actions()[0].exit.is_none());
}

#[test]
fn single_loop_without_start_or_exit_is_valid() {
    let mut spec = minimal_spec();
    spec.actions[0].nodes[0].mode = PlaybackMode::Loop;
    assert_valid(spec);
}

#[test]
fn unused_clips_are_allowed_but_a_missing_referenced_clip_is_not() {
    let mut spec = minimal_spec();
    spec.clips.push(AnimationClip::new("unused", 1, 5).unwrap());
    assert_valid(spec);
    let mut spec = minimal_spec();
    spec.clips.clear();
    assert_invalid(spec, AnimationValidationError::MissingReference);
}

#[test]
fn ids_are_unique_in_each_catalog_and_only_within_each_action_for_nodes() {
    for duplicate in 0..5 {
        let mut spec = minimal_spec();
        match duplicate {
            0 => spec.clips.push(spec.clips[0].clone()),
            1 => spec.actions.push(spec.actions[0].clone()),
            2 => {
                let duplicate = spec.actions[0].nodes[0].clone();
                spec.actions[0].nodes.push(duplicate);
            }
            3 => spec.parameters = vec![("flag".into(), false), ("flag".into(), true)],
            4 => spec.triggers = vec!["go".into(), "go".into()],
            _ => unreachable!(),
        }
        assert_invalid(spec, AnimationValidationError::DuplicateId);
    }
    let mut spec = minimal_spec();
    let mut second = spec.actions[0].clone();
    second.id = "second".into();
    spec.actions.push(second);
    assert_valid(spec);
}

#[test]
fn separate_named_catalogs_can_reuse_identifiers() {
    let mut spec = minimal_spec();
    spec.actions[0].id = "clip".into();
    spec.actions[0].entry = "clip".into();
    spec.actions[0].nodes[0].id = "clip".into();
    spec.parameters = vec![("clip".into(), true)];
    spec.triggers = vec!["clip".into()];
    spec.bindings = vec![AnimationBinding {
        state: "clip".into(),
        parameter: Some(("clip".into(), false)),
        action: "clip".into(),
        priority: 0,
    }];
    assert_valid(spec);
}

#[test]
fn multiple_bindings_and_action_reuse_do_not_require_unique_states() {
    let mut spec = minimal_spec();
    spec.parameters.push(("flag".into(), false));
    let mut conditional = binding("ready");
    conditional.parameter = Some(("flag".into(), true));
    spec.bindings = vec![
        binding("ready"),
        conditional,
        binding("other"),
        binding("ready"),
    ];
    assert_valid(spec);
}

#[test]
fn all_structural_references_must_exist_in_their_own_scope() {
    let mutations: [fn(&mut AnimationSpec); 5] = [
        |spec| spec.actions[0].entry = "missing".into(),
        |spec| spec.actions[0].exit = Some("missing".into()),
        |spec| spec.actions[0].nodes[0].clip = "missing".into(),
        |spec| spec.actions[0].nodes[0].edges = vec![to_node(EdgeCondition::Completed, "missing")],
        |spec| {
            let mut missing = binding("ready");
            missing.action = "missing".into();
            spec.bindings.push(missing);
        },
    ];
    for mutate in mutations {
        let mut spec = minimal_spec();
        mutate(&mut spec);
        assert_invalid(spec, AnimationValidationError::MissingReference);
    }
}

#[test]
fn node_edges_cannot_reference_a_node_from_another_action() {
    let mut spec = minimal_spec();
    let mut second = spec.actions[0].clone();
    second.id = "second".into();
    second.entry = "other".into();
    second.nodes[0].id = "other".into();
    spec.actions.push(second);
    spec.actions[0].nodes[0].edges = vec![to_node(EdgeCondition::Completed, "other")];
    assert_invalid(spec, AnimationValidationError::MissingReference);
}

#[test]
fn undeclared_edge_parameters_and_triggers_are_invalid_conditions() {
    for condition in [parameter(true), EdgeCondition::Trigger("go".into())] {
        let mut spec = minimal_spec();
        spec.actions[0].nodes[0].edges = vec![to_finish(condition)];
        assert_invalid(spec, AnimationValidationError::InvalidCondition);
    }
}

#[test]
fn undeclared_binding_parameters_are_invalid_conditions() {
    let mut spec = minimal_spec();
    let mut conditional = binding("ready");
    conditional.parameter = Some(("flag".into(), true));
    spec.bindings.push(conditional);
    assert_invalid(spec, AnimationValidationError::InvalidCondition);
}

#[test]
fn playback_boundary_conditions_must_match_the_node_mode() {
    for (mode, condition) in [
        (PlaybackMode::Once, EdgeCondition::LoopBoundary),
        (PlaybackMode::Loop, EdgeCondition::Completed),
    ] {
        let mut spec = minimal_spec();
        spec.actions[0].nodes[0].mode = mode;
        spec.actions[0].nodes[0].edges = vec![to_finish(condition)];
        assert_invalid(spec, AnimationValidationError::InvalidCondition);
    }
}

#[test]
fn declared_parameters_and_triggers_are_valid_for_both_playback_modes() {
    for mode in [PlaybackMode::Once, PlaybackMode::Loop] {
        let mut spec = minimal_spec();
        spec.parameters.push(("flag".into(), false));
        spec.triggers.push("go".into());
        spec.actions[0].nodes[0].mode = mode;
        spec.actions[0].nodes[0].edges = vec![
            to_finish(parameter(true)),
            to_finish(EdgeCondition::Trigger("go".into())),
        ];
        assert_valid(spec);
    }
}

#[test]
fn start_loop_exit_and_extra_intermediate_nodes_are_valid() {
    let mut spec = minimal_spec();
    spec.actions[0].nodes = vec![
        node(
            "entry",
            PlaybackMode::Once,
            vec![to_node(EdgeCondition::Completed, "middle")],
        ),
        node(
            "middle",
            PlaybackMode::Once,
            vec![to_node(EdgeCondition::Completed, "loop")],
        ),
        node("loop", PlaybackMode::Loop, vec![]),
        node(
            "exit",
            PlaybackMode::Once,
            vec![to_node(EdgeCondition::Completed, "last")],
        ),
        node(
            "last",
            PlaybackMode::Once,
            vec![to_finish(EdgeCondition::Completed)],
        ),
    ];
    spec.actions[0].exit = Some("exit".into());
    assert_valid(spec);
}

#[test]
fn ordinary_and_exit_entries_jointly_establish_reachability() {
    let spec = with_exit(vec![
        node(
            "exit",
            PlaybackMode::Once,
            vec![to_node(EdgeCondition::Completed, "last")],
        ),
        node("last", PlaybackMode::Once, vec![]),
    ]);
    assert_valid(spec);
}

#[test]
fn unreachable_nodes_outside_both_entries_are_rejected() {
    let mut spec = with_exit(vec![node("exit", PlaybackMode::Once, vec![])]);
    spec.actions[0]
        .nodes
        .push(node("stray", PlaybackMode::Once, vec![]));
    assert_invalid(spec, AnimationValidationError::UnreachableNode);
}

#[test]
fn parameter_and_trigger_self_cycles_are_rejected() {
    for condition in [parameter(true), EdgeCondition::Trigger("go".into())] {
        let mut spec = minimal_spec();
        spec.parameters.push(("flag".into(), false));
        spec.triggers.push("go".into());
        spec.actions[0].nodes[0].edges = vec![to_node(condition, "entry")];
        assert_invalid(spec, AnimationValidationError::ImmediateCycle);
    }
}

#[test]
fn mixed_parameter_trigger_cycles_are_rejected_regardless_of_current_values() {
    let mut spec = minimal_spec();
    spec.parameters.push(("flag".into(), false));
    spec.triggers.push("go".into());
    spec.actions[0].nodes = vec![
        node(
            "entry",
            PlaybackMode::Once,
            vec![to_node(parameter(true), "next")],
        ),
        node(
            "next",
            PlaybackMode::Once,
            vec![to_node(EdgeCondition::Trigger("go".into()), "entry")],
        ),
    ];
    assert_invalid(spec, AnimationValidationError::ImmediateCycle);
}

#[test]
fn contradictory_parameter_conditions_do_not_exempt_structural_cycles() {
    let mut spec = minimal_spec();
    spec.parameters.push(("flag".into(), false));
    spec.actions[0].nodes = vec![
        node(
            "entry",
            PlaybackMode::Once,
            vec![to_node(parameter(true), "next")],
        ),
        node(
            "next",
            PlaybackMode::Once,
            vec![to_node(parameter(false), "entry")],
        ),
    ];
    assert_invalid(spec, AnimationValidationError::ImmediateCycle);
}

#[test]
fn unreachable_instantaneous_cycles_are_reported_before_unreachable_nodes() {
    let mut spec = minimal_spec();
    spec.triggers.push("go".into());
    spec.actions[0].nodes.push(node(
        "stray",
        PlaybackMode::Once,
        vec![to_node(EdgeCondition::Trigger("go".into()), "stray")],
    ));
    assert_invalid(spec, AnimationValidationError::ImmediateCycle);
}

#[test]
fn instantaneous_cycles_in_later_actions_precede_earlier_reachability_errors() {
    let mut spec = minimal_spec();
    spec.triggers.push("go".into());
    let mut second = spec.actions[0].clone();
    second.id = "second".into();
    second.nodes[0].edges = vec![to_node(EdgeCondition::Trigger("go".into()), "entry")];
    spec.actions[0]
        .nodes
        .push(node("stray", PlaybackMode::Once, vec![]));
    spec.actions.push(second);
    assert_invalid(spec, AnimationValidationError::ImmediateCycle);
}

#[test]
fn positive_time_self_cycles_are_valid_in_ordinary_graphs() {
    for (mode, condition) in [
        (PlaybackMode::Once, EdgeCondition::Completed),
        (PlaybackMode::Loop, EdgeCondition::LoopBoundary),
    ] {
        let mut spec = minimal_spec();
        spec.actions[0].nodes[0].mode = mode;
        spec.actions[0].nodes[0].edges = vec![to_node(condition, "entry")];
        assert_valid(spec);
    }
}

#[test]
fn cycles_with_a_positive_time_boundary_remain_valid() {
    let mut spec = minimal_spec();
    spec.parameters.push(("flag".into(), false));
    spec.actions[0].nodes = vec![
        node(
            "entry",
            PlaybackMode::Once,
            vec![to_node(parameter(true), "next")],
        ),
        node(
            "next",
            PlaybackMode::Loop,
            vec![to_node(EdgeCondition::LoopBoundary, "entry")],
        ),
    ];
    assert_valid(spec);
}

#[test]
fn exhaustive_three_node_immediate_graphs_match_a_transitive_closure_oracle() {
    for mask in 0..512usize {
        let mut spec = minimal_spec();
        spec.parameters.push(("flag".into(), false));
        spec.triggers.push("go".into());
        let mut paths = [[false; 3]; 3];
        spec.actions[0].entry = "node_0".into();
        spec.actions[0].nodes = (0..3)
            .map(|index| node(&format!("node_{index}"), PlaybackMode::Once, vec![]))
            .collect();
        for (source, targets) in paths.iter_mut().enumerate() {
            for (target, reachable) in targets.iter_mut().enumerate() {
                if mask & (1 << (source * 3 + target)) != 0 {
                    *reachable = true;
                    let condition = if (source + target) % 2 == 0 {
                        parameter(true)
                    } else {
                        EdgeCondition::Trigger("go".into())
                    };
                    spec.actions[0].nodes[source]
                        .edges
                        .push(to_node(condition, &format!("node_{target}")));
                }
            }
        }
        for middle in 0..3 {
            for source in 0..3 {
                for target in 0..3 {
                    paths[source][target] |= paths[source][middle] && paths[middle][target];
                }
            }
        }
        let expected = if (0..3).any(|index| paths[index][index]) {
            Err(AnimationValidationError::ImmediateCycle)
        } else if (1..3).any(|index| !paths[0][index]) {
            Err(AnimationValidationError::UnreachableNode)
        } else {
            Ok(())
        };
        assert_eq!(validate_animation(&spec), expected, "graph mask {mask}");
        assert_eq!(
            AnimationDefinition::new(spec).map(|_| ()),
            expected,
            "constructor graph mask {mask}"
        );
    }
}

#[test]
fn exit_can_naturally_end_or_explicitly_finish() {
    for edges in [vec![], vec![to_finish(EdgeCondition::Completed)]] {
        assert_valid(with_exit(vec![node("exit", PlaybackMode::Once, edges)]));
    }
}

#[test]
fn ordinary_entry_can_also_be_a_valid_exit() {
    let mut spec = minimal_spec();
    spec.actions[0].exit = Some("entry".into());
    assert_valid(spec);
}

#[test]
fn exit_cannot_be_a_loop_or_reenter_an_ordinary_loop() {
    let spec = with_exit(vec![node("exit", PlaybackMode::Loop, vec![])]);
    assert_invalid(spec, AnimationValidationError::InvalidExit);
    let mut spec = with_exit(vec![node(
        "exit",
        PlaybackMode::Once,
        vec![to_node(EdgeCondition::Completed, "entry")],
    )]);
    spec.actions[0].nodes[0].mode = PlaybackMode::Loop;
    assert_invalid(spec, AnimationValidationError::InvalidExit);
}

#[test]
fn exit_cannot_depend_on_future_parameters_or_triggers_even_to_finish() {
    for condition in [parameter(true), EdgeCondition::Trigger("go".into())] {
        let mut spec = with_exit(vec![node(
            "exit",
            PlaybackMode::Once,
            vec![to_finish(condition)],
        )]);
        spec.parameters.push(("flag".into(), false));
        spec.triggers.push("go".into());
        assert_invalid(spec, AnimationValidationError::InvalidExit);
    }
}

#[test]
fn all_nodes_reachable_from_exit_must_obey_exit_restrictions() {
    let mut spec = with_exit(vec![
        node(
            "exit",
            PlaybackMode::Once,
            vec![to_node(EdgeCondition::Completed, "tail")],
        ),
        node("tail", PlaybackMode::Once, vec![to_finish(parameter(true))]),
    ]);
    spec.parameters.push(("flag".into(), false));
    assert_invalid(spec, AnimationValidationError::InvalidExit);
}

#[test]
fn exit_rejects_multiple_completed_successors_including_finish_edges() {
    let spec = with_exit(vec![node(
        "exit",
        PlaybackMode::Once,
        vec![
            to_finish(EdgeCondition::Completed),
            to_finish(EdgeCondition::Completed),
        ],
    )]);
    assert_invalid(spec, AnimationValidationError::InvalidExit);
}

#[test]
fn exit_rejects_positive_time_self_cycles_and_multinode_cycles() {
    let spec = with_exit(vec![node(
        "exit",
        PlaybackMode::Once,
        vec![to_node(EdgeCondition::Completed, "exit")],
    )]);
    assert_invalid(spec, AnimationValidationError::InvalidExit);
    let spec = with_exit(vec![
        node(
            "exit",
            PlaybackMode::Once,
            vec![to_node(EdgeCondition::Completed, "tail")],
        ),
        node(
            "tail",
            PlaybackMode::Once,
            vec![to_node(EdgeCondition::Completed, "exit")],
        ),
    ]);
    assert_invalid(spec, AnimationValidationError::InvalidExit);
}

#[test]
fn ordinary_graphs_can_reach_the_same_finite_exit_chain() {
    let mut spec = with_exit(vec![node("exit", PlaybackMode::Once, vec![])]);
    spec.actions[0].nodes[0].edges = vec![to_node(EdgeCondition::Completed, "exit")];
    assert_valid(spec);
}

#[test]
fn unreachable_positive_time_cycles_are_not_misclassified_as_immediate_cycles() {
    let mut spec = minimal_spec();
    spec.actions[0].nodes.push(node(
        "stray",
        PlaybackMode::Loop,
        vec![to_node(EdgeCondition::LoopBoundary, "stray")],
    ));
    assert_invalid(spec, AnimationValidationError::UnreachableNode);
}

#[test]
fn repeated_instantaneous_edges_without_a_cycle_are_valid() {
    let mut spec = minimal_spec();
    spec.parameters.push(("flag".into(), false));
    spec.actions[0].nodes = vec![
        node(
            "entry",
            PlaybackMode::Once,
            vec![
                to_node(parameter(true), "tail"),
                to_node(parameter(true), "tail"),
            ],
        ),
        node("tail", PlaybackMode::Once, vec![]),
    ];
    assert_valid(spec);
}

fn maximal_spec() -> AnimationSpec {
    let mut spec = minimal_spec();
    spec.clips
        .extend((1..128).map(|index| AnimationClip::new(format!("clip_{index}"), 1, 1).unwrap()));
    let prototype = spec.actions[0].clone();
    spec.actions.extend((1..128).map(|index| {
        let mut action = prototype.clone();
        action.id = format!("action_{index}");
        action
    }));
    for action in &mut spec.actions[..4] {
        action.nodes[0].edges = vec![to_finish(EdgeCondition::Completed); 128];
    }
    spec.parameters = (0..64)
        .map(|index| (format!("parameter_{index}"), false))
        .collect();
    spec.triggers = (0..64).map(|index| format!("trigger_{index}")).collect();
    spec.bindings = (0..128)
        .map(|index| binding(&format!("state_{index}")))
        .collect();
    spec
}

#[test]
fn all_capacity_limits_are_inclusive_and_signals_have_separate_catalogs() {
    let definition = assert_valid(maximal_spec());
    assert_eq!(definition.clips().len(), 128);
    assert_eq!(definition.actions().len(), 128);
    assert_eq!(
        definition
            .actions()
            .iter()
            .map(|action| action.nodes.len())
            .sum::<usize>(),
        128
    );
    assert_eq!(
        definition
            .actions()
            .iter()
            .flat_map(|action| &action.nodes)
            .map(|node| node.edges.len())
            .sum::<usize>(),
        512
    );
    assert_eq!(definition.bindings().len(), 128);
    assert_eq!(definition.parameters().len(), 64);
    assert_eq!(definition.triggers().len(), 64);
}

#[test]
fn one_over_each_capacity_is_rejected_without_truncation() {
    for overflow in 0..7 {
        let mut spec = maximal_spec();
        match overflow {
            0 => spec
                .clips
                .push(AnimationClip::new("overflow", 1, 1).unwrap()),
            1 => {
                let mut action = spec.actions[0].clone();
                action.id = "overflow".into();
                spec.actions.push(action);
            }
            2 => spec.actions[4]
                .nodes
                .push(node("overflow", PlaybackMode::Once, vec![])),
            3 => spec.actions[4].nodes[0]
                .edges
                .push(to_finish(EdgeCondition::Completed)),
            4 => spec.bindings.push(binding("overflow")),
            5 => spec.parameters.push(("overflow".into(), false)),
            6 => spec.triggers.push("overflow".into()),
            _ => unreachable!(),
        }
        assert_invalid(spec, AnimationValidationError::Capacity);
    }
}

#[test]
fn a_maximum_length_exit_chain_is_valid_without_an_arbitrary_phase_limit() {
    let mut spec = minimal_spec();
    spec.actions[0].entry = "node_0".into();
    spec.actions[0].exit = Some("node_0".into());
    spec.actions[0].nodes = (0..128)
        .map(|index| {
            let edges = if index < 127 {
                vec![to_node(
                    EdgeCondition::Completed,
                    &format!("node_{}", index + 1),
                )]
            } else {
                vec![]
            };
            node(&format!("node_{index}"), PlaybackMode::Once, edges)
        })
        .collect();
    assert_valid(spec);
}

#[test]
fn node_capacity_is_definition_wide_not_per_action() {
    let mut spec = minimal_spec();
    spec.actions[0].nodes = (0..65)
        .map(|index| node(&format!("node_{index}"), PlaybackMode::Once, vec![]))
        .collect();
    spec.actions[0].entry = "node_0".into();
    let mut second = spec.actions[0].clone();
    second.id = "second".into();
    spec.actions.push(second);
    assert_invalid(spec, AnimationValidationError::Capacity);
}

#[test]
fn edge_capacity_is_definition_wide_not_per_action() {
    let mut spec = minimal_spec();
    spec.actions[0].nodes[0].edges = vec![to_finish(EdgeCondition::Completed); 257];
    let mut second = spec.actions[0].clone();
    second.id = "second".into();
    spec.actions.push(second);
    assert_invalid(spec, AnimationValidationError::Capacity);
}

#[test]
fn publication_keeps_edge_and_binding_declaration_order_and_priority_values() {
    let mut spec = minimal_spec();
    spec.parameters.push(("flag".into(), false));
    spec.triggers.push("go".into());
    spec.actions[0].priority = i32::MIN;
    spec.actions[0].preemption = PreemptionPolicy::Immediate;
    spec.actions[0].nodes[0].edges = vec![
        AnimationEdge {
            priority: i32::MIN,
            condition: parameter(true),
            target: AnimationTarget::Finish,
        },
        AnimationEdge {
            priority: i32::MAX,
            condition: EdgeCondition::Trigger("go".into()),
            target: AnimationTarget::Finish,
        },
        AnimationEdge {
            priority: i32::MIN,
            condition: EdgeCondition::Completed,
            target: AnimationTarget::Finish,
        },
    ];
    let mut high = binding("high");
    high.priority = i32::MAX;
    spec.bindings = vec![binding("first"), high, binding("last")];
    let before = format!("{spec:?}");
    assert_eq!(validate_animation(&spec), Ok(()));
    assert_eq!(format!("{spec:?}"), before);
    let definition = AnimationDefinition::new(spec).unwrap();
    assert_eq!(
        definition.actions()[0].nodes[0]
            .edges
            .iter()
            .map(|edge| edge.priority)
            .collect::<Vec<_>>(),
        [i32::MIN, i32::MAX, i32::MIN]
    );
    assert_eq!(
        definition
            .bindings()
            .iter()
            .map(|binding| binding.state.as_str())
            .collect::<Vec<_>>(),
        ["first", "high", "last"]
    );
    assert_eq!(definition.actions()[0].priority, i32::MIN);
    assert_eq!(
        definition.actions()[0].preemption,
        PreemptionPolicy::Immediate
    );
}

#[test]
fn published_definitions_and_their_clones_do_not_follow_later_draft_edits() {
    let mut draft = minimal_spec();
    draft.parameters.push(("flag".into(), false));
    draft.triggers.push("go".into());
    draft.bindings.push(binding("ready"));
    let definition = assert_valid(draft.clone());
    let captured = definition.clone();
    draft.version = 2;
    draft.clips[0] = AnimationClip::new("clip", 2, 200).unwrap();
    draft.actions[0].nodes[0]
        .edges
        .push(to_finish(EdgeCondition::Completed));
    draft.parameters[0].1 = true;
    draft.triggers.clear();
    draft.bindings[0].state = "changed".into();
    let replacement = assert_valid(draft);
    for snapshot in [&definition, &captured] {
        assert_eq!(snapshot.version(), 1);
        assert_eq!(snapshot.clips()[0].asset_version(), 1);
        assert_eq!(snapshot.clips()[0].duration_ms(), 100);
        assert!(snapshot.actions()[0].nodes[0].edges.is_empty());
        assert_eq!(snapshot.parameters(), [("flag".into(), false)]);
        assert_eq!(snapshot.triggers(), ["go"]);
        assert_eq!(snapshot.bindings()[0].state, "ready");
    }
    assert!(std::ptr::eq(definition.actions(), captured.actions()));
    assert_eq!(replacement.version(), 2);
    assert_eq!(replacement.clips()[0].asset_version(), 2);
}

#[test]
fn validation_errors_implement_error_display_clone_and_equality() {
    for error in [
        AnimationValidationError::InvalidId,
        AnimationValidationError::InvalidVersion,
        AnimationValidationError::InvalidDuration,
        AnimationValidationError::InvalidDefinition,
        AnimationValidationError::Capacity,
        AnimationValidationError::DuplicateId,
        AnimationValidationError::MissingReference,
        AnimationValidationError::InvalidCondition,
        AnimationValidationError::UnreachableNode,
        AnimationValidationError::ImmediateCycle,
        AnimationValidationError::InvalidExit,
    ] {
        assert_eq!(error.clone(), error);
        assert!(!error.to_string().is_empty());
        assert!(error.source().is_none());
    }
}
