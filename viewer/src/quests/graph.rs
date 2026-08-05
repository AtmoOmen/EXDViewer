use std::ops::Range;

/// The prerequisite graph over the quest list: longest-path ranks, weakly connected components and
/// a layered layout, all computed once so a frame only culls.
///
/// Nodes are positions in the ascending `row_ids` slice [`Graph::build`] was given. Only
/// `PreviousQuest` belongs here; `QuestLock` is a symmetric exclusion and would make this cyclic.
pub struct Graph {
    prereq_starts: Vec<u32>,
    prereq_items: Vec<u32>,
    dep_starts: Vec<u32>,
    dep_items: Vec<u32>,
    rank: Vec<u32>,
    slot: Vec<u32>,
    component: Vec<u32>,
    comp_starts: Vec<u32>,
    /// Nodes grouped by component, each group in (rank, slot) order.
    comp_items: Vec<u32>,
    dangling: usize,
    cyclic: usize,
}

impl Graph {
    pub fn build(row_ids: &[u32], prev: &[[u32; 3]]) -> Self {
        let n = row_ids.len();
        let mut prereq_starts = Vec::with_capacity(n + 1);
        let mut prereq_items: Vec<u32> = Vec::with_capacity(n);
        let mut dangling = 0;
        for slots in prev {
            let start = prereq_items.len();
            prereq_starts.push(start as u32);
            for value in slots.iter().copied().filter(|v| *v != 0) {
                match row_ids.binary_search(&value) {
                    Ok(at) if !prereq_items[start..].contains(&(at as u32)) => {
                        prereq_items.push(at as u32);
                    }
                    Ok(_) => {}
                    Err(_) => dangling += 1,
                }
            }
        }
        prereq_starts.push(prereq_items.len() as u32);

        let mut dep_starts = vec![0u32; n + 1];
        for prereq in &prereq_items {
            dep_starts[*prereq as usize + 1] += 1;
        }
        for i in 0..n {
            dep_starts[i + 1] += dep_starts[i];
        }
        let mut fill = dep_starts.clone();
        let mut dep_items = vec![0u32; prereq_items.len()];
        for node in 0..n {
            for prereq in &prereq_items[csr(&prereq_starts, node)] {
                dep_items[fill[*prereq as usize] as usize] = node as u32;
                fill[*prereq as usize] += 1;
            }
        }

        let mut rank = vec![0u32; n];
        let mut indegree: Vec<u32> = (0..n)
            .map(|node| prereq_starts[node + 1] - prereq_starts[node])
            .collect();
        let mut queue: Vec<u32> = (0..n as u32)
            .filter(|node| indegree[*node as usize] == 0)
            .collect();
        let mut popped = 0;
        while let Some(node) = queue.pop() {
            popped += 1;
            for dependent in &dep_items[csr(&dep_starts, node as usize)] {
                let dependent = *dependent as usize;
                rank[dependent] = rank[dependent].max(rank[node as usize] + 1);
                indegree[dependent] -= 1;
                if indegree[dependent] == 0 {
                    queue.push(dependent as u32);
                }
            }
        }

        let mut parent: Vec<u32> = (0..n as u32).collect();
        for node in 0..n {
            for prereq in &prereq_items[csr(&prereq_starts, node)] {
                union(&mut parent, node as u32, *prereq);
            }
        }
        let mut label = vec![u32::MAX; n];
        let mut sizes: Vec<u32> = Vec::new();
        for node in 0..n {
            let root = find(&mut parent, node as u32) as usize;
            if label[root] == u32::MAX {
                label[root] = sizes.len() as u32;
                sizes.push(0);
            }
            sizes[label[root] as usize] += 1;
        }
        let mut by_size: Vec<u32> = (0..sizes.len() as u32).collect();
        by_size.sort_unstable_by_key(|c| (std::cmp::Reverse(sizes[*c as usize]), *c));
        let mut rename = vec![0u32; sizes.len()];
        for (to, from) in by_size.iter().enumerate() {
            rename[*from as usize] = to as u32;
        }
        let component: Vec<u32> = (0..n)
            .map(|node| rename[label[find(&mut parent, node as u32) as usize] as usize])
            .collect();

        let mut order: Vec<u32> = (0..n as u32).collect();
        order.sort_unstable_by_key(|node| {
            (
                component[*node as usize],
                rank[*node as usize],
                row_ids[*node as usize],
            )
        });
        // The barycenter only orders each rank; placing nodes at it lets the ribbon drift sideways
        // without bound, since every rank pushes its own nodes away from a collision one way.
        let mut slot = vec![0u32; n];
        let mut group: Vec<(f32, u32)> = Vec::new();
        let mut at = 0;
        while at < order.len() {
            let key = |node: u32| (component[node as usize], rank[node as usize]);
            let end = order[at..]
                .iter()
                .position(|node| key(*node) != key(order[at]))
                .map_or(order.len(), |offset| at + offset);
            group.clear();
            group.extend(order[at..end].iter().map(|node| {
                let prereqs = &prereq_items[csr(&prereq_starts, *node as usize)];
                let mean = prereqs.iter().map(|p| slot[*p as usize] as f32).sum::<f32>()
                    / prereqs.len().max(1) as f32;
                (mean, *node)
            }));
            group.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            for (at_slot, (_, node)) in group.iter().enumerate() {
                slot[*node as usize] = at_slot as u32;
                order[at + at_slot] = *node;
            }
            at = end;
        }

        let mut comp_starts = vec![0u32; sizes.len() + 1];
        for c in &component {
            comp_starts[*c as usize + 1] += 1;
        }
        for c in 0..sizes.len() {
            comp_starts[c + 1] += comp_starts[c];
        }

        Self {
            prereq_starts,
            prereq_items,
            dep_starts,
            dep_items,
            rank,
            slot,
            component,
            comp_starts,
            comp_items: order,
            dangling,
            cyclic: n - popped,
        }
    }

    pub fn len(&self) -> usize {
        self.rank.len()
    }

    pub fn edge_count(&self) -> usize {
        self.prereq_items.len()
    }

    /// Non-zero `PreviousQuest` values naming a row the quest list does not hold.
    pub fn dangling(&self) -> usize {
        self.dangling
    }

    /// Nodes a topological sort could not reach, so nonzero only if the sheet gained a cycle.
    pub fn cyclic(&self) -> usize {
        self.cyclic
    }

    pub fn prereqs(&self, node: u32) -> &[u32] {
        &self.prereq_items[csr(&self.prereq_starts, node as usize)]
    }

    pub fn dependents(&self, node: u32) -> &[u32] {
        &self.dep_items[csr(&self.dep_starts, node as usize)]
    }

    pub fn rank(&self, node: u32) -> u32 {
        self.rank[node as usize]
    }

    pub fn slot(&self, node: u32) -> u32 {
        self.slot[node as usize]
    }

    pub fn component(&self, node: u32) -> u32 {
        self.component[node as usize]
    }

    pub fn component_count(&self) -> usize {
        self.comp_starts.len() - 1
    }

    /// One component's nodes, largest component first, each in (rank, slot) order.
    pub fn component_nodes(&self, component: u32) -> &[u32] {
        &self.comp_items[csr(&self.comp_starts, component as usize)]
    }

    /// The nodes of `component` in the given rank window, for painting only what is on screen.
    pub fn ranked_slice(&self, component: u32, ranks: Range<u32>) -> &[u32] {
        let nodes = self.component_nodes(component);
        let start = nodes.partition_point(|node| self.rank(*node) < ranks.start);
        let end = nodes.partition_point(|node| self.rank(*node) < ranks.end);
        &nodes[start..end]
    }

    /// The last rank and slot a component occupies, so the canvas knows how big it is.
    pub fn extent(&self, component: u32) -> (u32, u32) {
        self.component_nodes(component)
            .iter()
            .fold((0, 0), |(rank, slot), node| {
                (rank.max(self.rank(*node)), slot.max(self.slot(*node)))
            })
    }
}

fn csr(starts: &[u32], at: usize) -> Range<usize> {
    starts[at] as usize..starts[at + 1] as usize
}

fn find(parent: &mut [u32], node: u32) -> u32 {
    let mut root = node;
    while parent[root as usize] != root {
        root = parent[root as usize];
    }
    let mut walk = node;
    while parent[walk as usize] != root {
        walk = std::mem::replace(&mut parent[walk as usize], root);
    }
    root
}

fn union(parent: &mut [u32], a: u32, b: u32) {
    let (a, b) = (find(parent, a), find(parent, b));
    if a != b {
        parent[a.max(b) as usize] = a.min(b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_by_longest_path_and_splits_components() {
        // 10 → 20 → 40, 10 → 40 (the long way wins), 30 alone.
        let row_ids = [10, 20, 30, 40];
        let graph = Graph::build(&row_ids, &[[0; 3], [10, 0, 0], [0; 3], [10, 20, 0]]);

        assert_eq!(graph.edge_count(), 3);
        assert_eq!(graph.dangling(), 0);
        assert_eq!(graph.cyclic(), 0);
        assert_eq!(
            (graph.rank(0), graph.rank(1), graph.rank(3)),
            (0, 1, 2),
            "40 sits below 20, not beside it"
        );
        assert_eq!(graph.dependents(0), [1, 3]);
        assert_eq!(graph.prereqs(3), [0, 1]);

        assert_eq!(graph.component_count(), 2);
        assert_eq!(graph.component_nodes(0), [0, 1, 3], "largest first");
        assert_eq!(graph.component_nodes(1), [2]);
        assert_eq!(graph.ranked_slice(0, 1..2), [1]);
        assert_eq!(graph.extent(0), (2, 0));
    }

    #[test]
    fn a_repeated_slot_is_one_edge() {
        let graph = Graph::build(&[10, 20], &[[0; 3], [10, 10, 0]]);
        assert_eq!(graph.prereqs(1), [0]);
        assert_eq!(graph.cyclic(), 0, "or the double count strands the node");
    }

    #[test]
    fn a_rank_packs_its_nodes_in_barycenter_order() {
        // 20 and 30 share rank 1; 50 follows 30 and 60 follows 20, so rank 2 has to swap them back.
        let row_ids = [10, 20, 30, 50, 60];
        let graph = Graph::build(
            &row_ids,
            &[[0; 3], [10, 0, 0], [10, 0, 0], [30, 0, 0], [20, 0, 0]],
        );

        assert_eq!((graph.slot(1), graph.slot(2)), (0, 1));
        assert_eq!((graph.slot(3), graph.slot(4)), (1, 0), "children follow their parent across");
        assert_eq!(graph.extent(0), (2, 1), "and no rank is wider than its own nodes");
    }

    #[test]
    fn a_missing_prerequisite_is_counted_not_linked() {
        let graph = Graph::build(&[10, 20], &[[0; 3], [99, 0, 0]]);
        assert_eq!(graph.dangling(), 1);
        assert_eq!(graph.edge_count(), 0);
        assert_eq!(graph.component_count(), 2);
    }
}
