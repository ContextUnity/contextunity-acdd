//! Topological wave layering, Tarjan SCC cycle extraction, unblocks counting, and scope-overlap analysis.

use super::types::{SpecInfo, SpecWavesOutput};
use crate::usecases::dto::{RoadmapSpecEntry, RoadmapTaskEntry};
use acdd_core::path_policy::scopes_overlap;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

type KahnWavesResult = (
    BTreeMap<usize, Vec<String>>,
    BTreeMap<String, usize>,
    BTreeSet<String>,
);

pub(super) fn compute_unblocks_counts(
    specs_by_id: &BTreeMap<String, SpecInfo>,
    forward_spec_edges: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, (usize, usize)> {
    let mut unblocks_counts = BTreeMap::new();
    let enqueue_children =
        |node: &str, visited: &mut BTreeSet<String>, queue: &mut VecDeque<String>| {
            if let Some(children) = forward_spec_edges.get(node) {
                for c in children {
                    if visited.insert(c.clone()) {
                        queue.push_back(c.clone());
                    }
                }
            }
        };
    for spec_id in specs_by_id.keys() {
        let mut visited = BTreeSet::new();
        visited.insert(spec_id.clone());
        let mut queue = VecDeque::new();
        enqueue_children(spec_id, &mut visited, &mut queue);
        while let Some(curr) = queue.pop_front() {
            enqueue_children(&curr, &mut visited, &mut queue);
        }
        let spec_count = visited.len().saturating_sub(1);
        let task_count: usize = visited
            .iter()
            .filter(|id| id.as_str() != spec_id.as_str())
            .filter_map(|id| specs_by_id.get(id))
            .map(|s| s.doc.tasks.len())
            .sum();
        unblocks_counts.insert(spec_id.clone(), (spec_count, task_count));
    }
    unblocks_counts
}

pub(super) fn apply_spec_overlaps<'a>(
    entries: &mut [RoadmapSpecEntry],
    info: impl Fn(&RoadmapSpecEntry) -> &'a SpecInfo,
) {
    let n = entries.len();
    for i in 0..n {
        let mut overlaps = BTreeSet::new();
        let globs_i = &info(&entries[i]).effective_globs;
        for (j, other) in entries.iter().enumerate() {
            if i == j {
                continue;
            }
            let globs_j = &info(other).effective_globs;
            if !globs_i.is_empty() && !globs_j.is_empty() && scopes_overlap(globs_i, globs_j) {
                overlaps.insert(other.spec_id.clone());
            }
        }
        entries[i].parallel_safe = overlaps.is_empty();
        entries[i].scope_overlaps = overlaps.into_iter().collect();
    }
}

fn kahn_propagate_waves(
    ids: &[String],
    mut in_degree: BTreeMap<String, usize>,
    adj: &BTreeMap<String, Vec<String>>,
    mut node_wave: BTreeMap<String, usize>,
) -> KahnWavesResult {
    let mut queue: VecDeque<String> = in_degree
        .iter()
        .filter(|&(_, &deg)| deg == 0)
        .map(|(id, _)| id.clone())
        .collect();
    let mut waves_by_idx: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut processed = BTreeSet::new();

    while let Some(u) = queue.pop_front() {
        processed.insert(u.clone());
        let w = node_wave[&u];
        waves_by_idx.entry(w).or_default().push(u.clone());
        if let Some(nexts) = adj.get(&u) {
            for v in nexts {
                let next_w = node_wave[v].max(w + 1);
                node_wave.insert(v.clone(), next_w);
                if let Some(d) = in_degree.get_mut(v) {
                    *d = d.saturating_sub(1);
                    if *d == 0 {
                        queue.push_back(v.clone());
                    }
                }
            }
        }
    }

    let remaining: BTreeSet<String> = ids
        .iter()
        .filter(|id| !processed.contains(id.as_str()))
        .cloned()
        .collect();
    (waves_by_idx, node_wave, remaining)
}

pub(super) fn compute_spec_waves(
    planned_ids: &[String],
    unsatisfied_blockers: &BTreeMap<String, BTreeSet<String>>,
    has_unsatisfied_external_blocker: &BTreeMap<String, bool>,
) -> SpecWavesOutput {
    let planned_set: BTreeSet<&str> = planned_ids.iter().map(String::as_str).collect();
    let mut in_degree: BTreeMap<String, usize> = BTreeMap::new();
    let mut adj: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut base_wave: BTreeMap<String, usize> = BTreeMap::new();

    for id in planned_ids {
        let mut deg = 0usize;
        let mut min_wave = usize::from(
            has_unsatisfied_external_blocker
                .get(id)
                .copied()
                .unwrap_or(false),
        );
        if let Some(blockers) = unsatisfied_blockers.get(id) {
            for b in blockers {
                if planned_set.contains(b.as_str()) {
                    deg += 1;
                    adj.entry(b.clone()).or_default().push(id.clone());
                } else {
                    // Any unsatisfied blocker outside `planned_ids` (in_progress, cancelled, etc.) keeps `id` out of Wave 0.
                    min_wave = min_wave.max(1);
                }
            }
        }
        in_degree.insert(id.clone(), deg);
        base_wave.insert(id.clone(), min_wave);
    }

    let (mut waves_by_idx, final_wave, remaining) =
        kahn_propagate_waves(planned_ids, in_degree, &adj, base_wave);
    let cycles = extract_true_cycles(&remaining, &adj);
    let cycle_members: BTreeSet<&str> = cycles.iter().flatten().map(String::as_str).collect();
    let non_cyclic: Vec<String> = remaining
        .iter()
        .filter(|id| !cycle_members.contains(id.as_str()))
        .cloned()
        .collect();
    if !non_cyclic.is_empty() {
        let non_cyclic_set: BTreeSet<&str> = non_cyclic.iter().map(String::as_str).collect();
        let mut res_in_deg: BTreeMap<String, usize> =
            non_cyclic.iter().map(|id| (id.clone(), 0usize)).collect();
        let mut res_adj: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let res_base_wave: BTreeMap<String, usize> = non_cyclic
            .iter()
            .map(|id| {
                let lb = final_wave.get(id).copied().unwrap_or(1).max(1);
                (id.clone(), lb)
            })
            .collect();
        for src in &non_cyclic {
            if let Some(nexts) = adj.get(src) {
                for dst in nexts {
                    if non_cyclic_set.contains(dst.as_str()) {
                        *res_in_deg.entry(dst.clone()).or_default() += 1;
                        res_adj.entry(src.clone()).or_default().push(dst.clone());
                    }
                }
            }
        }
        let (res_waves, _, _) =
            kahn_propagate_waves(&non_cyclic, res_in_deg, &res_adj, res_base_wave);
        for (w, ids) in res_waves {
            waves_by_idx.entry(w).or_default().extend(ids);
        }
    }

    (waves_by_idx.into_iter().collect(), cycles)
}

fn extract_true_cycles(
    remaining: &BTreeSet<String>,
    adj: &BTreeMap<String, Vec<String>>,
) -> Vec<Vec<String>> {
    if remaining.is_empty() {
        return Vec::new();
    }
    struct Tarjan<'a> {
        remaining: &'a BTreeSet<String>,
        adj: &'a BTreeMap<String, Vec<String>>,
        index: usize,
        indices: BTreeMap<String, usize>,
        lowlink: BTreeMap<String, usize>,
        stack: Vec<String>,
        on_stack: BTreeSet<String>,
        cycles: Vec<Vec<String>>,
    }
    impl<'a> Tarjan<'a> {
        fn strongconnect(&mut self, v: &str) {
            let v_idx = self.index;
            self.index += 1;
            self.indices.insert(v.to_owned(), v_idx);
            self.lowlink.insert(v.to_owned(), v_idx);
            self.stack.push(v.to_owned());
            self.on_stack.insert(v.to_owned());

            let mut self_loop = false;
            if let Some(neighbors) = self.adj.get(v) {
                for w in neighbors {
                    if !self.remaining.contains(w) {
                        continue;
                    }
                    if w == v {
                        self_loop = true;
                    }
                    if !self.indices.contains_key(w) {
                        self.strongconnect(w);
                        let low_w = self.lowlink[w];
                        if let Some(low_v) = self.lowlink.get_mut(v) {
                            *low_v = (*low_v).min(low_w);
                        }
                    } else if self.on_stack.contains(w) {
                        let idx_w = self.indices[w];
                        if let Some(low_v) = self.lowlink.get_mut(v) {
                            *low_v = (*low_v).min(idx_w);
                        }
                    }
                }
            }

            if self.lowlink[v] == v_idx {
                let mut comp = Vec::new();
                while let Some(w) = self.stack.pop() {
                    self.on_stack.remove(&w);
                    let done = w == v;
                    comp.push(w);
                    if done {
                        break;
                    }
                }
                if comp.len() > 1 || self_loop {
                    comp.sort();
                    self.cycles.push(comp);
                }
            }
        }
    }

    let mut tarjan = Tarjan {
        remaining,
        adj,
        index: 0,
        indices: BTreeMap::new(),
        lowlink: BTreeMap::new(),
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        cycles: Vec::new(),
    };
    for v in remaining {
        if !tarjan.indices.contains_key(v) {
            tarjan.strongconnect(v);
        }
    }
    tarjan.cycles.sort();
    tarjan.cycles
}

pub(super) fn compute_task_waves(
    task_ids: &[String],
    unsatisfied_local: &BTreeMap<String, BTreeSet<String>>,
    has_unsatisfied_ext: &BTreeMap<String, bool>,
) -> BTreeMap<String, usize> {
    let mut in_deg: BTreeMap<String, usize> = BTreeMap::new();
    let mut adj: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut base_wave: BTreeMap<String, usize> = BTreeMap::new();
    for id in task_ids {
        let blockers = unsatisfied_local.get(id);
        in_deg.insert(id.clone(), blockers.map_or(0, BTreeSet::len));
        base_wave.insert(
            id.clone(),
            usize::from(has_unsatisfied_ext.get(id).copied().unwrap_or(false)),
        );
        if let Some(bs) = blockers {
            for b in bs {
                adj.entry(b.clone()).or_default().push(id.clone());
            }
        }
    }
    let (_, task_wave, _) = kahn_propagate_waves(task_ids, in_deg, &adj, base_wave);
    task_wave
}

pub(super) fn apply_task_overlaps(entries: &mut [RoadmapTaskEntry]) {
    let n = entries.len();
    for i in 0..n {
        let mut overlaps = BTreeSet::new();
        for (j, other) in entries.iter().enumerate() {
            if i == j || entries[i].wave != other.wave {
                continue;
            }
            if !entries[i].scope.is_empty()
                && !other.scope.is_empty()
                && scopes_overlap(&entries[i].scope, &other.scope)
            {
                overlaps.insert(other.task_ref.clone());
            }
        }
        entries[i].scope_overlaps = overlaps.into_iter().collect();
    }
}

#[cfg(test)]
mod roadmap_graph_tests {
    use super::*;
    use std::path::PathBuf;

    fn blockers(rows: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
        rows.iter()
            .map(|(id, deps)| {
                (
                    (*id).to_owned(),
                    deps.iter().map(|dep| (*dep).to_owned()).collect(),
                )
            })
            .collect()
    }

    fn spec_info(id: &str, task_count: usize) -> SpecInfo {
        let tasks = (1..=task_count)
            .map(|index| {
                format!(
                    "### task: T-{index:02}\n```yaml\ntask_ref: T-{index:02}\nscope: [src/{id}/{index}/**]\nacceptance_criteria: [done]\n```\n"
                )
            })
            .collect::<String>();
        let doc = crate::spec::parse_spec(&format!(
            "---\nid: {id}\ntitle: {id}\nstatus: planned\n---\n{tasks}"
        ))
        .unwrap();
        SpecInfo {
            spec_id: id.to_owned(),
            collection: "milestones".to_owned(),
            path: format!("docs/milestones/{id}.md"),
            doc,
            checkout: PathBuf::new(),
            effective_globs: Vec::new(),
            is_archived: false,
            source_worktree: None,
            divergence: Vec::new(),
        }
    }

    #[test]
    fn pure_roadmap_graph_matrix() {
        struct Case {
            ids: Vec<&'static str>,
            blockers: Vec<(&'static str, Vec<&'static str>)>,
            external: Vec<&'static str>,
            waves: Vec<(usize, Vec<&'static str>)>,
            cycles: Vec<Vec<&'static str>>,
        }

        let cases = [
            Case {
                ids: vec!["A", "B", "C"],
                blockers: vec![("B", vec!["A"]), ("C", vec!["B"])],
                external: Vec::new(),
                waves: vec![(0, vec!["A"]), (1, vec!["B"]), (2, vec!["C"])],
                cycles: Vec::new(),
            },
            Case {
                ids: vec!["A", "B"],
                blockers: Vec::new(),
                external: vec!["B"],
                waves: vec![(0, vec!["A"]), (1, vec!["B"])],
                cycles: Vec::new(),
            },
            Case {
                ids: vec!["A", "C1", "C2", "D", "MIX-B", "MIX-1", "SELF"],
                blockers: vec![
                    ("C1", vec!["C2"]),
                    ("C2", vec!["C1"]),
                    ("D", vec!["C1"]),
                    ("MIX-B", vec!["A"]),
                    ("MIX-1", vec!["MIX-B", "C1"]),
                    ("SELF", vec!["SELF"]),
                ],
                external: Vec::new(),
                waves: vec![(0, vec!["A"]), (1, vec!["MIX-B", "D"]), (2, vec!["MIX-1"])],
                cycles: vec![vec!["C1", "C2"], vec!["SELF"]],
            },
        ];

        for case in cases {
            let ids: Vec<String> = case.ids.iter().map(|id| (*id).to_owned()).collect();
            let rows: Vec<(&str, &[&str])> = case
                .blockers
                .iter()
                .map(|(id, deps)| (*id, deps.as_slice()))
                .collect();
            let external = case
                .external
                .iter()
                .map(|id| ((*id).to_owned(), true))
                .collect();
            let (actual_waves, actual_cycles) =
                compute_spec_waves(&ids, &blockers(&rows), &external);
            let expected_waves: Vec<(usize, Vec<String>)> = case
                .waves
                .iter()
                .map(|(wave, ids)| (*wave, ids.iter().map(|id| (*id).to_owned()).collect()))
                .collect();
            let expected_cycles: Vec<Vec<String>> = case
                .cycles
                .iter()
                .map(|ids| ids.iter().map(|id| (*id).to_owned()).collect())
                .collect();
            assert_eq!(actual_waves, expected_waves);
            assert_eq!(actual_cycles, expected_cycles);
        }

        let task_waves = compute_task_waves(
            &["T-01".to_owned(), "T-02".to_owned(), "T-03".to_owned()],
            &blockers(&[("T-02", &["T-01"]), ("T-03", &["T-02"])]),
            &BTreeMap::new(),
        );
        assert_eq!(task_waves["T-01"], 0);
        assert_eq!(task_waves["T-02"], 1);
        assert_eq!(task_waves["T-03"], 2);

        let specs = [
            ("A".to_owned(), spec_info("A", 1)),
            ("B".to_owned(), spec_info("B", 2)),
            ("C".to_owned(), spec_info("C", 3)),
        ]
        .into_iter()
        .collect();
        let edges = BTreeMap::from([
            ("A".to_owned(), BTreeSet::from(["B".to_owned()])),
            ("B".to_owned(), BTreeSet::from(["C".to_owned()])),
        ]);
        assert_eq!(compute_unblocks_counts(&specs, &edges)["A"], (2, 5));
    }
}
