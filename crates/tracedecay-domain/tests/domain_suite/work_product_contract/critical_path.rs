use super::*;

fn project(items: Vec<WorkItemV1>) -> Result<(Vec<String>, u32), WorkProductContractError> {
    let graph = graph(items);
    let bundle = WorkProductProjectionBundleV1::from_graph(
        &graph,
        &runtime(&graph, UtcMicros(100), Vec::new()),
        UtcMicros(100),
    )?;
    Ok((
        bundle
            .critical_path()
            .task_ids()
            .iter()
            .map(|task_id| task_id.as_str().to_owned())
            .collect(),
        bundle.critical_path().total_effort(),
    ))
}

fn brute_force_critical_path(tasks: &[(String, Vec<String>, u32)]) -> (Vec<String>, u32) {
    fn extend(
        tasks: &[(String, Vec<String>, u32)],
        path: &mut Vec<String>,
        effort: u32,
        best: &mut Option<(Vec<String>, u32)>,
    ) {
        let better = best.as_ref().is_none_or(|(best_path, best_effort)| {
            effort > *best_effort || (effort == *best_effort && *path < *best_path)
        });
        if better {
            *best = Some((path.clone(), effort));
        }
        let last = path.last().unwrap().clone();
        for (task, dependencies, task_effort) in tasks {
            if dependencies.contains(&last) {
                path.push(task.clone());
                extend(tasks, path, effort + task_effort, best);
                path.pop();
            }
        }
    }
    let mut best = None;
    for (task, dependencies, effort) in tasks {
        if dependencies.is_empty() {
            extend(tasks, &mut vec![task.clone()], *effort, &mut best);
        }
    }
    best.unwrap_or_default()
}

#[test]
fn critical_path_matches_root_path_enumeration_at_joins_and_disconnected_roots() {
    let cases = [
        vec![
            ("task.z", vec![], 1),
            ("task.b", vec!["task.z"], 2),
            ("task.a", vec!["task.z"], 2),
            ("task.c", vec!["task.b", "task.a"], 1),
        ],
        vec![
            ("task.a", vec![], 1),
            ("task.z", vec!["task.a"], 1),
            ("task.b", vec!["task.a"], 2),
            ("task.c", vec!["task.b", "task.z"], 1),
        ],
        vec![
            ("task.a", vec![], 2),
            ("task.z", vec![], 1),
            ("task.b", vec!["task.z"], 1),
            ("task.c", vec!["task.a", "task.b"], 1),
        ],
        vec![
            ("task.a", vec![], 1),
            ("task.b", vec!["task.a"], 1),
            ("task.c", vec!["task.a"], 1),
            ("task.d", vec!["task.b", "task.c"], 1),
            ("task.z", vec![], 4),
        ],
    ];
    for case in cases {
        let tasks = case
            .iter()
            .map(|(task, dependencies, effort)| {
                (
                    (*task).to_owned(),
                    dependencies.iter().map(|id| (*id).to_owned()).collect(),
                    *effort,
                )
            })
            .collect::<Vec<_>>();
        let items = case
            .iter()
            .map(|(task, dependencies, effort)| item(task, dependencies, *effort))
            .collect();
        assert_eq!(
            project(items).unwrap(),
            brute_force_critical_path(&tasks),
            "{tasks:?}"
        );
    }
}

#[test]
fn critical_path_ties_break_on_the_whole_path_not_the_terminal_task() {
    let projected = project(vec![
        item("task.a", &[], 1),
        item("task.b", &[], 1),
        item("task.y", &["task.b"], 1),
        item("task.z", &["task.a"], 1),
        item("task.w", &[], 1),
    ])
    .unwrap();

    assert_eq!(
        projected,
        (vec!["task.a".to_owned(), "task.z".to_owned()], 2)
    );
}

#[test]
fn critical_path_effort_overflow_is_refused() {
    assert_eq!(
        project(vec![
            item("task.a", &[], u32::MAX),
            item("task.b", &["task.a"], 1),
        ])
        .unwrap_err(),
        WorkProductContractError::GraphTooLarge
    );
}

#[test]
fn a_dependency_chain_at_the_item_cap_projects_its_whole_path() {
    let task_ids = (0..MAX_WORK_PRODUCT_ITEMS)
        .map(|ordinal| format!("task.{ordinal:05}"))
        .collect::<Vec<_>>();
    let items = task_ids
        .iter()
        .enumerate()
        .map(|(ordinal, task_id)| {
            let dependencies = ordinal
                .checked_sub(1)
                .map(|previous| vec![task_ids[previous].as_str()])
                .unwrap_or_default();
            item(task_id, &dependencies, 1)
        })
        .collect();

    let (path, effort) = project(items).unwrap();

    assert_eq!(path, task_ids);
    assert_eq!(effort, u32::try_from(MAX_WORK_PRODUCT_ITEMS).unwrap());
}
