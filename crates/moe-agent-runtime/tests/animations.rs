use std::cell::RefCell;
use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;
use std::rc::Rc;

use moe_agent_runtime::animation::*;

#[derive(Debug)]
struct BackendCause;

impl fmt::Display for BackendCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("synthetic asset failure")
    }
}

impl Error for BackendCause {}

#[derive(Debug)]
struct BackendError {
    operation: PlaybackOperation,
    cause: BackendCause,
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "synthetic {:?} failure", self.operation)
    }
}

impl Error for BackendError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.cause)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    Prepare(String, u64, PlaybackMode),
    Start(PlaybackTicket, PlaybackMode),
    Advance(PlaybackTicket, u64),
    Pause(PlaybackTicket),
    Resume(PlaybackTicket),
    Stop(PlaybackTicket),
    Progress(PlaybackTicket),
}

impl Call {
    fn operation(&self) -> PlaybackOperation {
        match self {
            Self::Prepare(..) => PlaybackOperation::Prepare,
            Self::Start(..) => PlaybackOperation::Start,
            Self::Advance(..) => PlaybackOperation::Advance,
            Self::Pause(..) => PlaybackOperation::Pause,
            Self::Resume(..) => PlaybackOperation::Resume,
            Self::Stop(..) => PlaybackOperation::Stop,
            Self::Progress(..) => PlaybackOperation::Progress,
        }
    }
}

struct ActivePlayback {
    ticket: PlaybackTicket,
    duration_ms: u64,
    elapsed_ms: u64,
    mode: PlaybackMode,
    paused: bool,
}

struct PlayerState {
    calls: Vec<Call>,
    active: Option<ActivePlayback>,
    fail: Option<PlaybackOperation>,
    substitute: Option<(PlaybackOperation, PlaybackTicket)>,
    advance_override: Option<(u64, Option<PlaybackBoundary>)>,
    position_override: Option<(u64, bool)>,
    pause_resume: bool,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            active: None,
            fail: None,
            substitute: None,
            advance_override: None,
            position_override: None,
            pause_resume: true,
        }
    }
}

impl PlayerState {
    fn record(&mut self, call: Call) -> Result<(), BackendError> {
        let operation = call.operation();
        self.calls.push(call);
        if self.fail == Some(operation) {
            self.fail = None;
            return Err(BackendError {
                operation,
                cause: BackendCause,
            });
        }
        Ok(())
    }

    fn receipt(&mut self, operation: PlaybackOperation, ticket: &PlaybackTicket) -> PlaybackTicket {
        if self
            .substitute
            .as_ref()
            .is_some_and(|entry| entry.0 == operation)
        {
            self.substitute.take().unwrap().1
        } else {
            ticket.clone()
        }
    }
}

struct SyntheticPlayer(Rc<RefCell<PlayerState>>);

impl ClipPlayer for SyntheticPlayer {
    type Error = BackendError;

    fn prepare(
        &mut self,
        clip: &AnimationClip,
        mode: PlaybackMode,
    ) -> Result<ClipCapabilities, Self::Error> {
        let mut state = self.0.borrow_mut();
        state.record(Call::Prepare(clip.id().into(), clip.asset_version(), mode))?;
        Ok(ClipCapabilities {
            pause_resume: state.pause_resume,
        })
    }

    fn start(
        &mut self,
        clip: &AnimationClip,
        mode: PlaybackMode,
        ticket: &PlaybackTicket,
    ) -> Result<PlaybackAck, Self::Error> {
        let mut state = self.0.borrow_mut();
        assert!(
            state.active.is_none(),
            "start requires a confirmed empty channel"
        );
        state.active = Some(ActivePlayback {
            ticket: ticket.clone(),
            duration_ms: clip.duration_ms(),
            elapsed_ms: 0,
            mode,
            paused: false,
        });
        state.record(Call::Start(ticket.clone(), mode))?;
        Ok(PlaybackAck {
            ticket: state.receipt(PlaybackOperation::Start, ticket),
        })
    }

    fn advance(
        &mut self,
        ticket: &PlaybackTicket,
        delta_ms: u64,
    ) -> Result<PlaybackAdvance, Self::Error> {
        let mut state = self.0.borrow_mut();
        state.record(Call::Advance(ticket.clone(), delta_ms))?;
        let (consumed_ms, boundary) = if let Some(receipt) = state.advance_override.take() {
            receipt
        } else {
            let active = state.active.as_mut().unwrap();
            assert_eq!(&active.ticket, ticket);
            assert!(!active.paused);
            let remaining = active.duration_ms - active.elapsed_ms;
            if delta_ms < remaining {
                active.elapsed_ms += delta_ms;
                (delta_ms, None)
            } else {
                let boundary = match active.mode {
                    PlaybackMode::Once => {
                        active.elapsed_ms = active.duration_ms;
                        PlaybackBoundary::Completed
                    }
                    PlaybackMode::Loop => {
                        active.elapsed_ms = 0;
                        PlaybackBoundary::LoopBoundary
                    }
                };
                (remaining, Some(boundary))
            }
        };
        Ok(PlaybackAdvance {
            ticket: state.receipt(PlaybackOperation::Advance, ticket),
            consumed_ms,
            boundary,
        })
    }

    fn pause(&mut self, ticket: &PlaybackTicket) -> Result<PlaybackAck, Self::Error> {
        let mut state = self.0.borrow_mut();
        state.record(Call::Pause(ticket.clone()))?;
        state.active.as_mut().unwrap().paused = true;
        Ok(PlaybackAck {
            ticket: state.receipt(PlaybackOperation::Pause, ticket),
        })
    }

    fn resume(&mut self, ticket: &PlaybackTicket) -> Result<PlaybackAck, Self::Error> {
        let mut state = self.0.borrow_mut();
        state.record(Call::Resume(ticket.clone()))?;
        state.active.as_mut().unwrap().paused = false;
        Ok(PlaybackAck {
            ticket: state.receipt(PlaybackOperation::Resume, ticket),
        })
    }

    fn stop(&mut self, ticket: &PlaybackTicket) -> Result<PlaybackAck, Self::Error> {
        let mut state = self.0.borrow_mut();
        state.record(Call::Stop(ticket.clone()))?;
        state.active = None;
        Ok(PlaybackAck {
            ticket: state.receipt(PlaybackOperation::Stop, ticket),
        })
    }

    fn progress(&self, ticket: &PlaybackTicket) -> Result<PlaybackPosition, Self::Error> {
        let mut state = self.0.borrow_mut();
        state.record(Call::Progress(ticket.clone()))?;
        let (elapsed_ms, paused) = state.position_override.take().unwrap_or_else(|| {
            let active = state.active.as_ref().unwrap();
            assert_eq!(&active.ticket, ticket);
            (active.elapsed_ms, active.paused)
        });
        Ok(PlaybackPosition {
            ticket: state.receipt(PlaybackOperation::Progress, ticket),
            elapsed_ms,
            paused,
        })
    }
}

type TestAnimator = Animator<SyntheticPlayer>;
type SharedPlayer = Rc<RefCell<PlayerState>>;

fn edge(condition: EdgeCondition, target: &str, priority: i32) -> AnimationEdge {
    AnimationEdge {
        priority,
        condition,
        target: AnimationTarget::Node(target.into()),
    }
}

fn node(id: &str, mode: PlaybackMode, edges: Vec<AnimationEdge>) -> AnimationNode {
    AnimationNode {
        id: id.into(),
        clip: id.into(),
        mode,
        edges,
    }
}

fn action(
    id: &str,
    priority: i32,
    preemption: PreemptionPolicy,
    nodes: Vec<AnimationNode>,
) -> AnimationAction {
    AnimationAction {
        id: id.into(),
        entry: nodes[0].id.clone(),
        exit: None,
        priority,
        preemption,
        nodes,
    }
}

fn simple(
    id: &str,
    priority: i32,
    policy: PreemptionPolicy,
    mode: PlaybackMode,
) -> AnimationAction {
    action(
        id,
        priority,
        policy,
        vec![node(&format!("{id}-node"), mode, vec![])],
    )
}

fn staged() -> AnimationAction {
    let mut result = action(
        "main",
        10,
        PreemptionPolicy::AfterExit,
        vec![
            node(
                "opening",
                PlaybackMode::Once,
                vec![edge(EdgeCondition::Completed, "looping", 0)],
            ),
            node(
                "looping",
                PlaybackMode::Loop,
                vec![edge(EdgeCondition::LoopBoundary, "looping", 0)],
            ),
            node(
                "cleanup",
                PlaybackMode::Once,
                vec![edge(EdgeCondition::Completed, "tail", 0)],
            ),
            node("tail", PlaybackMode::Once, vec![]),
        ],
    );
    result.exit = Some("cleanup".into());
    result
}

fn binding(
    state: &str,
    action: &str,
    priority: i32,
    parameter: Option<(&str, bool)>,
) -> AnimationBinding {
    AnimationBinding {
        state: state.into(),
        parameter: parameter.map(|(name, value)| (name.into(), value)),
        action: action.into(),
        priority,
    }
}

fn spec(actions: Vec<AnimationAction>) -> AnimationSpec {
    let ids: BTreeSet<_> = actions
        .iter()
        .flat_map(|action| action.nodes.iter().map(|node| node.clip.clone()))
        .collect();
    AnimationSpec {
        version: 1,
        clips: ids
            .into_iter()
            .map(|id| AnimationClip::new(id, 1, 10).unwrap())
            .collect(),
        actions,
        parameters: vec![("flag".into(), false), ("other".into(), false)],
        triggers: vec!["ping".into(), "pong".into()],
        bindings: vec![],
    }
}

fn preemption_spec() -> AnimationSpec {
    let mut definition = spec(vec![
        staged(),
        simple("low", 5, PreemptionPolicy::Immediate, PlaybackMode::Loop),
        simple("peer", 10, PreemptionPolicy::Immediate, PlaybackMode::Loop),
        simple("high", 20, PreemptionPolicy::Immediate, PlaybackMode::Loop),
        simple(
            "delayed",
            20,
            PreemptionPolicy::AfterExit,
            PlaybackMode::Loop,
        ),
    ]);
    definition.bindings = vec![
        binding("main-state", "main", 0, None),
        binding("same-main", "main", 0, None),
        binding("low-state", "low", 0, None),
        binding("peer-state", "peer", 0, None),
        binding("high-state", "high", 0, None),
    ];
    definition
}

fn rig(spec: AnimationSpec) -> (TestAnimator, SharedPlayer) {
    let state = Rc::new(RefCell::new(PlayerState::default()));
    let player = SyntheticPlayer(Rc::clone(&state));
    (
        Animator::new(AnimationDefinition::new(spec).unwrap(), player),
        state,
    )
}

fn current(animator: &TestAnimator, action: &str, node: &str) {
    let snapshot = animator.snapshot();
    assert_eq!(snapshot.action.as_deref(), Some(action));
    assert_eq!(snapshot.node.as_deref(), Some(node));
}

fn starts(player: &SharedPlayer) -> Vec<String> {
    player
        .borrow()
        .calls
        .iter()
        .filter_map(|call| match call {
            Call::Start(ticket, _) => Some(ticket.clip().to_owned()),
            _ => None,
        })
        .collect()
}

fn active_ticket(animator: &TestAnimator) -> PlaybackTicket {
    animator.snapshot().ticket.unwrap()
}

fn enter_phase(animator: &mut TestAnimator, phase: &str) {
    animator.sync_state("main-state").unwrap();
    match phase {
        "opening" => {}
        "looping" => {
            animator.advance(10).unwrap();
        }
        "cleanup" => {
            animator.request_exit().unwrap();
            animator.advance(10).unwrap();
        }
        _ => panic!("unknown fixture phase"),
    }
    current(animator, "main", phase);
}

#[test]
fn composed_opening_loop_and_multisegment_exit_follow_declared_order() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    animator.advance(10).unwrap();
    current(&animator, "main", "looping");
    animator.advance(20).unwrap();
    animator.request_exit().unwrap();
    animator.advance(10).unwrap();
    current(&animator, "main", "cleanup");
    assert!(animator.snapshot().exiting);
    animator.advance(10).unwrap();
    current(&animator, "main", "tail");
    assert_eq!(animator.advance(10).unwrap().status, AnimationStatus::Idle);
    assert_eq!(
        starts(&player),
        [
            "opening", "looping", "looping", "looping", "cleanup", "tail"
        ]
    );
}

#[test]
fn optional_opening_loop_and_exit_are_not_required() {
    for mode in [PlaybackMode::Once, PlaybackMode::Loop] {
        let (mut animator, player) = rig(spec(vec![simple(
            "bare",
            0,
            PreemptionPolicy::AfterExit,
            mode,
        )]));
        animator.request_action("bare").unwrap();
        if mode == PlaybackMode::Loop {
            animator.request_exit().unwrap();
        }
        assert_eq!(animator.advance(10).unwrap().status, AnimationStatus::Idle);
        assert_eq!(starts(&player), ["bare-node"]);
    }
}

#[test]
fn leftover_time_moves_once_to_each_successor_and_not_twice() {
    let definition = spec(vec![action(
        "chain",
        0,
        PreemptionPolicy::AfterExit,
        vec![
            node(
                "first",
                PlaybackMode::Once,
                vec![edge(EdgeCondition::Completed, "second", 0)],
            ),
            node(
                "second",
                PlaybackMode::Once,
                vec![edge(EdgeCondition::Completed, "third", 0)],
            ),
            node("third", PlaybackMode::Once, vec![]),
        ],
    )]);
    let (mut animator, player) = rig(definition);
    animator.request_action("chain").unwrap();
    let result = animator.advance(25).unwrap();
    assert_eq!(result.pending_ms, 0);
    assert_eq!(result.processed, 2);
    current(&animator, "chain", "third");
    assert_eq!(animator.playback_progress().unwrap().unwrap().elapsed_ms, 5);
    let deltas: Vec<_> = player
        .borrow()
        .calls
        .iter()
        .filter_map(|call| match call {
            Call::Advance(_, delta) => Some(*delta),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, [25, 15, 5]);
}

#[test]
fn loop_without_matching_edge_keeps_the_same_playback_ticket() {
    let (mut animator, player) = rig(spec(vec![simple(
        "loop",
        0,
        PreemptionPolicy::AfterExit,
        PlaybackMode::Loop,
    )]));
    animator.request_action("loop").unwrap();
    let ticket = active_ticket(&animator);
    assert_eq!(animator.advance(35).unwrap().processed, 3);
    assert_eq!(active_ticket(&animator), ticket);
    assert_eq!(animator.playback_progress().unwrap().unwrap().elapsed_ms, 5);
    assert_eq!(starts(&player), ["loop-node"]);
}

#[test]
fn positive_time_cycles_are_valid_and_create_new_playback_tickets() {
    let definition = spec(vec![action(
        "cycle",
        0,
        PreemptionPolicy::AfterExit,
        vec![
            node(
                "first",
                PlaybackMode::Once,
                vec![edge(EdgeCondition::Completed, "second", 0)],
            ),
            node(
                "second",
                PlaybackMode::Once,
                vec![edge(EdgeCondition::Completed, "first", 0)],
            ),
        ],
    )]);
    let (mut animator, player) = rig(definition);
    animator.request_action("cycle").unwrap();
    animator.advance(40).unwrap();
    assert_eq!(
        starts(&player),
        ["first", "second", "first", "second", "first"]
    );
    assert_eq!(active_ticket(&animator).sequence(), 5);
}

#[test]
fn bindings_select_highest_priority_before_declaration_order() {
    let mut definition = preemption_spec();
    definition.bindings = vec![
        binding("state", "low", 0, None),
        binding("state", "high", 20, None),
        binding("state", "peer", 10, None),
    ];
    let (mut animator, _) = rig(definition);
    animator.sync_state("state").unwrap();
    current(&animator, "high", "high-node");
}

#[test]
fn equal_priority_bindings_choose_first_declaration() {
    let mut definition = preemption_spec();
    definition.bindings = vec![
        binding("state", "peer", 20, None),
        binding("state", "high", 20, None),
    ];
    let (mut animator, _) = rig(definition);
    animator.sync_state("state").unwrap();
    current(&animator, "peer", "peer-node");
}

#[test]
fn parameter_change_reselects_variant_but_identical_value_does_not_replay() {
    let mut definition = preemption_spec();
    definition.bindings = vec![
        binding("state", "main", 0, Some(("flag", false))),
        binding("state", "high", 0, Some(("flag", true))),
    ];
    let (mut animator, player) = rig(definition);
    animator.sync_state("state").unwrap();
    animator.set_parameter("flag", true).unwrap();
    current(&animator, "high", "high-node");
    let snapshot = animator.snapshot();
    animator.set_parameter("flag", true).unwrap();
    animator.sync_state("state").unwrap();
    assert_eq!(animator.snapshot(), snapshot);
    assert_eq!(starts(&player), ["opening", "high-node"]);
}

#[test]
fn different_states_can_reuse_current_action_without_restarting() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    let ticket = active_ticket(&animator);
    animator.sync_state("same-main").unwrap();
    animator.sync_state("same-main").unwrap();
    assert_eq!(active_ticket(&animator), ticket);
    assert!(!animator.snapshot().exit_requested);
    assert_eq!(starts(&player), ["opening"]);
}

#[test]
fn natural_finish_does_not_replay_on_repeated_state_or_parameter_sync() {
    let mut definition = spec(vec![simple(
        "once",
        0,
        PreemptionPolicy::AfterExit,
        PlaybackMode::Once,
    )]);
    definition.bindings = vec![
        binding("state", "once", 0, None),
        binding("alias", "once", 0, None),
    ];
    let (mut animator, player) = rig(definition);
    animator.sync_state("state").unwrap();
    animator.advance(10).unwrap();
    animator.sync_state("state").unwrap();
    animator.sync_state("alias").unwrap();
    animator.set_parameter("flag", true).unwrap();
    animator.set_parameter("flag", true).unwrap();
    assert!(animator.snapshot().action.is_none());
    assert_eq!(starts(&player), ["once-node"]);
    animator.request_action("once").unwrap();
    assert_eq!(starts(&player), ["once-node", "once-node"]);
}

#[test]
fn repeated_state_selection_preserves_explicit_candidate() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    animator.request_action("peer").unwrap();
    let snapshot = animator.snapshot();
    animator.sync_state("main-state").unwrap();
    animator.set_parameter("other", true).unwrap();
    assert_eq!(animator.snapshot(), snapshot);
    assert_eq!(starts(&player), ["opening"]);
    animator.advance(30).unwrap();
    current(&animator, "peer", "peer-node");
}

#[test]
fn genuine_state_selection_overwrites_explicit_candidate_even_with_lower_priority() {
    let (mut animator, _) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    animator.request_action("peer").unwrap();
    animator.sync_state("low-state").unwrap();
    assert_eq!(animator.snapshot().candidate.as_deref(), Some("low"));
    animator.advance(30).unwrap();
    current(&animator, "low", "low-node");
}

#[test]
fn reselecting_current_action_before_cleanup_revokes_unstarted_exit() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    animator.request_action("peer").unwrap();
    animator.sync_state("same-main").unwrap();
    assert!(!animator.snapshot().exit_requested);
    assert!(animator.snapshot().candidate.is_none());
    animator.advance(10).unwrap();
    current(&animator, "main", "looping");
    assert_eq!(starts(&player), ["opening", "looping"]);
}

#[test]
fn reselecting_current_action_in_cleanup_queues_it_without_truncation() {
    let (mut animator, player) = rig(preemption_spec());
    enter_phase(&mut animator, "cleanup");
    animator.request_action("peer").unwrap();
    animator.sync_state("same-main").unwrap();
    assert_eq!(animator.snapshot().candidate.as_deref(), Some("main"));
    current(&animator, "main", "cleanup");
    animator.advance(20).unwrap();
    current(&animator, "main", "opening");
    assert_eq!(starts(&player), ["opening", "cleanup", "tail", "opening"]);
}

#[test]
fn state_revocation_cannot_be_blocked_by_old_action_priority() {
    let (mut animator, player) = rig(preemption_spec());
    enter_phase(&mut animator, "looping");
    animator.sync_state("unbound").unwrap();
    assert!(animator.snapshot().exit_requested);
    assert!(animator.snapshot().candidate.is_none());
    assert_eq!(animator.advance(30).unwrap().status, AnimationStatus::Idle);
    assert_eq!(starts(&player), ["opening", "looping", "cleanup", "tail"]);
}

#[test]
fn normal_exit_wins_over_completed_loop_and_instantaneous_edges() {
    let mut main = staged();
    main.nodes[0]
        .edges
        .push(edge(EdgeCondition::Trigger("ping".into()), "trap", 100));
    main.nodes.push(node("trap", PlaybackMode::Once, vec![]));
    let (mut animator, player) = rig(spec(vec![main]));
    animator.request_action("main").unwrap();
    animator.request_exit().unwrap();
    animator.trigger("ping").unwrap();
    current(&animator, "main", "opening");
    animator.advance(10).unwrap();
    current(&animator, "main", "cleanup");
    assert_eq!(starts(&player), ["opening", "cleanup"]);
}

#[test]
fn explicit_preemption_matrix_applies_in_opening_loop_and_cleanup() {
    for phase in ["opening", "looping", "cleanup"] {
        for requested in ["low", "peer", "high", "delayed"] {
            let (mut animator, player) = rig(preemption_spec());
            enter_phase(&mut animator, phase);
            let before = animator.snapshot();
            let calls = player.borrow().calls.clone();
            let result = animator.request_action(requested).unwrap();
            if requested == "low" {
                assert_eq!(result, AnimationRequest::RejectedLowerPriority);
                assert_eq!(animator.snapshot(), before);
                assert_eq!(player.borrow().calls, calls);
            } else if requested == "high" {
                assert_eq!(result, AnimationRequest::Accepted);
                current(&animator, "high", "high-node");
                assert!(!animator.snapshot().exiting);
            } else {
                assert_eq!(result, AnimationRequest::Accepted);
                current(&animator, "main", phase);
                let delay = if phase == "cleanup" { 20 } else { 30 };
                animator.advance(delay - 1).unwrap();
                current(&animator, "main", "tail");
                animator.advance(1).unwrap();
                current(&animator, requested, &format!("{requested}-node"));
            }
        }
    }
}

#[test]
fn high_priority_state_selection_can_preempt_each_phase_immediately() {
    for phase in ["opening", "looping", "cleanup"] {
        let (mut animator, _) = rig(preemption_spec());
        enter_phase(&mut animator, phase);
        animator.sync_state("high-state").unwrap();
        current(&animator, "high", "high-node");
        assert!(!animator.snapshot().exiting);
    }
}

#[test]
fn latest_accepted_candidate_wins_without_restarting_cleanup() {
    let (mut animator, player) = rig(preemption_spec());
    enter_phase(&mut animator, "cleanup");
    animator.request_action("peer").unwrap();
    animator.advance(3).unwrap();
    animator.request_action("delayed").unwrap();
    assert_eq!(animator.snapshot().candidate.as_deref(), Some("delayed"));
    animator.advance(17).unwrap();
    current(&animator, "delayed", "delayed-node");
    assert_eq!(
        starts(&player),
        ["opening", "cleanup", "tail", "delayed-node"]
    );
}

#[test]
fn repeated_exit_while_cleaning_up_neither_reenters_nor_truncates_chain() {
    let (mut animator, player) = rig(preemption_spec());
    enter_phase(&mut animator, "cleanup");
    for _ in 0..5 {
        animator.request_exit().unwrap();
        animator.advance(2).unwrap();
    }
    current(&animator, "main", "tail");
    animator.request_exit().unwrap();
    assert_eq!(animator.advance(10).unwrap().status, AnimationStatus::Idle);
    assert_eq!(starts(&player), ["opening", "cleanup", "tail"]);
}

#[test]
fn natural_entry_into_exit_subgraph_obeys_cleanup_rules() {
    for requested in [None, Some("peer"), Some("delayed")] {
        let mut definition = preemption_spec();
        definition.actions[0].nodes[1].edges[0].target = AnimationTarget::Node("cleanup".into());
        let (mut animator, player) = rig(definition);
        animator.sync_state("main-state").unwrap();
        animator.advance(20).unwrap();
        current(&animator, "main", "cleanup");
        assert!(animator.snapshot().exiting);
        if let Some(requested) = requested {
            animator.request_action(requested).unwrap();
        } else {
            animator.request_exit().unwrap();
        }
        animator.advance(10).unwrap();
        current(&animator, "main", "tail");
        animator.advance(10).unwrap();
        assert_eq!(
            &starts(&player)[..4],
            ["opening", "looping", "cleanup", "tail"]
        );
        assert_eq!(animator.snapshot().action.as_deref(), requested);
    }
}

#[test]
fn lower_priority_request_preserves_existing_candidate_and_backend() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    animator.request_action("peer").unwrap();
    let snapshot = animator.snapshot();
    let calls = player.borrow().calls.clone();
    assert_eq!(
        animator.request_action("low").unwrap(),
        AnimationRequest::RejectedLowerPriority
    );
    assert_eq!(animator.snapshot(), snapshot);
    assert_eq!(player.borrow().calls, calls);
}

#[test]
fn explicit_same_action_request_replays_only_after_normal_exit() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    let ticket = active_ticket(&animator);
    animator.request_action("main").unwrap();
    assert_eq!(active_ticket(&animator), ticket);
    animator.advance(30).unwrap();
    current(&animator, "main", "opening");
    assert_ne!(active_ticket(&animator), ticket);
    assert_eq!(starts(&player), ["opening", "cleanup", "tail", "opening"]);
}

#[test]
fn repeated_pending_trigger_merges_and_is_consumed_only_once() {
    let definition = spec(vec![action(
        "signals",
        0,
        PreemptionPolicy::AfterExit,
        vec![
            node(
                "entry",
                PlaybackMode::Loop,
                vec![edge(EdgeCondition::Trigger("ping".into()), "middle", 0)],
            ),
            node(
                "middle",
                PlaybackMode::Loop,
                vec![edge(EdgeCondition::Trigger("ping".into()), "last", 0)],
            ),
            node("last", PlaybackMode::Loop, vec![]),
        ],
    )]);
    let (mut animator, player) = rig(definition);
    animator.request_action("signals").unwrap();
    animator.pause().unwrap();
    animator.trigger("ping").unwrap();
    animator.trigger("ping").unwrap();
    animator.resume().unwrap();
    current(&animator, "signals", "middle");
    assert_eq!(starts(&player), ["entry", "middle"]);
    animator.advance(0).unwrap();
    current(&animator, "signals", "middle");
    animator.trigger("ping").unwrap();
    current(&animator, "signals", "last");
}

#[test]
fn parameter_edge_winner_does_not_consume_losing_trigger() {
    let definition = spec(vec![action(
        "signals",
        0,
        PreemptionPolicy::AfterExit,
        vec![
            node(
                "entry",
                PlaybackMode::Loop,
                vec![
                    edge(EdgeCondition::Trigger("ping".into()), "trap", 1),
                    edge(
                        EdgeCondition::Parameter {
                            name: "flag".into(),
                            value: true,
                        },
                        "middle",
                        10,
                    ),
                ],
            ),
            node(
                "middle",
                PlaybackMode::Loop,
                vec![edge(EdgeCondition::Trigger("ping".into()), "last", 0)],
            ),
            node("trap", PlaybackMode::Loop, vec![]),
            node("last", PlaybackMode::Loop, vec![]),
        ],
    )]);
    let (mut animator, player) = rig(definition);
    animator.request_action("signals").unwrap();
    animator.pause().unwrap();
    animator.trigger("ping").unwrap();
    animator.set_parameter("flag", true).unwrap();
    animator.resume().unwrap();
    current(&animator, "signals", "last");
    assert_eq!(starts(&player), ["entry", "middle", "last"]);
}

#[test]
fn only_winning_trigger_is_consumed_and_other_pending_trigger_survives() {
    let definition = spec(vec![action(
        "signals",
        0,
        PreemptionPolicy::AfterExit,
        vec![
            node(
                "entry",
                PlaybackMode::Loop,
                vec![
                    edge(EdgeCondition::Trigger("pong".into()), "trap", 1),
                    edge(EdgeCondition::Trigger("ping".into()), "middle", 10),
                ],
            ),
            node(
                "middle",
                PlaybackMode::Loop,
                vec![edge(EdgeCondition::Trigger("pong".into()), "last", 0)],
            ),
            node("trap", PlaybackMode::Loop, vec![]),
            node("last", PlaybackMode::Loop, vec![]),
        ],
    )]);
    let (mut animator, player) = rig(definition);
    animator.request_action("signals").unwrap();
    animator.pause().unwrap();
    animator.trigger("pong").unwrap();
    animator.trigger("ping").unwrap();
    animator.resume().unwrap();
    assert_eq!(starts(&player), ["entry", "middle", "last"]);
}

#[test]
fn ordinary_edge_ties_are_broken_by_declaration_not_trigger_arrival() {
    let definition = spec(vec![action(
        "signals",
        0,
        PreemptionPolicy::AfterExit,
        vec![
            node(
                "entry",
                PlaybackMode::Loop,
                vec![
                    edge(EdgeCondition::Trigger("ping".into()), "first", 10),
                    edge(EdgeCondition::Trigger("pong".into()), "second", 10),
                ],
            ),
            node("first", PlaybackMode::Loop, vec![]),
            node("second", PlaybackMode::Loop, vec![]),
        ],
    )]);
    let (mut animator, _) = rig(definition);
    animator.request_action("signals").unwrap();
    animator.pause().unwrap();
    animator.trigger("pong").unwrap();
    animator.trigger("ping").unwrap();
    animator.resume().unwrap();
    current(&animator, "signals", "first");
}

fn trigger_target_spec() -> AnimationSpec {
    let mut definition = preemption_spec();
    definition.actions[3].nodes[0].edges.push(edge(
        EdgeCondition::Trigger("ping".into()),
        "high-hit",
        0,
    ));
    definition.actions[3]
        .nodes
        .push(node("high-hit", PlaybackMode::Loop, vec![]));
    definition
        .clips
        .push(AnimationClip::new("high-hit", 1, 10).unwrap());
    definition
}

#[test]
fn replacing_action_clears_triggers_received_for_previous_action() {
    let (mut animator, player) = rig(trigger_target_spec());
    animator.sync_state("main-state").unwrap();
    animator.pause().unwrap();
    animator.trigger("ping").unwrap();
    animator.request_action("high").unwrap();
    animator.resume().unwrap();
    current(&animator, "high", "high-node");
    assert_eq!(starts(&player), ["opening", "high-node"]);
    animator.trigger("ping").unwrap();
    current(&animator, "high", "high-hit");
}

#[test]
fn trigger_received_while_idle_cannot_be_consumed_by_future_action() {
    let (mut animator, _) = rig(trigger_target_spec());
    animator.trigger("ping").unwrap();
    animator.request_action("high").unwrap();
    current(&animator, "high", "high-node");
}

#[test]
fn pause_freezes_cursor_rejects_positive_time_and_is_idempotent() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    animator.advance(3).unwrap();
    animator.pause().unwrap();
    let snapshot = animator.snapshot();
    let calls = player.borrow().calls.clone();
    animator.pause().unwrap();
    assert_eq!(animator.advance(0).unwrap().status, AnimationStatus::Paused);
    assert!(matches!(animator.advance(100), Err(AnimationError::Paused)));
    assert_eq!(animator.snapshot(), snapshot);
    assert_eq!(player.borrow().calls, calls);
    let progress = animator.playback_progress().unwrap().unwrap();
    assert_eq!(progress.elapsed_ms, 3);
    assert!(progress.paused);
    animator.resume().unwrap();
    let calls = player.borrow().calls.clone();
    animator.resume().unwrap();
    assert_eq!(player.borrow().calls, calls);
    animator.advance(6).unwrap();
    current(&animator, "main", "opening");
    animator.advance(1).unwrap();
    current(&animator, "main", "looping");
}

#[test]
fn explicit_immediate_request_while_paused_only_queues_until_resume() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    animator.pause().unwrap();
    let calls = player.borrow().calls.clone();
    animator.request_action("high").unwrap();
    current(&animator, "main", "opening");
    assert_eq!(animator.snapshot().candidate.as_deref(), Some("high"));
    assert_eq!(player.borrow().calls, calls);
    animator.resume().unwrap();
    current(&animator, "high", "high-node");
    assert_eq!(starts(&player), ["opening", "high-node"]);
}

#[test]
fn state_parameter_and_trigger_updates_while_paused_have_no_backend_effects() {
    let mut definition = trigger_target_spec();
    definition
        .bindings
        .push(binding("variant", "main", 0, Some(("flag", false))));
    definition
        .bindings
        .push(binding("variant", "high", 0, Some(("flag", true))));
    let (mut animator, player) = rig(definition);
    animator.sync_state("main-state").unwrap();
    animator.pause().unwrap();
    let calls = player.borrow().calls.clone();
    animator.sync_state("variant").unwrap();
    animator.set_parameter("flag", true).unwrap();
    animator.trigger("ping").unwrap();
    assert_eq!(animator.parameter("flag"), Some(true));
    assert_eq!(animator.snapshot().state.as_deref(), Some("variant"));
    assert_eq!(animator.snapshot().candidate.as_deref(), Some("high"));
    assert_eq!(player.borrow().calls, calls);
    animator.resume().unwrap();
    current(&animator, "high", "high-node");
}

#[test]
fn pause_without_backend_capability_is_side_effect_free() {
    let (mut animator, player) = rig(preemption_spec());
    player.borrow_mut().pause_resume = false;
    animator.sync_state("main-state").unwrap();
    let snapshot = animator.snapshot();
    let calls = player.borrow().calls.clone();
    assert!(matches!(
        animator.pause(),
        Err(AnimationError::PauseUnsupported)
    ));
    assert_eq!(animator.snapshot(), snapshot);
    assert_eq!(player.borrow().calls, calls);
}

#[test]
fn idle_pause_defers_new_playback_and_stop_clears_pause_without_backend_calls() {
    let (mut animator, player) = rig(preemption_spec());
    animator.pause().unwrap();
    animator.request_action("main").unwrap();
    assert!(player.borrow().calls.is_empty());
    assert!(animator.snapshot().action.is_none());
    animator.stop().unwrap();
    assert!(!animator.snapshot().paused);
    assert!(animator.snapshot().candidate.is_none());
    assert!(player.borrow().calls.is_empty());
    animator.pause().unwrap();
    animator.request_action("main").unwrap();
    animator.resume().unwrap();
    assert_eq!(starts(&player), ["opening"]);
}

#[test]
fn forced_stop_while_paused_in_cleanup_bypasses_remaining_exit_and_is_idempotent() {
    let (mut animator, player) = rig(preemption_spec());
    enter_phase(&mut animator, "cleanup");
    animator.pause().unwrap();
    animator.request_action("high").unwrap();
    animator.stop().unwrap();
    let snapshot = animator.snapshot();
    let calls = player.borrow().calls.clone();
    animator.stop().unwrap();
    assert_eq!(animator.snapshot(), snapshot);
    assert_eq!(player.borrow().calls, calls);
    assert!(!snapshot.paused);
    assert!(!snapshot.exiting);
    assert!(snapshot.action.is_none());
    assert!(snapshot.candidate.is_none());
    assert_eq!(starts(&player), ["opening", "cleanup"]);
}

#[test]
fn invalid_commands_leave_snapshot_backend_and_declared_parameters_unchanged() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    animator.request_action("peer").unwrap();
    let snapshot = animator.snapshot();
    let calls = player.borrow().calls.clone();
    assert!(matches!(
        animator.request_action("missing"),
        Err(AnimationError::UnknownAction)
    ));
    assert!(matches!(
        animator.set_parameter("missing", true),
        Err(AnimationError::UnknownParameter)
    ));
    assert!(matches!(
        animator.trigger("missing"),
        Err(AnimationError::UnknownTrigger)
    ));
    for state in ["", "bad/path", "non ascii", &"x".repeat(129)] {
        assert!(matches!(
            animator.sync_state(state),
            Err(AnimationError::Invalid(_))
        ));
    }
    assert_eq!(animator.parameter("flag"), Some(false));
    assert_eq!(animator.parameter("missing"), None);
    assert_eq!(animator.snapshot(), snapshot);
    assert_eq!(player.borrow().calls, calls);
}

fn invoke_operation(
    animator: &mut TestAnimator,
    operation: PlaybackOperation,
) -> Result<(), AnimationError<BackendError>> {
    match operation {
        PlaybackOperation::Prepare | PlaybackOperation::Start => {
            animator.request_action("high").map(|_| ())
        }
        PlaybackOperation::Advance => animator.advance(1).map(|_| ()),
        PlaybackOperation::Pause => animator.pause(),
        PlaybackOperation::Resume => animator.resume(),
        PlaybackOperation::Stop => animator.stop(),
        PlaybackOperation::Progress => animator.playback_progress().map(|_| ()),
    }
}

fn assert_fault_locked(animator: &mut TestAnimator, player: &SharedPlayer) {
    assert!(animator.snapshot().faulted);
    let snapshot = animator.snapshot();
    let calls = player.borrow().calls.clone();
    assert!(matches!(
        animator.request_action("high"),
        Err(AnimationError::Faulted)
    ));
    assert!(matches!(
        animator.sync_state("low-state"),
        Err(AnimationError::Faulted)
    ));
    assert!(matches!(
        animator.set_parameter("flag", true),
        Err(AnimationError::Faulted)
    ));
    assert!(matches!(
        animator.trigger("ping"),
        Err(AnimationError::Faulted)
    ));
    assert!(matches!(
        animator.request_exit(),
        Err(AnimationError::Faulted)
    ));
    assert!(matches!(animator.advance(0), Err(AnimationError::Faulted)));
    assert!(matches!(animator.pause(), Err(AnimationError::Faulted)));
    assert!(matches!(animator.resume(), Err(AnimationError::Faulted)));
    assert!(matches!(
        animator.playback_progress(),
        Err(AnimationError::Faulted)
    ));
    assert_eq!(animator.snapshot(), snapshot);
    assert_eq!(player.borrow().calls, calls);
}

#[test]
fn every_backend_failure_retains_concrete_error_chain_and_requires_explicit_stop() {
    for operation in [
        PlaybackOperation::Prepare,
        PlaybackOperation::Start,
        PlaybackOperation::Advance,
        PlaybackOperation::Pause,
        PlaybackOperation::Resume,
        PlaybackOperation::Stop,
        PlaybackOperation::Progress,
    ] {
        let (mut animator, player) = rig(preemption_spec());
        animator.sync_state("main-state").unwrap();
        if operation == PlaybackOperation::Resume {
            animator.pause().unwrap();
        }
        player.borrow_mut().fail = Some(operation);
        let error = invoke_operation(&mut animator, operation).unwrap_err();
        assert!(
            matches!(&error, AnimationError::Backend { operation: actual, source } if *actual == operation && source.operation == operation)
        );
        assert!(error.to_string().contains("synthetic"));
        let source = error.source().unwrap();
        assert!(source.downcast_ref::<BackendError>().is_some());
        assert!(
            source
                .source()
                .unwrap()
                .downcast_ref::<BackendCause>()
                .is_some()
        );
        assert_fault_locked(&mut animator, &player);
        animator.stop().unwrap();
        assert!(!animator.snapshot().faulted);
        assert!(!animator.snapshot().paused);
        assert!(animator.snapshot().action.is_none());
        assert!(player.borrow().active.is_none());
        animator.request_action("high").unwrap();
        current(&animator, "high", "high-node");
    }
}

#[test]
fn failed_prepare_does_not_stop_or_replace_old_playback() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    animator.advance(3).unwrap();
    let ticket = active_ticket(&animator);
    let offset = player.borrow().calls.len();
    player.borrow_mut().fail = Some(PlaybackOperation::Prepare);
    assert!(matches!(
        animator.request_action("high"),
        Err(AnimationError::Backend {
            operation: PlaybackOperation::Prepare,
            ..
        })
    ));
    assert_eq!(active_ticket(&animator), ticket);
    let state = player.borrow();
    assert_eq!(state.active.as_ref().unwrap().ticket, ticket);
    assert_eq!(state.active.as_ref().unwrap().elapsed_ms, 3);
    assert!(matches!(&state.calls[offset..], [Call::Prepare(_, _, _)]));
}

#[test]
fn failed_stop_does_not_start_replacement_or_automatically_retry() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    let ticket = active_ticket(&animator);
    player.borrow_mut().fail = Some(PlaybackOperation::Stop);
    assert!(animator.request_action("high").is_err());
    assert_eq!(starts(&player), ["opening"]);
    assert_eq!(player.borrow().active.as_ref().unwrap().ticket, ticket);
    assert_fault_locked(&mut animator, &player);
    player.borrow_mut().fail = Some(PlaybackOperation::Stop);
    assert!(animator.stop().is_err());
    assert_fault_locked(&mut animator, &player);
    animator.stop().unwrap();
    animator.request_action("high").unwrap();
    assert_eq!(starts(&player), ["opening", "high-node"]);
}

#[test]
fn every_ticket_bearing_backend_operation_rejects_foreign_receipts() {
    let (mut foreign, _) = rig(preemption_spec());
    foreign.sync_state("main-state").unwrap();
    let foreign_ticket = active_ticket(&foreign);
    for operation in [
        PlaybackOperation::Start,
        PlaybackOperation::Advance,
        PlaybackOperation::Pause,
        PlaybackOperation::Resume,
        PlaybackOperation::Stop,
        PlaybackOperation::Progress,
    ] {
        let (mut animator, player) = rig(preemption_spec());
        animator.sync_state("main-state").unwrap();
        if operation == PlaybackOperation::Resume {
            animator.pause().unwrap();
        }
        player.borrow_mut().substitute = Some((operation, foreign_ticket.clone()));
        let error = invoke_operation(&mut animator, operation).unwrap_err();
        assert!(
            matches!(error, AnimationError::Protocol { operation: actual, reason: PlaybackProtocolError::WrongTicket } if actual == operation)
        );
        assert_fault_locked(&mut animator, &player);
        animator.stop().unwrap();
        assert!(player.borrow().active.is_none());
    }
}

#[test]
fn tickets_capture_instance_definition_asset_clip_and_monotonic_playback_sequence() {
    let mut definition = preemption_spec();
    definition.version = 17;
    let opening = definition
        .clips
        .iter_mut()
        .find(|clip| clip.id() == "opening")
        .unwrap();
    *opening = AnimationClip::new("opening", 23, 10).unwrap();
    let (mut animator, _) = rig(definition.clone());
    animator.sync_state("main-state").unwrap();
    let first = active_ticket(&animator);
    assert_eq!(first.instance(), &animator.snapshot().instance);
    assert_eq!(first.definition_version(), 17);
    assert_eq!(first.asset_version(), 23);
    assert_eq!(first.clip(), "opening");
    assert_eq!(first.sequence(), 1);
    animator.advance(10).unwrap();
    let second = active_ticket(&animator);
    assert_eq!(second.instance(), first.instance());
    assert_eq!(second.definition_version(), first.definition_version());
    assert_eq!(second.clip(), "looping");
    assert_eq!(second.asset_version(), 1);
    assert_eq!(second.sequence(), 2);
    animator.stop().unwrap();
    animator.request_action("main").unwrap();
    assert_eq!(active_ticket(&animator).sequence(), 3);
    let (mut foreign, _) = rig(definition);
    foreign.sync_state("main-state").unwrap();
    let other = active_ticket(&foreign);
    assert_eq!(first.clip(), other.clip());
    assert_eq!(first.sequence(), other.sequence());
    assert_ne!(first.instance(), other.instance());
    assert_ne!(first, other);
}

#[test]
fn old_clip_and_replayed_same_clip_receipts_are_rejected_within_one_instance() {
    for same_clip in [false, true] {
        let (mut animator, player) = rig(preemption_spec());
        enter_phase(&mut animator, if same_clip { "looping" } else { "opening" });
        let old_ticket = active_ticket(&animator);
        animator.advance(10).unwrap();
        let current_ticket = active_ticket(&animator);
        assert_eq!(old_ticket.instance(), current_ticket.instance());
        assert_ne!(old_ticket.sequence(), current_ticket.sequence());
        assert_eq!(old_ticket.clip() == current_ticket.clip(), same_clip);
        player.borrow_mut().substitute = Some((PlaybackOperation::Advance, old_ticket));
        assert!(matches!(
            animator.advance(1),
            Err(AnimationError::Protocol {
                reason: PlaybackProtocolError::WrongTicket,
                ..
            })
        ));
        assert_eq!(active_ticket(&animator), current_ticket);
        assert_eq!(animator.snapshot().pending_ms, 1);
        assert_fault_locked(&mut animator, &player);
    }
}

#[test]
fn receipts_from_other_definition_and_asset_versions_are_rejected() {
    for (version, asset_version) in [(2, 1), (1, 2), (2, 2)] {
        let mut foreign_definition = preemption_spec();
        foreign_definition.version = version;
        let opening = foreign_definition
            .clips
            .iter_mut()
            .find(|clip| clip.id() == "opening")
            .unwrap();
        *opening = AnimationClip::new("opening", asset_version, 10).unwrap();
        let (mut foreign, _) = rig(foreign_definition);
        foreign.sync_state("main-state").unwrap();
        let foreign_ticket = active_ticket(&foreign);
        assert_eq!(foreign_ticket.definition_version(), version);
        assert_eq!(foreign_ticket.asset_version(), asset_version);
        let (mut animator, player) = rig(preemption_spec());
        animator.sync_state("main-state").unwrap();
        let ticket = active_ticket(&animator);
        player.borrow_mut().substitute = Some((PlaybackOperation::Advance, foreign_ticket));
        player.borrow_mut().advance_override = Some((1, Some(PlaybackBoundary::Completed)));
        let before_starts = starts(&player);
        assert!(matches!(
            animator.advance(7),
            Err(AnimationError::Protocol {
                reason: PlaybackProtocolError::WrongTicket,
                ..
            })
        ));
        assert_eq!(active_ticket(&animator), ticket);
        assert_eq!(animator.snapshot().pending_ms, 7);
        assert_eq!(starts(&player), before_starts);
    }
}

#[test]
fn same_playback_ticket_is_not_an_operation_level_anti_replay_token() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    let ticket = animator.playback_progress().unwrap().unwrap().ticket;
    for operation in [
        PlaybackOperation::Pause,
        PlaybackOperation::Resume,
        PlaybackOperation::Advance,
        PlaybackOperation::Progress,
    ] {
        player.borrow_mut().substitute = Some((operation, ticket.clone()));
        invoke_operation(&mut animator, operation).unwrap();
        assert_eq!(active_ticket(&animator), ticket);
        assert!(!animator.snapshot().faulted);
    }
}

#[test]
fn wrong_stop_receipt_prevents_replacement_start_until_confirmed_stop() {
    let (mut foreign, _) = rig(preemption_spec());
    foreign.request_action("main").unwrap();
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    let old_ticket = active_ticket(&animator);
    player.borrow_mut().substitute = Some((PlaybackOperation::Stop, active_ticket(&foreign)));
    assert!(matches!(
        animator.request_action("high"),
        Err(AnimationError::Protocol {
            operation: PlaybackOperation::Stop,
            reason: PlaybackProtocolError::WrongTicket
        })
    ));
    assert_eq!(active_ticket(&animator), old_ticket);
    assert_eq!(starts(&player), ["opening"]);
    assert_fault_locked(&mut animator, &player);
    animator.stop().unwrap();
    animator.request_action("high").unwrap();
    current(&animator, "high", "high-node");
}

#[test]
fn invalid_consumption_and_mode_boundaries_lock_before_transition_or_time_subtraction() {
    for (mode, consumed_ms, boundary, reason) in [
        (
            PlaybackMode::Once,
            6,
            None,
            PlaybackProtocolError::InvalidConsumption,
        ),
        (
            PlaybackMode::Once,
            4,
            None,
            PlaybackProtocolError::InvalidConsumption,
        ),
        (
            PlaybackMode::Once,
            0,
            Some(PlaybackBoundary::Completed),
            PlaybackProtocolError::InvalidConsumption,
        ),
        (
            PlaybackMode::Once,
            6,
            Some(PlaybackBoundary::Completed),
            PlaybackProtocolError::InvalidConsumption,
        ),
        (
            PlaybackMode::Once,
            5,
            Some(PlaybackBoundary::LoopBoundary),
            PlaybackProtocolError::InvalidBoundary,
        ),
        (
            PlaybackMode::Loop,
            5,
            Some(PlaybackBoundary::Completed),
            PlaybackProtocolError::InvalidBoundary,
        ),
    ] {
        let (mut animator, player) = rig(spec(vec![simple(
            "test",
            0,
            PreemptionPolicy::AfterExit,
            mode,
        )]));
        animator.request_action("test").unwrap();
        let ticket = active_ticket(&animator);
        player.borrow_mut().advance_override = Some((consumed_ms, boundary));
        assert!(
            matches!(animator.advance(5), Err(AnimationError::Protocol { operation: PlaybackOperation::Advance, reason: actual }) if actual == reason)
        );
        assert_eq!(active_ticket(&animator), ticket);
        assert_eq!(animator.snapshot().pending_ms, 5);
        assert_eq!(starts(&player), ["test-node"]);
        assert_fault_locked(&mut animator, &player);
    }
}

#[test]
fn backend_is_boundary_authority_without_kernel_duration_guessing() {
    let (mut animator, player) = rig(spec(vec![simple(
        "test",
        0,
        PreemptionPolicy::AfterExit,
        PlaybackMode::Once,
    )]));
    animator.request_action("test").unwrap();
    player.borrow_mut().advance_override = Some((2, Some(PlaybackBoundary::Completed)));
    assert_eq!(animator.advance(5).unwrap().status, AnimationStatus::Idle);
    assert!(!animator.snapshot().faulted);
    assert_eq!(animator.snapshot().pending_ms, 0);
}

#[test]
fn invalid_progress_positions_and_pause_flags_fault_lock() {
    for (mode, elapsed_ms, paused) in [
        (PlaybackMode::Once, 11, false),
        (PlaybackMode::Loop, 10, false),
        (PlaybackMode::Once, 0, true),
    ] {
        let (mut animator, player) = rig(spec(vec![simple(
            "test",
            0,
            PreemptionPolicy::AfterExit,
            mode,
        )]));
        animator.request_action("test").unwrap();
        let ticket = active_ticket(&animator);
        player.borrow_mut().position_override = Some((elapsed_ms, paused));
        assert!(matches!(
            animator.playback_progress(),
            Err(AnimationError::Protocol {
                operation: PlaybackOperation::Progress,
                reason: PlaybackProtocolError::InvalidPosition
            })
        ));
        assert_eq!(active_ticket(&animator), ticket);
        assert_eq!(animator.snapshot().pending_ms, 0);
        assert_fault_locked(&mut animator, &player);
    }
}

#[test]
fn completed_once_position_is_queryable_without_inventing_a_boundary() {
    let (mut animator, player) = rig(spec(vec![simple(
        "test",
        0,
        PreemptionPolicy::AfterExit,
        PlaybackMode::Once,
    )]));
    animator.request_action("test").unwrap();
    player.borrow_mut().position_override = Some((10, false));
    assert_eq!(
        animator.playback_progress().unwrap().unwrap().elapsed_ms,
        10
    );
    current(&animator, "test", "test-node");
    assert!(!animator.snapshot().faulted);
}

fn fast_loop_spec() -> AnimationSpec {
    let mut definition = spec(vec![simple(
        "fast",
        10,
        PreemptionPolicy::AfterExit,
        PlaybackMode::Loop,
    )]);
    definition.clips[0] = AnimationClip::new("fast-node", 1, 1).unwrap();
    definition
}

#[test]
fn boundary_budget_yields_at_128_and_zero_increments_drain_remaining_time() {
    let (mut animator, player) = rig(fast_loop_spec());
    animator.request_action("fast").unwrap();
    let ticket = active_ticket(&animator);
    let first = animator.advance(300).unwrap();
    assert_eq!(first.status, AnimationStatus::Yielded);
    assert_eq!(first.processed, ANIMATION_ADVANCE_BUDGET);
    assert_eq!(first.pending_ms, 172);
    let second = animator.advance(0).unwrap();
    assert_eq!(second.status, AnimationStatus::Yielded);
    assert_eq!(second.processed, 128);
    assert_eq!(second.pending_ms, 44);
    let third = animator.advance(0).unwrap();
    assert_eq!(third.status, AnimationStatus::Playing);
    assert_eq!(third.processed, 44);
    assert_eq!(third.pending_ms, 0);
    let count = player.borrow().calls.len();
    assert_eq!(animator.advance(0).unwrap().processed, 0);
    assert_eq!(player.borrow().calls.len(), count);
    assert_eq!(active_ticket(&animator), ticket);
    assert_eq!(starts(&player), ["fast-node"]);
}

#[test]
fn pending_time_overflow_is_rejected_before_any_state_or_backend_mutation() {
    let (mut animator, player) = rig(fast_loop_spec());
    animator.request_action("fast").unwrap();
    let yielded = animator.advance(u64::MAX).unwrap();
    assert_eq!(yielded.status, AnimationStatus::Yielded);
    assert_eq!(yielded.pending_ms, u64::MAX - 128);
    let snapshot = animator.snapshot();
    let calls = player.borrow().calls.clone();
    assert!(matches!(
        animator.advance(129),
        Err(AnimationError::TimeOverflow)
    ));
    assert_eq!(animator.snapshot(), snapshot);
    assert_eq!(player.borrow().calls, calls);
    assert!(!animator.snapshot().faulted);
}

#[test]
fn paused_zero_increment_queries_do_not_consume_preexisting_yielded_debt() {
    let (mut animator, player) = rig(fast_loop_spec());
    animator.request_action("fast").unwrap();
    animator.advance(300).unwrap();
    animator.pause().unwrap();
    let snapshot = animator.snapshot();
    let calls = player.borrow().calls.clone();
    let progress = animator.advance(0).unwrap();
    assert_eq!(progress.status, AnimationStatus::Paused);
    assert_eq!(progress.pending_ms, 172);
    assert_eq!(progress.processed, 0);
    assert!(matches!(animator.advance(1), Err(AnimationError::Paused)));
    assert_eq!(animator.snapshot(), snapshot);
    assert_eq!(player.borrow().calls, calls);
    animator.resume().unwrap();
    assert_eq!(animator.snapshot().pending_ms, 172);
    assert_eq!(animator.advance(0).unwrap().pending_ms, 44);
}

#[test]
fn normal_successor_inherits_remaining_time_after_complete_cleanup_chain() {
    let (mut animator, _) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    animator.request_action("peer").unwrap();
    let result = animator.advance(35).unwrap();
    current(&animator, "peer", "peer-node");
    assert_eq!(result.pending_ms, 0);
    assert_eq!(animator.playback_progress().unwrap().unwrap().elapsed_ms, 5);
}

fn debt_spec() -> AnimationSpec {
    let mut definition = trigger_target_spec();
    definition.actions[0].entry = "looping".into();
    definition.actions[0].nodes.remove(0);
    let looping = definition
        .clips
        .iter_mut()
        .find(|clip| clip.id() == "looping")
        .unwrap();
    *looping = AnimationClip::new("looping", 1, 1).unwrap();
    definition
}

fn assert_new_action_has_no_old_debt_or_trigger(
    animator: &mut TestAnimator,
    player: &SharedPlayer,
) {
    animator.request_action("high").unwrap();
    current(animator, "high", "high-node");
    let calls = player.borrow().calls.clone();
    let result = animator.advance(0).unwrap();
    assert_eq!(result.pending_ms, 0);
    assert_eq!(result.processed, 0);
    assert_eq!(player.borrow().calls, calls);
    let snapshot = animator.snapshot();
    assert!(snapshot.candidate.is_none());
    assert!(!snapshot.exit_requested);
    assert!(!snapshot.exiting);
    assert!(!snapshot.paused);
    assert!(!snapshot.faulted);
    assert_eq!(animator.playback_progress().unwrap().unwrap().elapsed_ms, 0);
}

#[test]
fn confirmed_stop_discards_old_debt_candidate_and_triggers_before_fresh_zero_advance() {
    let (mut animator, player) = rig(debt_spec());
    animator.sync_state("main-state").unwrap();
    assert_eq!(
        animator.advance(1000).unwrap().status,
        AnimationStatus::Yielded
    );
    assert_eq!(animator.snapshot().pending_ms, 872);
    animator.request_action("peer").unwrap();
    animator.trigger("ping").unwrap();
    animator.pause().unwrap();
    animator.stop().unwrap();
    assert_eq!(animator.snapshot().pending_ms, 0);
    assert_new_action_has_no_old_debt_or_trigger(&mut animator, &player);
}

#[test]
fn natural_finish_without_successor_discards_surplus_time_and_old_triggers() {
    let mut definition = trigger_target_spec();
    definition.actions[0] = simple("main", 10, PreemptionPolicy::AfterExit, PlaybackMode::Once);
    definition
        .clips
        .push(AnimationClip::new("main-node", 1, 10).unwrap());
    let (mut animator, player) = rig(definition);
    animator.sync_state("main-state").unwrap();
    animator.trigger("ping").unwrap();
    let result = animator.advance(1000).unwrap();
    assert_eq!(result.status, AnimationStatus::Idle);
    assert_eq!(result.pending_ms, 0);
    assert!(animator.snapshot().action.is_none());
    assert_new_action_has_no_old_debt_or_trigger(&mut animator, &player);
}

#[test]
fn cleanup_finish_without_successor_discards_preexisting_debt_and_old_triggers() {
    let (mut animator, player) = rig(debt_spec());
    animator.sync_state("main-state").unwrap();
    animator.advance(1000).unwrap();
    animator.request_action("peer").unwrap();
    animator.trigger("ping").unwrap();
    animator.request_exit().unwrap();
    let result = animator.advance(0).unwrap();
    assert_eq!(result.status, AnimationStatus::Idle);
    assert_eq!(result.pending_ms, 0);
    assert!(animator.snapshot().candidate.is_none());
    assert_new_action_has_no_old_debt_or_trigger(&mut animator, &player);
}

#[test]
fn fault_confirmation_stop_discards_debt_candidate_and_triggers_before_restart() {
    let (mut animator, player) = rig(debt_spec());
    animator.sync_state("main-state").unwrap();
    animator.advance(1000).unwrap();
    animator.request_action("peer").unwrap();
    animator.trigger("ping").unwrap();
    player.borrow_mut().fail = Some(PlaybackOperation::Advance);
    assert!(animator.advance(5).is_err());
    assert_eq!(animator.snapshot().pending_ms, 877);
    assert_fault_locked(&mut animator, &player);
    animator.stop().unwrap();
    assert_eq!(animator.snapshot().pending_ms, 0);
    assert_new_action_has_no_old_debt_or_trigger(&mut animator, &player);
}

#[test]
fn idle_time_cannot_be_banked_for_a_future_action() {
    let (mut animator, player) = rig(trigger_target_spec());
    assert!(animator.playback_progress().unwrap().is_none());
    assert_eq!(
        animator.advance(u64::MAX).unwrap().status,
        AnimationStatus::Idle
    );
    assert_eq!(animator.snapshot().pending_ms, 0);
    assert!(player.borrow().calls.is_empty());
    assert_new_action_has_no_old_debt_or_trigger(&mut animator, &player);
}

#[test]
fn entering_interior_cleanup_node_naturally_does_not_reenter_exit_entry() {
    let mut definition = preemption_spec();
    definition.actions[0].nodes[0].edges[0].target = AnimationTarget::Node("tail".into());
    definition.actions[0].nodes.remove(1);
    let (mut animator, player) = rig(definition);
    animator.sync_state("main-state").unwrap();
    animator.advance(10).unwrap();
    current(&animator, "main", "tail");
    assert!(animator.snapshot().exiting);
    animator.request_action("peer").unwrap();
    animator.advance(9).unwrap();
    current(&animator, "main", "tail");
    animator.advance(1).unwrap();
    current(&animator, "peer", "peer-node");
    assert_eq!(starts(&player), ["opening", "tail", "peer-node"]);
}

#[test]
fn action_entry_inside_cleanup_subgraph_is_already_exiting() {
    let mut definition = preemption_spec();
    definition.actions[0].entry = "cleanup".into();
    definition.actions[0].nodes.drain(0..2);
    let (mut animator, player) = rig(definition);
    animator.request_action("main").unwrap();
    assert!(animator.snapshot().exiting);
    animator.request_exit().unwrap();
    assert_eq!(animator.advance(20).unwrap().status, AnimationStatus::Idle);
    assert_eq!(starts(&player), ["cleanup", "tail"]);
}

#[test]
fn paused_latest_accepted_request_replaces_candidate_using_current_priority() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    animator.pause().unwrap();
    let calls = player.borrow().calls.clone();
    animator.request_action("high").unwrap();
    animator.request_action("peer").unwrap();
    assert_eq!(
        animator.request_action("low").unwrap(),
        AnimationRequest::RejectedLowerPriority
    );
    assert_eq!(animator.snapshot().candidate.as_deref(), Some("peer"));
    assert_eq!(player.borrow().calls, calls);
    animator.resume().unwrap();
    current(&animator, "main", "opening");
    animator.advance(30).unwrap();
    current(&animator, "peer", "peer-node");
    assert_eq!(starts(&player), ["opening", "cleanup", "tail", "peer-node"]);
}

#[test]
fn replacement_prepares_before_stop_and_starts_only_after_stop_acknowledgment() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    let old_ticket = active_ticket(&animator);
    let offset = player.borrow().calls.len();
    animator.request_action("high").unwrap();
    let new_ticket = active_ticket(&animator);
    assert_eq!(
        &player.borrow().calls[offset..],
        [
            Call::Prepare("high-node".into(), 1, PlaybackMode::Loop),
            Call::Stop(old_ticket),
            Call::Start(new_ticket, PlaybackMode::Loop),
        ]
    );
}

#[test]
fn unknown_start_effect_is_confirmed_stopped_using_the_new_playback_ticket() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    player.borrow_mut().fail = Some(PlaybackOperation::Start);
    assert!(animator.request_action("high").is_err());
    let unknown_ticket = active_ticket(&animator);
    assert_eq!(unknown_ticket.clip(), "high-node");
    assert_eq!(
        player.borrow().active.as_ref().unwrap().ticket,
        unknown_ticket
    );
    assert_fault_locked(&mut animator, &player);
    animator.stop().unwrap();
    assert_eq!(
        player.borrow().calls.last(),
        Some(&Call::Stop(unknown_ticket))
    );
    assert!(player.borrow().active.is_none());
    animator.request_action("high").unwrap();
    assert_eq!(active_ticket(&animator).sequence(), 3);
}

#[test]
fn instances_sharing_one_definition_keep_identity_parameters_cursor_and_pause_isolated() {
    let definition = AnimationDefinition::new(preemption_spec()).unwrap();
    let left_player = Rc::new(RefCell::new(PlayerState::default()));
    let right_player = Rc::new(RefCell::new(PlayerState::default()));
    let mut left = Animator::new(definition.clone(), SyntheticPlayer(Rc::clone(&left_player)));
    let mut right = Animator::new(definition, SyntheticPlayer(Rc::clone(&right_player)));
    left.sync_state("main-state").unwrap();
    right.sync_state("main-state").unwrap();
    let right_calls = right_player.borrow().calls.clone();
    left.advance(13).unwrap();
    left.set_parameter("flag", true).unwrap();
    left.pause().unwrap();
    assert_eq!(right_player.borrow().calls, right_calls);
    assert_eq!(right.parameter("flag"), Some(false));
    assert_eq!(left.parameter("flag"), Some(true));
    assert_ne!(
        active_ticket(&left).instance(),
        active_ticket(&right).instance()
    );
    assert_eq!(active_ticket(&left).sequence(), 2);
    assert_eq!(active_ticket(&right).sequence(), 1);
    right.advance(5).unwrap();
    assert_eq!(right.playback_progress().unwrap().unwrap().elapsed_ms, 5);
    assert_eq!(left.playback_progress().unwrap().unwrap().elapsed_ms, 3);
    assert!(left.snapshot().paused);
    assert!(!right.snapshot().paused);
    left.stop().unwrap();
    assert!(right_player.borrow().active.is_some());
    current(&right, "main", "opening");
}

#[derive(Debug, PartialEq, Eq)]
struct Trace {
    action: Option<String>,
    node: Option<String>,
    sequence: Option<u64>,
    elapsed_ms: u64,
    pending_ms: u64,
}

fn layout_trace(animator: &mut TestAnimator, layout: &mut [(String, i32, i32)]) -> Vec<Trace> {
    animator.sync_state("main-state").unwrap();
    let mut result = Vec::new();
    for (index, delta) in [0, 7, 3, 13, 7, 10, 10, 5].into_iter().enumerate() {
        layout.rotate_left(1);
        layout[0].1 += 7;
        layout[0].2 -= 11;
        if index == 4 {
            animator.request_action("peer").unwrap();
        }
        animator.advance(delta).unwrap();
        let snapshot = animator.snapshot();
        let elapsed_ms = animator
            .playback_progress()
            .unwrap()
            .map_or(0, |position| position.elapsed_ms);
        result.push(Trace {
            action: snapshot.action,
            node: snapshot.node,
            sequence: snapshot.ticket.map(|ticket| ticket.sequence()),
            elapsed_ms,
            pending_ms: snapshot.pending_ms,
        });
    }
    result
}

#[test]
fn host_layout_edits_do_not_change_execution_trace() {
    let (mut left, left_player) = rig(preemption_spec());
    let (mut right, right_player) = rig(preemption_spec());
    let mut left_layout = [("opening".into(), 0, 0), ("looping".into(), 1, 1)];
    let mut right_layout = [
        ("looping".into(), -1000, 500),
        ("opening".into(), 9000, -700),
    ];
    assert_eq!(
        layout_trace(&mut left, &mut left_layout),
        layout_trace(&mut right, &mut right_layout)
    );
    assert_eq!(starts(&left_player), starts(&right_player));
    assert_ne!(left_layout, right_layout);
    current(&left, "peer", "peer-node");
}

#[test]
fn publishing_new_definition_does_not_hot_edit_in_flight_instance() {
    let mut draft = preemption_spec();
    let (mut old, old_player) = rig(draft.clone());
    old.sync_state("main-state").unwrap();
    let old_ticket = active_ticket(&old);
    draft.version = 2;
    let opening = draft
        .clips
        .iter_mut()
        .find(|clip| clip.id() == "opening")
        .unwrap();
    *opening = AnimationClip::new("opening", 2, 20).unwrap();
    let (mut new, new_player) = rig(draft);
    new.sync_state("main-state").unwrap();
    old.advance(10).unwrap();
    new.advance(10).unwrap();
    current(&old, "main", "looping");
    current(&new, "main", "opening");
    assert_eq!(old.snapshot().definition_version, 1);
    assert_eq!(new.snapshot().definition_version, 2);
    assert_eq!(old_ticket.definition_version(), 1);
    assert_eq!(old_ticket.asset_version(), 1);
    assert_eq!(active_ticket(&new).definition_version(), 2);
    assert_eq!(active_ticket(&new).asset_version(), 2);
    assert!(old_player.borrow().calls.contains(&Call::Prepare(
        "opening".into(),
        1,
        PlaybackMode::Once
    )));
    assert!(new_player.borrow().calls.contains(&Call::Prepare(
        "opening".into(),
        2,
        PlaybackMode::Once
    )));
}

#[test]
fn editing_owned_snapshot_cannot_change_runtime_or_backend() {
    let (mut animator, player) = rig(preemption_spec());
    animator.sync_state("main-state").unwrap();
    let before = animator.snapshot();
    let calls = player.borrow().calls.clone();
    let mut edited = animator.snapshot();
    edited.action = None;
    edited.node = Some("forged".into());
    edited.ticket = None;
    edited.state = None;
    edited.candidate = Some("high".into());
    edited.paused = true;
    edited.faulted = true;
    edited.exit_requested = true;
    edited.pending_ms = u64::MAX;
    assert_ne!(edited, before);
    assert_eq!(animator.snapshot(), before);
    assert_eq!(player.borrow().calls, calls);
}
