use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

const MAX_ID_BYTES: usize = 128;
const MAX_CATALOG_ITEMS: usize = 128;
const MAX_SIGNAL_ITEMS: usize = 64;
const MAX_NODES: usize = 128;
const MAX_EDGES: usize = 512;

/// Immutable metadata registered by a trusted host, not proof of loaded assets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnimationClip {
    id: String,
    asset_version: u64,
    duration_ms: u64,
}

impl AnimationClip {
    /// Creates a clip with an ASCII identifier, nonzero version and positive duration.
    pub fn new(
        id: impl Into<String>,
        asset_version: u64,
        duration_ms: u64,
    ) -> Result<Self, AnimationValidationError> {
        let clip = Self {
            id: id.into(),
            asset_version,
            duration_ms,
        };
        validate_clip(&clip)?;
        Ok(clip)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn asset_version(&self) -> u64 {
        self.asset_version
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackMode {
    Once,
    Loop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreemptionPolicy {
    Immediate,
    AfterExit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EdgeCondition {
    Completed,
    LoopBoundary,
    Parameter { name: String, value: bool },
    Trigger(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnimationTarget {
    Node(String),
    Finish,
}

/// An edge draft; declaration order breaks equal-priority ties.
#[derive(Clone, Debug)]
pub struct AnimationEdge {
    pub priority: i32,
    pub condition: EdgeCondition,
    pub target: AnimationTarget,
}

#[derive(Clone, Debug)]
pub struct AnimationNode {
    pub id: String,
    pub clip: String,
    pub mode: PlaybackMode,
    pub edges: Vec<AnimationEdge>,
}

#[derive(Clone, Debug)]
pub struct AnimationAction {
    pub id: String,
    pub entry: String,
    pub exit: Option<String>,
    pub priority: i32,
    pub preemption: PreemptionPolicy,
    pub nodes: Vec<AnimationNode>,
}

/// A state may have multiple bindings, including bindings to the same action.
#[derive(Clone, Debug)]
pub struct AnimationBinding {
    pub state: String,
    pub parameter: Option<(String, bool)>,
    pub action: String,
    pub priority: i32,
}

/// Editable execution data, with no host layout or mutable runtime state.
#[derive(Clone, Debug)]
pub struct AnimationSpec {
    pub version: u64,
    pub clips: Vec<AnimationClip>,
    pub actions: Vec<AnimationAction>,
    pub parameters: Vec<(String, bool)>,
    pub triggers: Vec<String>,
    pub bindings: Vec<AnimationBinding>,
}

/// A validated, shared snapshot that cannot be edited after publication.
#[derive(Clone, Debug)]
pub struct AnimationDefinition {
    inner: Arc<AnimationSpec>,
}

impl AnimationDefinition {
    /// Validates the entire draft before capturing its immutable version.
    pub fn new(spec: AnimationSpec) -> Result<Self, AnimationValidationError> {
        validate_animation(&spec)?;
        Ok(Self {
            inner: Arc::new(spec),
        })
    }

    pub fn version(&self) -> u64 {
        self.inner.version
    }

    pub fn clips(&self) -> &[AnimationClip] {
        &self.inner.clips
    }

    pub fn actions(&self) -> &[AnimationAction] {
        &self.inner.actions
    }

    pub fn parameters(&self) -> &[(String, bool)] {
        &self.inner.parameters
    }

    pub fn triggers(&self) -> &[String] {
        &self.inner.triggers
    }

    pub fn bindings(&self) -> &[AnimationBinding] {
        &self.inner.bindings
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnimationValidationError {
    InvalidId,
    InvalidVersion,
    InvalidDuration,
    InvalidDefinition,
    Capacity,
    DuplicateId,
    MissingReference,
    InvalidCondition,
    UnreachableNode,
    ImmediateCycle,
    InvalidExit,
}

impl fmt::Display for AnimationValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidId => "animation identifier must contain 1 to 128 ASCII letters, digits, underscores or hyphens",
            Self::InvalidVersion => "animation definition and asset versions must be nonzero",
            Self::InvalidDuration => "animation clip duration must be positive",
            Self::InvalidDefinition => "animation actions require nonempty node lists",
            Self::Capacity => "animation definition exceeds its capacity",
            Self::DuplicateId => "animation directory contains a duplicate identifier",
            Self::MissingReference => "animation definition references a missing item",
            Self::InvalidCondition => "animation condition is undeclared or incompatible with its playback mode",
            Self::UnreachableNode => "animation node is unreachable from the entry and exit",
            Self::ImmediateCycle => "animation graph contains an instantaneous transition cycle",
            Self::InvalidExit => "animation exit must be an acyclic chain of single-play completion nodes",
        })
    }
}

impl std::error::Error for AnimationValidationError {}

/// Checks the same contracts used by [`AnimationDefinition::new`].
///
/// Limits are 128 clips, actions, total nodes and bindings, 512 total edges,
/// and 64 declarations each for boolean parameters and triggers. Catalogs may
/// be empty and clips may be unused; each declared action requires a valid entry
/// and nonempty node list. Instantaneous cycles precede reachability, including
/// cycles outside either entry. Positive-time cycles in ordinary graphs are
/// allowed, but every exit must terminate without parameters or triggers.
pub fn validate_animation(spec: &AnimationSpec) -> Result<(), AnimationValidationError> {
    if spec.version == 0 {
        return Err(AnimationValidationError::InvalidVersion);
    }
    validate_capacity(spec)?;
    validate_identifiers(spec.clips.iter().map(AnimationClip::id))?;
    validate_identifiers(spec.actions.iter().map(|action| action.id.as_str()))?;
    validate_identifiers(spec.parameters.iter().map(|(name, _)| name.as_str()))?;
    validate_identifiers(spec.triggers.iter().map(String::as_str))?;
    for clip in &spec.clips {
        validate_clip(clip)?;
    }
    for binding in &spec.bindings {
        validate_identifier(&binding.state)?;
        validate_identifier(&binding.action)?;
        if !spec
            .actions
            .iter()
            .any(|action| action.id == binding.action)
        {
            return Err(AnimationValidationError::MissingReference);
        }
        if let Some((name, _)) = &binding.parameter {
            validate_parameter(spec, name)?;
        }
    }
    let graphs = spec
        .actions
        .iter()
        .map(|action| build_action_graph(spec, action))
        .collect::<Result<Vec<_>, _>>()?;
    for graph in &graphs {
        validate_immediate_graph(&graph.immediate)?;
    }
    for (action, graph) in spec.actions.iter().zip(&graphs) {
        validate_reachability(graph)?;
        if let Some(exit) = graph.exit {
            validate_exit(action, graph, exit)?;
        }
    }
    Ok(())
}

fn validate_identifier(value: &str) -> Result<(), AnimationValidationError> {
    if value.is_empty()
        || value.len() > MAX_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(AnimationValidationError::InvalidId);
    }
    Ok(())
}

fn validate_identifiers<'identifier>(
    identifiers: impl IntoIterator<Item = &'identifier str>,
) -> Result<(), AnimationValidationError> {
    let mut seen = BTreeSet::new();
    for identifier in identifiers {
        validate_identifier(identifier)?;
        if !seen.insert(identifier) {
            return Err(AnimationValidationError::DuplicateId);
        }
    }
    Ok(())
}

fn validate_clip(clip: &AnimationClip) -> Result<(), AnimationValidationError> {
    validate_identifier(clip.id())?;
    if clip.asset_version() == 0 {
        return Err(AnimationValidationError::InvalidVersion);
    }
    if clip.duration_ms() == 0 {
        return Err(AnimationValidationError::InvalidDuration);
    }
    Ok(())
}

fn validate_capacity(spec: &AnimationSpec) -> Result<(), AnimationValidationError> {
    if spec.clips.len() > MAX_CATALOG_ITEMS
        || spec.actions.len() > MAX_CATALOG_ITEMS
        || spec.bindings.len() > MAX_CATALOG_ITEMS
        || spec.parameters.len() > MAX_SIGNAL_ITEMS
        || spec.triggers.len() > MAX_SIGNAL_ITEMS
    {
        return Err(AnimationValidationError::Capacity);
    }
    let mut nodes = 0usize;
    let mut edges = 0usize;
    for action in &spec.actions {
        nodes = nodes
            .checked_add(action.nodes.len())
            .ok_or(AnimationValidationError::Capacity)?;
        if nodes > MAX_NODES {
            return Err(AnimationValidationError::Capacity);
        }
        for node in &action.nodes {
            edges = edges
                .checked_add(node.edges.len())
                .ok_or(AnimationValidationError::Capacity)?;
            if edges > MAX_EDGES {
                return Err(AnimationValidationError::Capacity);
            }
        }
    }
    Ok(())
}

fn validate_parameter(spec: &AnimationSpec, name: &str) -> Result<(), AnimationValidationError> {
    validate_identifier(name)?;
    if !spec.parameters.iter().any(|(declared, _)| declared == name) {
        return Err(AnimationValidationError::InvalidCondition);
    }
    Ok(())
}

fn validate_condition(
    spec: &AnimationSpec,
    mode: PlaybackMode,
    condition: &EdgeCondition,
) -> Result<(), AnimationValidationError> {
    match condition {
        EdgeCondition::Completed if mode == PlaybackMode::Once => Ok(()),
        EdgeCondition::LoopBoundary if mode == PlaybackMode::Loop => Ok(()),
        EdgeCondition::Parameter { name, .. } => validate_parameter(spec, name),
        EdgeCondition::Trigger(name) => {
            validate_identifier(name)?;
            if spec.triggers.contains(name) {
                Ok(())
            } else {
                Err(AnimationValidationError::InvalidCondition)
            }
        }
        EdgeCondition::Completed | EdgeCondition::LoopBoundary => {
            Err(AnimationValidationError::InvalidCondition)
        }
    }
}

struct ActionGraph {
    targets: Vec<Vec<usize>>,
    immediate: Vec<Vec<usize>>,
    entry: usize,
    exit: Option<usize>,
}

fn node_index(action: &AnimationAction, name: &str) -> Result<usize, AnimationValidationError> {
    validate_identifier(name)?;
    action
        .nodes
        .iter()
        .position(|node| node.id == name)
        .ok_or(AnimationValidationError::MissingReference)
}

fn build_action_graph(
    spec: &AnimationSpec,
    action: &AnimationAction,
) -> Result<ActionGraph, AnimationValidationError> {
    if action.nodes.is_empty() {
        return Err(AnimationValidationError::InvalidDefinition);
    }
    validate_identifiers(action.nodes.iter().map(|node| node.id.as_str()))?;
    let entry = node_index(action, &action.entry)?;
    let exit = action
        .exit
        .as_deref()
        .map(|name| node_index(action, name))
        .transpose()?;
    let mut targets = vec![Vec::new(); action.nodes.len()];
    let mut immediate = vec![Vec::new(); action.nodes.len()];
    for (index, node) in action.nodes.iter().enumerate() {
        validate_identifier(&node.clip)?;
        if !spec.clips.iter().any(|clip| clip.id() == node.clip) {
            return Err(AnimationValidationError::MissingReference);
        }
        for edge in &node.edges {
            validate_condition(spec, node.mode, &edge.condition)?;
            if let AnimationTarget::Node(name) = &edge.target {
                let target = node_index(action, name)?;
                targets[index].push(target);
                if matches!(
                    edge.condition,
                    EdgeCondition::Parameter { .. } | EdgeCondition::Trigger(_)
                ) {
                    immediate[index].push(target);
                }
            }
        }
    }
    Ok(ActionGraph {
        targets,
        immediate,
        entry,
        exit,
    })
}

fn validate_immediate_graph(graph: &[Vec<usize>]) -> Result<(), AnimationValidationError> {
    let mut indegrees = vec![0usize; graph.len()];
    for targets in graph {
        for &target in targets {
            indegrees[target] = indegrees[target]
                .checked_add(1)
                .ok_or(AnimationValidationError::Capacity)?;
        }
    }
    let mut ready = indegrees
        .iter()
        .enumerate()
        .filter_map(|(index, &degree)| (degree == 0).then_some(index))
        .collect::<Vec<_>>();
    let mut visited = 0usize;
    while let Some(index) = ready.pop() {
        visited = visited
            .checked_add(1)
            .ok_or(AnimationValidationError::Capacity)?;
        for &target in &graph[index] {
            indegrees[target] -= 1;
            if indegrees[target] == 0 {
                ready.push(target);
            }
        }
    }
    if visited != graph.len() {
        return Err(AnimationValidationError::ImmediateCycle);
    }
    Ok(())
}

fn validate_reachability(graph: &ActionGraph) -> Result<(), AnimationValidationError> {
    let mut visited = vec![false; graph.targets.len()];
    let mut pending = vec![graph.entry];
    if let Some(exit) = graph.exit {
        pending.push(exit);
    }
    while let Some(index) = pending.pop() {
        if visited[index] {
            continue;
        }
        visited[index] = true;
        pending.extend(&graph.targets[index]);
    }
    if visited.contains(&false) {
        return Err(AnimationValidationError::UnreachableNode);
    }
    Ok(())
}

fn validate_exit(
    action: &AnimationAction,
    graph: &ActionGraph,
    exit: usize,
) -> Result<(), AnimationValidationError> {
    let mut visited = vec![false; action.nodes.len()];
    let mut current = Some(exit);
    while let Some(index) = current {
        if visited[index] {
            return Err(AnimationValidationError::InvalidExit);
        }
        visited[index] = true;
        let node = &action.nodes[index];
        if node.mode != PlaybackMode::Once
            || node.edges.len() > 1
            || node
                .edges
                .iter()
                .any(|edge| edge.condition != EdgeCondition::Completed)
        {
            return Err(AnimationValidationError::InvalidExit);
        }
        current = graph.targets[index].first().copied();
    }
    Ok(())
}
