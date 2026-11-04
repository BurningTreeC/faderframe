//! The 32 classic six-operator algorithms, written from their published
//! diagrams: which operators modulate which, which are heard (carriers),
//! and where the feedback loop is. Operators are numbered 1–6 in the
//! diagrams and indexed 0–5 here; every modulation goes from a higher
//! number to a lower one, so computing 6 down to 1 has every modulator's
//! output ready (a loop's feedback is a sample late, as on the original).

/// One algorithm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Algorithm {
    /// Per operator, the operators modulating it (bit `i` = operator
    /// `i + 1`).
    pub mods: [u8; 6],
    /// The operators heard (bit per operator).
    pub carriers: u8,
    /// The feedback loop: from this operator's output into that one's
    /// phase (the same operator for a plain self-feedback).
    pub feedback: (usize, usize),
}

impl Algorithm {
    pub fn is_carrier(&self, op: usize) -> bool {
        self.carriers >> op & 1 == 1
    }

    /// Whether operator `from` modulates `to`.
    pub fn modulates(&self, from: usize, to: usize) -> bool {
        self.mods[to] >> from & 1 == 1
    }
}

/// An algorithm from its edges (`from → to`, operator numbers 1–6), its
/// carriers and its feedback loop (`from → to`).
const fn alg(edges: &[(usize, usize)], carriers: &[usize], feedback: (usize, usize)) -> Algorithm {
    let mut mods = [0u8; 6];
    let mut i = 0;
    while i < edges.len() {
        let (from, to) = edges[i];
        mods[to - 1] |= 1 << (from - 1);
        i += 1;
    }
    let mut c = 0u8;
    let mut i = 0;
    while i < carriers.len() {
        c |= 1 << (carriers[i] - 1);
        i += 1;
    }
    Algorithm {
        mods,
        carriers: c,
        feedback: (feedback.0 - 1, feedback.1 - 1),
    }
}

/// Algorithms 1–32 (index 0–31).
pub const ALGORITHMS: [Algorithm; 32] = [
    // 1–2: a pair and a stack of four.
    alg(&[(2, 1), (4, 3), (5, 4), (6, 5)], &[1, 3], (6, 6)),
    alg(&[(2, 1), (4, 3), (5, 4), (6, 5)], &[1, 3], (2, 2)),
    // 3–4: two stacks of three (4: the loop round 4–6).
    alg(&[(2, 1), (3, 2), (5, 4), (6, 5)], &[1, 4], (6, 6)),
    alg(&[(2, 1), (3, 2), (5, 4), (6, 5)], &[1, 4], (4, 6)),
    // 5–6: three pairs (6: the loop round 5–6).
    alg(&[(2, 1), (4, 3), (6, 5)], &[1, 3, 5], (6, 6)),
    alg(&[(2, 1), (4, 3), (6, 5)], &[1, 3, 5], (5, 6)),
    // 7–9: a pair, and 3 under 4 and the pair 5–6.
    alg(&[(2, 1), (4, 3), (5, 3), (6, 5)], &[1, 3], (6, 6)),
    alg(&[(2, 1), (4, 3), (5, 3), (6, 5)], &[1, 3], (4, 4)),
    alg(&[(2, 1), (4, 3), (5, 3), (6, 5)], &[1, 3], (2, 2)),
    // 10–11: a stack of three, and 4 under 5 and 6.
    alg(&[(2, 1), (3, 2), (5, 4), (6, 4)], &[1, 4], (3, 3)),
    alg(&[(2, 1), (3, 2), (5, 4), (6, 4)], &[1, 4], (6, 6)),
    // 12–13: a pair, and 3 under 4, 5 and 6.
    alg(&[(2, 1), (4, 3), (5, 3), (6, 3)], &[1, 3], (2, 2)),
    alg(&[(2, 1), (4, 3), (5, 3), (6, 3)], &[1, 3], (6, 6)),
    // 14–15: a pair, and 3 under 4 under 5 and 6.
    alg(&[(2, 1), (4, 3), (5, 4), (6, 4)], &[1, 3], (6, 6)),
    alg(&[(2, 1), (4, 3), (5, 4), (6, 4)], &[1, 3], (2, 2)),
    // 16–17: one carrier under 2, the pair 3–4 and the pair 5–6.
    alg(&[(2, 1), (3, 1), (4, 3), (5, 1), (6, 5)], &[1], (6, 6)),
    alg(&[(2, 1), (3, 1), (4, 3), (5, 1), (6, 5)], &[1], (2, 2)),
    // 18: one carrier under 2, 3 and the stack 4–6.
    alg(&[(2, 1), (3, 1), (4, 1), (5, 4), (6, 5)], &[1], (3, 3)),
    // 19: a stack of three, and 6 over the carriers 4 and 5.
    alg(&[(2, 1), (3, 2), (6, 4), (6, 5)], &[1, 4, 5], (6, 6)),
    // 20: 3 over the carriers 1 and 2; 4 under 5 and 6.
    alg(&[(3, 1), (3, 2), (5, 4), (6, 4)], &[1, 2, 4], (3, 3)),
    // 21: 3 over 1 and 2; 6 over 4 and 5.
    alg(&[(3, 1), (3, 2), (6, 4), (6, 5)], &[1, 2, 4, 5], (3, 3)),
    // 22: a pair; 6 over 3, 4 and 5.
    alg(&[(2, 1), (6, 3), (6, 4), (6, 5)], &[1, 3, 4, 5], (6, 6)),
    // 23: 1 alone, the pair 2–3, 6 over 4 and 5.
    alg(&[(3, 2), (6, 4), (6, 5)], &[1, 2, 4, 5], (6, 6)),
    // 24–25: 6 over three (two) of five carriers.
    alg(&[(6, 3), (6, 4), (6, 5)], &[1, 2, 3, 4, 5], (6, 6)),
    alg(&[(6, 4), (6, 5)], &[1, 2, 3, 4, 5], (6, 6)),
    // 26–27: 1 alone, the pair 2–3, 4 under 5 and 6.
    alg(&[(3, 2), (5, 4), (6, 4)], &[1, 2, 4], (6, 6)),
    alg(&[(3, 2), (5, 4), (6, 4)], &[1, 2, 4], (3, 3)),
    // 28: a pair, a stack of three, 6 alone.
    alg(&[(2, 1), (4, 3), (5, 4)], &[1, 3, 6], (5, 5)),
    // 29: 1 and 2 alone, two pairs.
    alg(&[(4, 3), (6, 5)], &[1, 2, 3, 5], (6, 6)),
    // 30: 1 and 2 alone, a stack of three, 6 alone.
    alg(&[(4, 3), (5, 4)], &[1, 2, 3, 6], (5, 5)),
    // 31: five carriers, the last under 6.
    alg(&[(6, 5)], &[1, 2, 3, 4, 5], (6, 6)),
    // 32: six carriers (an organ of sines).
    alg(&[], &[1, 2, 3, 4, 5, 6], (6, 6)),
];

/// Where an algorithm's operators sit when drawn: per operator its row
/// (0: the carriers, at the bottom) and column.
pub fn layout(a: &Algorithm) -> [(usize, f32); 6] {
    let mut row = [0usize; 6];
    // Higher operators only modulate lower ones: settle rows from 1 up.
    for op in 0..6 {
        if a.is_carrier(op) {
            continue;
        }
        let below = (0..op)
            .filter(|t| a.modulates(op, *t))
            .map(|t| row[t])
            .max();
        row[op] = below.map_or(0, |r| r + 1);
    }
    let mut col = [0f32; 6];
    let mut next = 0f32;
    for (op, c) in col.iter_mut().enumerate() {
        if a.is_carrier(op) {
            *c = next;
            next += 1.0;
        }
    }
    let first = |op: usize| (0..op).find(|t| a.modulates(op, *t));
    for op in 0..6 {
        if a.is_carrier(op) {
            continue;
        }
        // Over the first operator it modulates, beside its other
        // modulators.
        match first(op) {
            Some(t) => {
                let siblings = (0..op)
                    .filter(|o| !a.is_carrier(*o) && first(*o) == Some(t))
                    .count();
                col[op] = col[t] + siblings as f32;
            }
            None => {
                col[op] = next;
                next += 1.0;
            }
        }
    }
    std::array::from_fn(|op| (row[op], col[op]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_algorithm_modulates_downwards_and_has_a_carrier() {
        for (n, a) in ALGORITHMS.iter().enumerate() {
            assert_ne!(a.carriers, 0, "algorithm {}", n + 1);
            for to in 0..6 {
                for from in 0..6 {
                    if a.modulates(from, to) {
                        assert!(from > to, "algorithm {}: {from} → {to}", n + 1);
                        assert!(!a.is_carrier(from), "a carrier modulating");
                    }
                }
                // Every operator is heard or modulates something heard.
                if !a.is_carrier(to) {
                    assert!(
                        (0..to).any(|t| a.modulates(to, t)),
                        "algorithm {}: operator {} does nothing",
                        n + 1,
                        to + 1
                    );
                }
            }
        }
        // Spot checks against the diagrams.
        assert_eq!(ALGORITHMS[0].carriers, 0b000101);
        assert_eq!(ALGORITHMS[3].feedback, (3, 5));
        assert_eq!(ALGORITHMS[31].carriers, 0b111111);
        assert_eq!(ALGORITHMS[15].mods[0], 0b10110);
        let l = layout(&ALGORITHMS[0]);
        assert_eq!(l[5].0, 3, "operator 6 tops the stack");
        assert_eq!(l[0].0, 0);
    }
}
