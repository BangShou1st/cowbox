use cowbox_dag::{BASE_STEP, DagError, SessionDag, StepNode};
use std::path::PathBuf;

fn new_dag() -> SessionDag {
    SessionDag::new("session-1", PathBuf::from("/tmp/cowbox-workspace"))
}

fn append(dag: &mut SessionDag, label: &str) -> u64 {
    let id = dag.next_step_id();
    let parent = dag.head_step;
    let mut node = StepNode::new(id, Some(parent), label);
    node.timestamp_unix_ms = id * 100;
    node.exit_code = Some(0);
    node.duration_ms = id * 10;
    dag.add_step(node).expect("append step");
    id
}

fn three_step_dag() -> SessionDag {
    let mut dag = new_dag();
    append(&mut dag, "step 1");
    append(&mut dag, "step 2");
    append(&mut dag, "step 3");
    dag
}

#[test]
fn new_session_has_only_base() {
    let dag = new_dag();

    assert_eq!(dag.session_id, "session-1");
    assert_eq!(dag.workspace_root, PathBuf::from("/tmp/cowbox-workspace"));
    assert_eq!(dag.head_step, BASE_STEP);
    assert_eq!(dag.nodes.len(), 1);
    assert_eq!(dag.next_step_id(), 1);
    assert!(dag.active_layer_chain().is_empty());
    assert_eq!(dag.path_to_base(BASE_STEP).unwrap(), vec![BASE_STEP]);
    dag.validate().unwrap();

    let base = dag.nodes.get(&BASE_STEP).unwrap();
    assert_eq!(base.id, BASE_STEP);
    assert_eq!(base.parent_id, None);
    assert_eq!(base.label, "base");
}

#[test]
fn append_steps_advances_head_and_chain() {
    let mut dag = new_dag();

    let first = append(&mut dag, "step 1");
    let second = append(&mut dag, "step 2");

    assert_eq!(first, 1);
    assert_eq!(second, 2);
    assert_eq!(dag.head_step, 2);
    assert_eq!(dag.next_step_id(), 3);
    assert_eq!(dag.active_layer_chain(), vec![1, 2]);
    assert_eq!(dag.path_to_base(2).unwrap(), vec![2, 1, 0]);
    dag.validate().unwrap();
}

#[test]
fn add_step_rejects_non_sequential_id() {
    let mut dag = new_dag();
    append(&mut dag, "step 1");

    let mut node = StepNode::new(3, Some(dag.head_step), "step 3");
    node.exit_code = Some(0);

    assert_eq!(dag.add_step(node), Err(DagError::NonSequentialStep(3)));
    assert_eq!(dag.head_step, 1);
}

#[test]
fn add_step_rejects_duplicate_id() {
    let mut dag = new_dag();
    append(&mut dag, "step 1");

    let node = StepNode::new(1, Some(dag.head_step), "duplicate");

    assert_eq!(dag.add_step(node), Err(DagError::DuplicateStep(1)));
}

#[test]
fn add_step_rejects_missing_parent() {
    let mut dag = new_dag();

    let node = StepNode::new(1, Some(99), "orphan");

    assert_eq!(dag.add_step(node), Err(DagError::InvalidParent(99)));
}

#[test]
fn add_step_rejects_parent_that_is_not_head() {
    let mut dag = new_dag();
    append(&mut dag, "step 1");
    append(&mut dag, "step 2");

    let node = StepNode::new(3, Some(1), "branch from old step");

    assert_eq!(
        dag.add_step(node),
        Err(DagError::ParentIsNotHead { parent: 1, head: 2 })
    );
}

#[test]
fn rollback_to_unknown_step_fails() {
    let mut dag = three_step_dag();

    assert_eq!(dag.rollback(42), Err(DagError::UnknownStep(42)));
    assert_eq!(dag.head_step, 3);
}

#[test]
fn rollback_then_branch_preserves_old_branch() {
    let mut dag = three_step_dag();

    dag.rollback(1).unwrap();
    assert_eq!(dag.head_step, 1);
    assert_eq!(dag.active_layer_chain(), vec![1]);

    let branch = append(&mut dag, "branch from step 1");

    assert_eq!(branch, 4);
    assert_eq!(dag.head_step, 4);
    assert_eq!(dag.active_layer_chain(), vec![1, 4]);
    assert_eq!(dag.path_to_base(3).unwrap(), vec![3, 2, 1, 0]);
    assert_eq!(dag.path_to_base(4).unwrap(), vec![4, 1, 0]);
    assert!(dag.nodes.contains_key(&2));
    assert!(dag.nodes.contains_key(&3));
    dag.validate().unwrap();
}

#[test]
fn undo_zero_is_a_no_op() {
    let mut dag = three_step_dag();

    assert_eq!(dag.undo(0).unwrap(), 3);
    assert_eq!(dag.head_step, 3);
}

#[test]
fn undo_walks_back_to_base() {
    let mut dag = three_step_dag();

    assert_eq!(dag.undo(1).unwrap(), 2);
    assert_eq!(dag.undo(1).unwrap(), 1);
    assert_eq!(dag.undo(1).unwrap(), BASE_STEP);
    assert_eq!(dag.head_step, BASE_STEP);
    assert!(dag.active_layer_chain().is_empty());
}

#[test]
fn undo_too_deep_reports_available_depth() {
    let mut dag = three_step_dag();

    assert_eq!(
        dag.undo(5),
        Err(DagError::UndoTooDeep {
            requested: 5,
            available: 3,
        })
    );
    assert_eq!(dag.head_step, 3);

    assert_eq!(
        dag.undo(4),
        Err(DagError::UndoTooDeep {
            requested: 4,
            available: 3,
        })
    );
    assert_eq!(dag.head_step, 3);
}

#[test]
fn undo_on_base_reports_zero_available() {
    let mut dag = new_dag();

    assert_eq!(
        dag.undo(1),
        Err(DagError::UndoTooDeep {
            requested: 1,
            available: 0,
        })
    );
}

#[test]
fn active_chain_follows_current_head_only() {
    let mut dag = three_step_dag();

    dag.rollback(2).unwrap();
    assert_eq!(dag.active_layer_chain(), vec![1, 2]);

    dag.rollback(0).unwrap();
    assert!(dag.active_layer_chain().is_empty());
}

#[test]
fn serde_round_trip_preserves_dag_and_metadata() {
    let mut dag = three_step_dag();
    dag.rollback(2).unwrap();
    dag.nodes.get_mut(&2).unwrap().timed_out = true;
    dag.nodes.get_mut(&2).unwrap().signal = Some(9);
    dag.nodes.get_mut(&2).unwrap().orphans_reaped = 2;

    let encoded = serde_json::to_string(&dag).unwrap();
    let decoded: SessionDag = serde_json::from_str(&encoded).unwrap();

    assert_eq!(decoded, dag);
    assert_eq!(decoded.head_step, 2);
    assert_eq!(decoded.nodes.get(&2).unwrap().duration_ms, 20);
    assert_eq!(decoded.nodes.get(&2).unwrap().signal, Some(9));
    assert_eq!(decoded.nodes.get(&2).unwrap().orphans_reaped, 2);
    assert!(decoded.nodes.get(&2).unwrap().timed_out);
}

#[test]
fn validate_detects_missing_base() {
    let mut dag = new_dag();
    dag.nodes.remove(&BASE_STEP);

    assert_eq!(dag.validate(), Err(DagError::MissingBase));
}

#[test]
fn validate_detects_missing_head() {
    let mut dag = three_step_dag();
    dag.head_step = 99;

    assert_eq!(dag.validate(), Err(DagError::InvalidHead(99)));
}

#[test]
fn validate_detects_invalid_parent_reference() {
    let mut dag = three_step_dag();
    dag.nodes.get_mut(&2).unwrap().parent_id = Some(99);

    assert_eq!(dag.validate(), Err(DagError::InvalidParent(99)));
}

#[test]
fn validate_detects_base_with_parent() {
    let mut dag = new_dag();
    dag.nodes.get_mut(&BASE_STEP).unwrap().parent_id = Some(1);

    assert_eq!(dag.validate(), Err(DagError::InvalidParent(BASE_STEP)));
}

#[test]
fn validate_detects_non_base_without_parent() {
    let mut dag = three_step_dag();
    dag.nodes.get_mut(&1).unwrap().parent_id = None;

    assert_eq!(dag.validate(), Err(DagError::InvalidParent(1)));
}

#[test]
fn validate_detects_cycle() {
    let mut dag = three_step_dag();
    dag.nodes.get_mut(&1).unwrap().parent_id = Some(2);
    dag.nodes.get_mut(&2).unwrap().parent_id = Some(1);

    assert_eq!(dag.validate(), Err(DagError::Cycle(1)));
}

#[test]
fn path_to_base_detects_cycle() {
    let mut dag = three_step_dag();
    dag.nodes.get_mut(&1).unwrap().parent_id = Some(2);
    dag.nodes.get_mut(&2).unwrap().parent_id = Some(1);

    assert_eq!(dag.path_to_base(1), Err(DagError::Cycle(1)));
}

#[test]
fn undo_detects_cycle_without_hanging() {
    let mut dag = three_step_dag();
    dag.nodes.get_mut(&1).unwrap().parent_id = Some(2);
    dag.nodes.get_mut(&2).unwrap().parent_id = Some(1);

    assert_eq!(dag.undo(10), Err(DagError::Cycle(2)));
}

#[test]
fn active_chain_is_safe_on_corrupt_dag() {
    let mut dag = three_step_dag();
    dag.nodes.get_mut(&1).unwrap().parent_id = Some(2);
    dag.nodes.get_mut(&2).unwrap().parent_id = Some(1);

    let chain = dag.active_layer_chain();

    assert_eq!(chain, vec![1, 2, 3]);
}
