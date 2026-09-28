use glaeda::durable_journal::{
    DurableExecutionError, JournalCheckpoint, JournalCheckpointFailure, JournalCheckpointPhase,
    execute_plan_durably,
};
use glaeda::journal::{
    ActionFailure, ActionOutcome, ActionReceipt, ExecutionJournal, ExecutionLane,
    JOURNAL_SCHEMA_VERSION, JournalRecord, MutationExecutor, PlannedMutation, Preconditions,
    RollbackClass,
};

#[derive(Debug, Default)]
struct ScriptedExecutor {
    execute_calls: Vec<String>,
    rollback_calls: Vec<String>,
}

impl MutationExecutor for ScriptedExecutor {
    fn execute(&mut self, action: &PlannedMutation) -> Result<ActionReceipt, ActionFailure> {
        self.execute_calls.push(action.id.clone());
        match action.id.as_str() {
            "prepare-account" => Ok(ActionReceipt::public("account prepared")),
            "verify-runner" => Err(ActionFailure::public(
                "runner-verification-failed",
                "runner verification failed",
            )),
            other => panic!("unexpected action {other:?}"),
        }
    }

    fn rollback(
        &mut self,
        action: &PlannedMutation,
        receipt: &ActionReceipt,
    ) -> Result<ActionReceipt, ActionFailure> {
        self.rollback_calls.push(action.id.clone());
        assert_eq!(action.id, "prepare-account");
        assert_eq!(receipt.summary(), "account prepared");
        Ok(ActionReceipt::public("account compensation completed"))
    }
}

#[derive(Debug)]
struct InterruptingCheckpoint {
    fail_at: usize,
    snapshots: Vec<ExecutionJournal>,
}

impl InterruptingCheckpoint {
    fn new(fail_at: usize) -> Self {
        Self {
            fail_at,
            snapshots: Vec::new(),
        }
    }
}

impl JournalCheckpoint for InterruptingCheckpoint {
    fn checkpoint(&mut self, journal: &ExecutionJournal) -> Result<(), JournalCheckpointFailure> {
        self.snapshots.push(journal.clone());
        if self.snapshots.len() == self.fail_at {
            Err(JournalCheckpointFailure::public(
                "injected durable checkpoint interruption",
            ))
        } else {
            Ok(())
        }
    }
}

fn action(id: &str, rollback: RollbackClass) -> PlannedMutation {
    PlannedMutation::new(
        id,
        ExecutionLane::Root,
        format!("execute {id}"),
        rollback,
        Preconditions::new([format!("{id} was freshly observed")]),
    )
}

fn plan() -> Vec<PlannedMutation> {
    vec![
        action("prepare-account", RollbackClass::Compensating),
        action("verify-runner", RollbackClass::Reversible),
    ]
}

#[test]
fn every_uncertain_checkpoint_retains_the_last_durable_snapshot_and_stops() {
    let cases = [
        (1, JournalCheckpointPhase::Initial, None, 0, 0),
        (
            2,
            JournalCheckpointPhase::BeforeExecute,
            Some("prepare-account"),
            0,
            0,
        ),
        (
            3,
            JournalCheckpointPhase::AfterExecute,
            Some("prepare-account"),
            1,
            0,
        ),
        (
            4,
            JournalCheckpointPhase::BeforeExecute,
            Some("verify-runner"),
            1,
            0,
        ),
        (
            5,
            JournalCheckpointPhase::AfterExecute,
            Some("verify-runner"),
            2,
            0,
        ),
        (
            6,
            JournalCheckpointPhase::BeforeRollback,
            Some("prepare-account"),
            2,
            0,
        ),
        (
            7,
            JournalCheckpointPhase::AfterRollback,
            Some("prepare-account"),
            2,
            1,
        ),
    ];

    for (fail_at, phase, action_id, execute_calls, rollback_calls) in cases {
        let mut executor = ScriptedExecutor::default();
        let mut checkpoint = InterruptingCheckpoint::new(fail_at);
        let result = execute_plan_durably(plan(), &mut executor, &mut checkpoint, false);
        let error = match result {
            Err(DurableExecutionError::Checkpoint(error)) => error,
            other => panic!("checkpoint {fail_at} returned {other:?}"),
        };

        assert_eq!(error.phase(), phase);
        assert_eq!(error.action_id(), action_id);
        assert_eq!(error.attempted(), &checkpoint.snapshots[fail_at - 1]);
        let expected_last = fail_at
            .checked_sub(2)
            .map(|index| &checkpoint.snapshots[index]);
        assert_eq!(error.last_durable(), expected_last);
        assert_eq!(executor.execute_calls.len(), execute_calls);
        assert_eq!(executor.rollback_calls.len(), rollback_calls);
    }
}

#[test]
fn uninterrupted_execution_publishes_each_boundary_in_order() {
    let mut executor = ScriptedExecutor::default();
    let mut checkpoint = InterruptingCheckpoint::new(usize::MAX);
    let journal = execute_plan_durably(plan(), &mut executor, &mut checkpoint, false)
        .expect("durable execution completes");

    assert_eq!(checkpoint.snapshots.len(), 7);
    assert_eq!(
        checkpoint
            .snapshots
            .iter()
            .map(|snapshot| {
                snapshot
                    .records
                    .iter()
                    .map(|record| record.outcome)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>(),
        vec![
            vec![ActionOutcome::Pending, ActionOutcome::Pending],
            vec![ActionOutcome::Executing, ActionOutcome::Pending],
            vec![ActionOutcome::Completed, ActionOutcome::Pending],
            vec![ActionOutcome::Completed, ActionOutcome::Executing],
            vec![ActionOutcome::Completed, ActionOutcome::Failed],
            vec![ActionOutcome::RollbackInProgress, ActionOutcome::Failed],
            vec![ActionOutcome::Compensated, ActionOutcome::Failed],
        ]
    );
    assert_eq!(journal.records[0].outcome, ActionOutcome::Compensated);
    assert_eq!(journal.records[1].outcome, ActionOutcome::Failed);
    assert_eq!(journal.stopped_after.as_deref(), Some("verify-runner"));
}

#[derive(Debug)]
struct ExistingEvidenceCheckpoint {
    existing: ExecutionJournal,
    attempted: Vec<ExecutionJournal>,
}

impl JournalCheckpoint for ExistingEvidenceCheckpoint {
    fn checkpoint(&mut self, journal: &ExecutionJournal) -> Result<(), JournalCheckpointFailure> {
        self.attempted.push(journal.clone());
        Err(JournalCheckpointFailure::public(
            "journal identifier already contains recovery evidence",
        ))
    }
}

#[test]
fn rerun_refuses_to_replace_interrupted_evidence_before_execution() {
    let interrupted = ExecutionJournal {
        schema_version: JOURNAL_SCHEMA_VERSION,
        records: vec![JournalRecord {
            action: action("prepare-account", RollbackClass::Compensating),
            outcome: ActionOutcome::Executing,
            message: None,
        }],
        stopped_after: None,
    };
    let mut checkpoint = ExistingEvidenceCheckpoint {
        existing: interrupted.clone(),
        attempted: Vec::new(),
    };
    let mut executor = ScriptedExecutor::default();

    let result = execute_plan_durably(
        vec![action("prepare-account", RollbackClass::Compensating)],
        &mut executor,
        &mut checkpoint,
        false,
    );
    let error = match result {
        Err(DurableExecutionError::Checkpoint(error)) => error,
        other => panic!("rerun returned {other:?}"),
    };

    assert_eq!(error.phase(), JournalCheckpointPhase::Initial);
    assert_eq!(error.last_durable(), None);
    assert_eq!(checkpoint.existing, interrupted);
    assert_eq!(checkpoint.attempted.len(), 1);
    assert_eq!(
        checkpoint.attempted[0].records[0].outcome,
        ActionOutcome::Pending
    );
    assert!(executor.execute_calls.is_empty());
    assert!(executor.rollback_calls.is_empty());
}
