//! Graph algorithms independent of processors and buffers.

use std::collections::VecDeque;

/// A cycle prevented ordering; contains node indices along one cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyError {
    pub cycle: Vec<usize>,
}

/// Kahn's algorithm over `node_count` nodes and `(from, to)` edges.
///
/// The order is deterministic: among ready nodes the lowest index goes
/// first. On failure, one concrete cycle is returned for diagnostics.
pub fn topological_order(
    node_count: usize,
    edges: &[(usize, usize)],
) -> Result<Vec<usize>, TopologyError> {
    let mut indegree = vec![0usize; node_count];
    let mut adjacency = vec![Vec::new(); node_count];
    for &(from, to) in edges {
        adjacency[from].push(to);
        indegree[to] += 1;
    }
    for list in &mut adjacency {
        list.sort_unstable();
    }
    let mut ready: VecDeque<usize> = (0..node_count).filter(|&n| indegree[n] == 0).collect();
    let mut order = Vec::with_capacity(node_count);
    while let Some(n) = ready.pop_front() {
        order.push(n);
        for &m in &adjacency[n] {
            indegree[m] -= 1;
            if indegree[m] == 0 {
                ready.push_back(m);
            }
        }
    }
    if order.len() == node_count {
        Ok(order)
    } else {
        Err(TopologyError {
            cycle: find_cycle(&adjacency, &indegree),
        })
    }
}

/// Find one cycle among nodes that Kahn's algorithm could not order.
fn find_cycle(adjacency: &[Vec<usize>], indegree: &[usize]) -> Vec<usize> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        New,
        OnStack,
        Done,
    }
    let n = adjacency.len();
    let mut mark = vec![Mark::New; n];
    let mut stack: Vec<usize> = Vec::new();
    for start in (0..n).filter(|&s| indegree[s] > 0) {
        if mark[start] != Mark::New {
            continue;
        }
        // Iterative DFS keeping (node, next child index).
        let mut frames: Vec<(usize, usize)> = vec![(start, 0)];
        mark[start] = Mark::OnStack;
        stack.push(start);
        while let Some(&mut (node, ref mut child)) = frames.last_mut() {
            if let Some(&next) = adjacency[node].get(*child) {
                *child += 1;
                match mark[next] {
                    Mark::OnStack => {
                        let pos = stack.iter().position(|&x| x == next).unwrap_or(0);
                        return stack[pos..].to_vec();
                    }
                    Mark::New => {
                        mark[next] = Mark::OnStack;
                        stack.push(next);
                        frames.push((next, 0));
                    }
                    Mark::Done => {}
                }
            } else {
                mark[node] = Mark::Done;
                stack.pop();
                frames.pop();
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_dag_deterministically() {
        // 0 → 2, 1 → 2, 2 → 4, 3 → 4
        let order = topological_order(5, &[(0, 2), (1, 2), (2, 4), (3, 4)]).unwrap();
        assert_eq!(order, vec![0, 1, 3, 2, 4]);
    }

    #[test]
    fn reports_cycle() {
        // 0 → 1 → 2 → 3 → 1, 4 isolated
        let err = topological_order(5, &[(0, 1), (1, 2), (2, 3), (3, 1)]).unwrap_err();
        let mut c = err.cycle.clone();
        c.sort_unstable();
        assert_eq!(c, vec![1, 2, 3]);
    }

    #[test]
    fn self_loop_is_a_cycle() {
        let err = topological_order(2, &[(1, 1)]).unwrap_err();
        assert_eq!(err.cycle, vec![1]);
    }
}
