//! Blocks every lane runs: the regions of per-lane branches (the blocks a lane branch reaches
//! that do not post-dominate it, and the loops it leaves), where two host waves of one console
//! wave cannot meet. A child module of the lowering.

use std::collections::HashMap;

use super::{Atom, LaneExit, Structured};

/// Per IR block: every lane of the wave that started the program runs it, as often as the others
/// – no per-lane branch separates it from the entry (it lies in no lane branch's region: the
/// blocks reachable from the branch that do not post-dominate it, and the blocks of a loop the
/// branch leaves). None with an indirect exit (not followed).
pub(super) fn all_lane_blocks(st: &Structured) -> Option<Vec<bool>> {
    let n = st.exits.len();
    let mut all = vec![true; n];
    for (_, region) in lane_regions(st)? {
        for b in (0..n).filter(|&b| region[b]) {
            all[b] = false;
        }
    }
    Some(all)
}

/// Every per-lane branch (by its block) with its region: the blocks reachable from it that do
/// not post-dominate it, and those of a loop it leaves, which only some lanes run (or run
/// again). Not the branches the wave takes together
/// ([`Structured::wide`]), nor those on scalar conditions. None with an indirect exit.
pub(super) fn lane_regions(st: &Structured) -> Option<Vec<(usize, Vec<bool>)>> {
    let n = st.exits.len();
    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (b, e) in st.exits.iter().enumerate() {
        match *e {
            LaneExit::Jump(t) => succ[b].push(t),
            LaneExit::Branch {
                taken, not_taken, ..
            } => {
                succ[b].push(taken);
                succ[b].push(not_taken);
            }
            LaneExit::End => {}
            LaneExit::Indirect => return None,
        }
    }
    // post-dominators (bitsets; a virtual exit after every End)
    let words = n.div_ceil(64);
    let full = vec![u64::MAX; words];
    let mut pdom: Vec<Vec<u64>> = (0..n)
        .map(|b| {
            if succ[b].is_empty() {
                let mut v = vec![0; words];
                v[b / 64] |= 1 << (b % 64);
                v
            } else {
                full.clone()
            }
        })
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..n).rev() {
            if succ[b].is_empty() {
                continue;
            }
            let mut v = full.clone();
            for &t in &succ[b] {
                for w in 0..words {
                    v[w] &= pdom[t][w];
                }
            }
            v[b / 64] |= 1 << (b % 64);
            if v != pdom[b] {
                pdom[b] = v;
                changed = true;
            }
        }
    }
    let loops = natural_loops(&succ);
    let mut regions = Vec::new();
    for (d, e) in st.exits.iter().enumerate() {
        let LaneExit::Branch { when, .. } = *e else {
            continue;
        };
        if st.wide.contains(&d)
            || st
                .bools
                .support(when)
                .iter()
                .all(|a| matches!(a, Atom::Uniform(_)))
        {
            continue; // a wave-wide or uniform branch: all lanes go the same way
        }
        // blocks reachable from the lane branch that do not post-dominate it
        let mut seen = vec![false; n];
        let mut stack: Vec<usize> = succ[d].clone();
        while let Some(b) = stack.pop() {
            if seen[b] || pdom[d][b / 64] >> (b % 64) & 1 == 1 {
                continue;
            }
            seen[b] = true;
            stack.extend(succ[b].iter().copied());
        }
        // A lane branch that leaves a loop (its own back edge, a break): each lane leaves on
        // its own, and the loop's blocks, the branch's own among them, are run again only by
        // the lanes that stayed, though some post-dominate it.
        for body in loops
            .iter()
            .filter(|l| l[d] && succ[d].iter().any(|&s| !l[s]))
        {
            for b in (0..n).filter(|&b| body[b]) {
                seen[b] = true;
            }
        }
        regions.push((d, seen));
    }
    Some(regions)
}

/// The natural loops of the blocks `succ` links (each block's successors, block 0 the entry),
/// one per header: the header and every block that reaches one of its back edges without
/// passing through it. A back edge goes to a block no later in reverse postorder (the program
/// is reducible: `ir::build` splits irreducible loops).
fn natural_loops(succ: &[Vec<usize>]) -> Vec<Vec<bool>> {
    let n = succ.len();
    // reverse postorder of the reachable blocks
    let mut rpo = vec![usize::MAX; n];
    let (mut seen, mut post, mut stack) = (vec![false; n], Vec::new(), vec![(0usize, 0usize)]);
    if n > 0 {
        seen[0] = true;
    }
    while let Some(top) = stack.last_mut() {
        let (b, i) = *top;
        top.1 += 1;
        match succ[b].get(i) {
            Some(&s) if !seen[s] => {
                seen[s] = true;
                stack.push((s, 0));
            }
            Some(_) => {}
            None => {
                post.push(b);
                stack.pop();
            }
        }
    }
    for (k, &b) in post.iter().rev().enumerate() {
        rpo[b] = k;
    }
    let mut pred: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (b, s) in succ.iter().enumerate() {
        for &t in s {
            pred[t].push(b);
        }
    }
    let mut loops: HashMap<usize, Vec<bool>> = HashMap::new();
    for (q, s) in succ.iter().enumerate() {
        for &h in s
            .iter()
            .filter(|&&h| rpo[q] != usize::MAX && rpo[h] <= rpo[q])
        {
            let body = loops.entry(h).or_insert_with(|| {
                let mut v = vec![false; n];
                v[h] = true;
                v
            });
            let mut stack = vec![q];
            while let Some(b) = stack.pop() {
                if !std::mem::replace(&mut body[b], true) {
                    stack.extend(pred[b].iter().copied());
                }
            }
        }
    }
    loops.into_values().collect()
}
