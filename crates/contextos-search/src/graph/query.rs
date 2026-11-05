//! Read-only traversals over the in-memory wikilink graph: neighbourhood
//! expansion, backlinks, shortest path, orphan and unresolved-target
//! listing, and the whole-graph view, plus the deterministic ordering
//! every result shares.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};

use petgraph::Direction;
use petgraph::stable_graph::{EdgeIndex, NodeIndex};
use petgraph::visit::EdgeRef;

use crate::SearchError;

use super::{GraphBackend, GraphDirection, GraphEdge, GraphNode, GraphNodeKind, GraphView, LinkGraph, SyncStatus};

impl LinkGraph {
    /// Returns nodes and edges reachable from `from` within `depth` hops
    /// following `direction`.
    ///
    /// The returned view lists every node discovered during the traversal
    /// (including `from` itself) plus every edge walked to reach it: an
    /// edge is recorded once, the first time either of its ends is
    /// encountered while its source has not yet reached the depth bound.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::InvalidDepth`] when `depth` is outside
    /// `1..=4`, and [`SearchError::UnknownNote`] when `from` is not in the
    /// graph.
    pub fn neighbours(&mut self, from: &str, depth: u32, direction: GraphDirection) -> Result<GraphView, SearchError> {
        self.catch_up()?;
        if !(1..=4).contains(&depth) {
            return Err(SearchError::InvalidDepth { depth });
        }
        let start = self.require_node(from)?;

        let mut visited: HashMap<NodeIndex, u32> = HashMap::new();
        visited.insert(start, 0);
        let mut queue: VecDeque<NodeIndex> = VecDeque::new();
        queue.push_back(start);
        let mut recorded: HashSet<EdgeIndex> = HashSet::new();
        let mut edges: Vec<GraphEdge> = Vec::new();

        while let Some(node) = queue.pop_front() {
            let node_depth = visited.get(&node).copied().unwrap_or(0);
            if node_depth >= depth {
                continue;
            }
            for &dir in direction.directions() {
                for edge_ref in self.graph.edges_directed(node, dir) {
                    let edge_id = edge_ref.id();
                    let other = if dir == Direction::Outgoing {
                        edge_ref.target()
                    } else {
                        edge_ref.source()
                    };
                    if recorded.insert(edge_id)
                        && let Some((_, _, view)) = self.edge_view(edge_id)
                    {
                        edges.push(view);
                    }
                    if let Entry::Vacant(slot) = visited.entry(other) {
                        slot.insert(node_depth + 1);
                        queue.push_back(other);
                    }
                }
            }
        }

        let nodes: Vec<GraphNode> = visited
            .keys()
            .filter_map(|&idx| self.graph.node_weight(idx).cloned())
            .collect();
        Ok(Self::into_view(nodes, edges))
    }

    /// Returns the notes and edges that link or embed directly to `from`.
    /// Equivalent to `neighbours(from, 1, GraphDirection::In)`,
    /// kept as its own method for the tool surface.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::UnknownNote`] when `from` is not in the
    /// graph.
    pub fn backlinks(&mut self, from: &str) -> Result<GraphView, SearchError> {
        self.neighbours(from, 1, GraphDirection::In)
    }

    /// Returns the shortest path between `from` and `to` by hop count,
    /// following `direction`.
    ///
    /// Ties are broken deterministically by exploring each node's
    /// neighbours in lexicographic order of their path, so the same graph
    /// always yields the same route. Returns an empty view when no route
    /// exists.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::UnknownNote`] when `from` or `to` is not in
    /// the graph.
    pub fn path_between(&mut self, from: &str, to: &str, direction: GraphDirection) -> Result<GraphView, SearchError> {
        self.catch_up()?;
        let start = self.require_node(from)?;
        let goal = self.require_node(to)?;

        if start == goal {
            let nodes = match self.graph.node_weight(start) {
                Some(node) => vec![node.clone()],
                None => Vec::new(),
            };
            return Ok(Self::into_view(nodes, Vec::new()));
        }

        let Some(route) = self.shortest_route(start, goal, direction) else {
            return Ok(GraphView {
                nodes: Vec::new(),
                edges: Vec::new(),
            });
        };

        let mut node_set: HashSet<NodeIndex> = HashSet::new();
        node_set.insert(start);
        let mut edges: Vec<GraphEdge> = Vec::new();
        for edge_id in route {
            if let Some((source, target, view)) = self.edge_view(edge_id) {
                node_set.insert(source);
                node_set.insert(target);
                edges.push(view);
            }
        }
        let nodes: Vec<GraphNode> = node_set
            .into_iter()
            .filter_map(|idx| self.graph.node_weight(idx).cloned())
            .collect();
        Ok(Self::into_view(nodes, edges))
    }

    /// Returns every note with neither incoming nor outgoing edges. Only notes
    /// appear: a file node or phantom exists only while a link reaches it. A note
    /// whose links could not be read is left out, because what it links to is
    /// not known.
    ///
    /// # Errors
    ///
    /// Returns a storage error when a cross-instance catch-up cannot read
    /// from the store; the traversal itself never fails.
    pub fn orphans(&mut self) -> Result<GraphView, SearchError> {
        self.catch_up()?;
        let nodes: Vec<GraphNode> = self
            .graph
            .node_indices()
            .filter_map(|idx| {
                let node = self.graph.node_weight(idx)?;
                let no_outgoing = self.graph.edges_directed(idx, Direction::Outgoing).next().is_none();
                let no_incoming = self.graph.edges_directed(idx, Direction::Incoming).next().is_none();
                (node.kind == GraphNodeKind::Note && node.unreadable.is_none() && no_outgoing && no_incoming)
                    .then(|| node.clone())
            })
            .collect();
        Ok(Self::into_view(nodes, Vec::new()))
    }

    /// Returns the sorted phantom-node paths `from` links or embeds to: the
    /// link targets that resolve to no file or to several, which back `links_read`'s
    /// `unresolved` reporting.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::UnknownNote`] when `from` is not in the
    /// graph.
    pub fn unresolved_targets(&mut self, from: &str) -> Result<Vec<String>, SearchError> {
        self.catch_up()?;
        let idx = self.require_node(from)?;
        let mut targets: Vec<String> = self
            .graph
            .edges_directed(idx, Direction::Outgoing)
            .filter_map(|edge_ref| {
                self.graph
                    .node_weight(edge_ref.target())
                    .filter(|node| node.phantom())
                    .map(|node| node.path.clone())
            })
            .collect();
        targets.sort_unstable();
        targets.dedup();
        Ok(targets)
    }

    /// Returns every node and edge currently in the graph, in the same
    /// deterministic order as a query result.
    ///
    /// Exposed beyond the crate (a deliberate, minimal visibility change)
    /// so a combined vault search service can report total node and edge
    /// counts for `query_index_status` without duplicating the
    /// graph's internal traversal.
    ///
    /// # Errors
    ///
    /// Returns a storage error when a cross-instance catch-up cannot read
    /// from the store.
    pub fn full_view(&mut self) -> Result<GraphView, SearchError> {
        self.catch_up()?;
        let nodes: Vec<GraphNode> = self.graph.node_weights().cloned().collect();
        let edges: Vec<GraphEdge> = self
            .graph
            .edge_indices()
            .filter_map(|id| self.edge_view(id).map(|(_, _, edge)| edge))
            .collect();
        Ok(Self::into_view(nodes, edges))
    }

    /// This instance's cross-instance propagation status, or `None` when
    /// the configured backend does not track one
    /// (`fjall`, `serde`).
    #[must_use]
    pub fn sync_status(&self) -> Option<SyncStatus> {
        (self.backend == GraphBackend::Sqlite).then_some(SyncStatus {
            generation: self.last_seen_generation,
            last_catch_up: self.last_catch_up,
        })
    }

    /// Resolves `path` to its node index.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::UnknownNote`] when `path` is not in the
    /// graph.
    fn require_node(&self, path: &str) -> Result<NodeIndex, SearchError> {
        self.index
            .get(path)
            .copied()
            .ok_or_else(|| SearchError::UnknownNote { path: path.to_owned() })
    }

    /// Builds the externally rendered `GraphEdge` for one internal edge
    /// id, alongside its source and target node indices.
    fn edge_view(&self, edge_id: EdgeIndex) -> Option<(NodeIndex, NodeIndex, GraphEdge)> {
        let (source, target) = self.graph.edge_endpoints(edge_id)?;
        let link = &self.graph.edge_weight(edge_id)?.link;
        let from = self.graph.node_weight(source)?.path.clone();
        let to = self.graph.node_weight(target)?.path.clone();
        Some((
            source,
            target,
            GraphEdge {
                from,
                to,
                form: link.form,
                target: link.target.clone(),
                line: link.line,
                raw: link.raw.clone(),
            },
        ))
    }

    /// Breadth-first search for the shortest `from`-to-`goal` route,
    /// breaking ties by exploring each node's neighbours in lexicographic
    /// path order. Returns the ordered edge ids of the route, or `None`
    /// when `goal` is unreachable.
    fn shortest_route(&self, start: NodeIndex, goal: NodeIndex, direction: GraphDirection) -> Option<Vec<EdgeIndex>> {
        let mut visited: HashSet<NodeIndex> = HashSet::new();
        visited.insert(start);
        let mut queue: VecDeque<NodeIndex> = VecDeque::new();
        queue.push_back(start);
        let mut predecessor: HashMap<NodeIndex, EdgeIndex> = HashMap::new();
        let mut found = false;

        'search: while let Some(node) = queue.pop_front() {
            let mut candidates: Vec<(NodeIndex, EdgeIndex)> = Vec::new();
            for &dir in direction.directions() {
                for edge_ref in self.graph.edges_directed(node, dir) {
                    let other = if dir == Direction::Outgoing {
                        edge_ref.target()
                    } else {
                        edge_ref.source()
                    };
                    candidates.push((other, edge_ref.id()));
                }
            }
            candidates.sort_by(|(a, _), (b, _)| {
                let a_path = self.graph.node_weight(*a).map_or("", |node| node.path.as_str());
                let b_path = self.graph.node_weight(*b).map_or("", |node| node.path.as_str());
                a_path.cmp(b_path)
            });

            for (other, edge_id) in candidates {
                if visited.contains(&other) {
                    continue;
                }
                visited.insert(other);
                predecessor.insert(other, edge_id);
                if other == goal {
                    found = true;
                    break 'search;
                }
                queue.push_back(other);
            }
        }

        if !found {
            return None;
        }

        let mut route: Vec<EdgeIndex> = Vec::new();
        let mut cursor = goal;
        while cursor != start {
            let Some(&edge_id) = predecessor.get(&cursor) else {
                break;
            };
            route.push(edge_id);
            let Some((source, target)) = self.graph.edge_endpoints(edge_id) else {
                break;
            };
            cursor = if target == cursor { source } else { target };
        }
        route.reverse();
        Some(route)
    }

    /// Sorts `nodes` by path and `edges` by `(from, to, form, line, raw)`, producing
    /// the deterministic order every query result shares.
    fn into_view(mut nodes: Vec<GraphNode>, mut edges: Vec<GraphEdge>) -> GraphView {
        nodes.sort_by(|a, b| a.path.cmp(&b.path));
        edges.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
        GraphView { nodes, edges }
    }
}
