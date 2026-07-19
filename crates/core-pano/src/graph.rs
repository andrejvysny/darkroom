//! Match graph: connected components over verified pairs, then a maximum-inlier spanning tree.
//!
//! Panorama frames form a graph whose edges are verified overlaps. We keep the *largest connected
//! component* (a shoot can contain unrelated stragglers) and build a maximum-spanning tree over it so
//! pose seeding chains through the most-confident overlaps. The tree root is the frame with the most
//! total inlier support — the "central" frame — which becomes the identity-rotation reference.

use crate::PanoError;

/// An undirected verified edge between two frames, weighted by inlier count.
pub(crate) struct Edge {
    pub a: usize,
    pub b: usize,
    pub weight: usize,
    /// Index into the caller's `pairwise` vector, so pose seeding can fetch the homography.
    pub pair_idx: usize,
}

/// A directed spanning-tree step: `child`'s pose is derived from `parent`'s via `pair_idx`.
pub(crate) struct TreeEdge {
    pub parent: usize,
    pub child: usize,
    pub pair_idx: usize,
}

pub(crate) struct SpanningTree {
    /// Frames in the largest connected component, ascending. Length ≥ 2.
    pub used: Vec<usize>,
    /// Root frame (max total inlier support) — the identity-rotation reference.
    pub reference: usize,
    /// Parent→child steps in the order they join the tree (each parent already has a pose).
    pub order: Vec<TreeEdge>,
}

/// Simple union-find over `n` elements.
struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        UnionFind {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }
    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        // Path compression.
        let mut cur = x;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }
    fn union(&mut self, x: usize, y: usize) {
        let (rx, ry) = (self.find(x), self.find(y));
        if rx == ry {
            return;
        }
        if self.rank[rx] < self.rank[ry] {
            self.parent[rx] = ry;
        } else if self.rank[rx] > self.rank[ry] {
            self.parent[ry] = rx;
        } else {
            self.parent[ry] = rx;
            self.rank[rx] += 1;
        }
    }
}

/// Build the largest-component maximum-inlier spanning tree. Errors with `TooFewMatched` if the
/// biggest component has fewer than two frames.
pub(crate) fn largest_component_tree(n: usize, edges: &[Edge]) -> Result<SpanningTree, PanoError> {
    let mut uf = UnionFind::new(n);
    for e in edges {
        uf.union(e.a, e.b);
    }

    // Group frames by component root; pick the largest (ties: lowest member index for determinism).
    let mut comp_members: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for v in 0..n {
        let r = uf.find(v);
        comp_members.entry(r).or_default().push(v);
    }
    let mut best: Option<Vec<usize>> = None;
    for members in comp_members.values() {
        let take = match &best {
            None => true,
            Some(b) => {
                members.len() > b.len()
                    || (members.len() == b.len() && members.iter().min() < b.iter().min())
            }
        };
        if take {
            let mut m = members.clone();
            m.sort_unstable();
            best = Some(m);
        }
    }
    let used = best.unwrap_or_default();
    if used.len() < 2 {
        return Err(PanoError::TooFewMatched(used.len()));
    }

    let in_component: std::collections::HashSet<usize> = used.iter().copied().collect();

    // Adjacency restricted to the chosen component.
    let mut adj: std::collections::HashMap<usize, Vec<(usize, usize, usize)>> =
        std::collections::HashMap::new(); // node -> [(neighbor, weight, pair_idx)]
    let mut total_inliers: std::collections::HashMap<usize, usize> =
        std::collections::HashMap::new();
    for e in edges {
        if in_component.contains(&e.a) && in_component.contains(&e.b) {
            adj.entry(e.a)
                .or_default()
                .push((e.b, e.weight, e.pair_idx));
            adj.entry(e.b)
                .or_default()
                .push((e.a, e.weight, e.pair_idx));
            *total_inliers.entry(e.a).or_default() += e.weight;
            *total_inliers.entry(e.b).or_default() += e.weight;
        }
    }

    // Reference = frame with max total inlier support (ties: lowest index).
    let mut reference = used[0];
    let mut best_support = 0usize;
    for &v in &used {
        let s = *total_inliers.get(&v).unwrap_or(&0);
        if s > best_support {
            best_support = s;
            reference = v;
        }
    }

    // Prim's algorithm, maximizing edge weight, from the reference.
    let mut in_tree: std::collections::HashSet<usize> = std::collections::HashSet::new();
    in_tree.insert(reference);
    let mut order: Vec<TreeEdge> = Vec::with_capacity(used.len() - 1);
    while in_tree.len() < used.len() {
        // Pick the heaviest edge crossing the frontier (ties: lowest child then parent index).
        let mut best_edge: Option<(usize, usize, usize, usize)> = None; // (weight, parent, child, pair_idx)
        for &p in in_tree.iter() {
            if let Some(neighbors) = adj.get(&p) {
                for &(nb, w, pi) in neighbors {
                    if in_tree.contains(&nb) {
                        continue;
                    }
                    let candidate = (w, p, nb, pi);
                    let take = match best_edge {
                        None => true,
                        Some((bw, bp, bc, _)) => {
                            w > bw || (w == bw && (nb < bc || (nb == bc && p < bp)))
                        }
                    };
                    if take {
                        best_edge = Some(candidate);
                    }
                }
            }
        }
        // The component is connected, so an edge always exists until the tree is complete.
        let (_, parent, child, pair_idx) = best_edge.expect("connected component must span");
        in_tree.insert(child);
        order.push(TreeEdge {
            parent,
            child,
            pair_idx,
        });
    }

    Ok(SpanningTree {
        used,
        reference,
        order,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_largest_component_and_does_not_error_at_two() {
        // 4 frames; only (0,1) and (2,3) verify. Both components have size 2; the tie is broken by
        // lowest member index, so {0,1} is kept. Two frames is enough — no TooFewMatched.
        let edges = vec![
            Edge {
                a: 0,
                b: 1,
                weight: 50,
                pair_idx: 0,
            },
            Edge {
                a: 2,
                b: 3,
                weight: 40,
                pair_idx: 1,
            },
        ];
        let tree = largest_component_tree(4, &edges).expect("2-frame component must not error");
        assert_eq!(tree.used, vec![0, 1]);
        assert_eq!(tree.reference, 0); // both have 50 inliers; tie -> lowest index
        assert_eq!(tree.order.len(), 1);
        let e = &tree.order[0];
        assert_eq!((e.parent, e.child, e.pair_idx), (0, 1, 0));
    }

    #[test]
    fn too_few_matched_when_nothing_connects() {
        let tree = largest_component_tree(3, &[]);
        assert!(matches!(tree, Err(PanoError::TooFewMatched(1))));
    }
}
