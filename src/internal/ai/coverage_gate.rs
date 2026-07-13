//! Per-turn coverage claim gate (plan-20260713 DR-05c-0, ADR-DR-09/10/16).
//!
//! `agent_coverage_claim` is the WRITE-FRONT idempotence gate: a checkpoint
//! writer reserves the logical turns it is about to cover *before* building
//! objects, and commits the claims (revision insert + claim advance + catalog
//! row) inside the SAME SQLite transaction as the traces ref CAS — so a
//! repeated TurnEnd, a crash retry, or a concurrent writer can never produce
//! a second visible checkpoint for an already-covered turn.
//!
//! Arbitration (ADR-DR-09) is embodied in [`reserve_live_turn_claims`]:
//! equivalent committed content → no-op; committed `incomplete` upgraded by
//! new `complete` content → revision advance; committed `complete` vs a
//! different `complete` digest → `conflicted` (doctor's job, never silent
//! overwrite); an unexpired foreign reservation → skip (someone else is
//! writing this turn); an expired one → fenced takeover. Every mutation is a
//! conditional write checked via `rows_affected == 1` — losers re-read, they
//! never assume.
//!
//! Failure policy (ADR-DR-10): any DB error here fails the checkpoint write
//! *closed* — the caller must not append to `refs/libra/traces` without a
//! reservation, and a commit-time fence mismatch rolls the whole final
//! transaction back (ref update included).

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement};

use crate::internal::ai::{
    history::{TracesCommitCtx, TracesTxnExtra},
    observed_agents::{COVERAGE_SCHEMA_VERSION, Completeness, NormalizedTurn},
};

/// Live reservation lease length. Generous relative to a single hook write
/// (sub-second) so takeover only fires on genuinely dead writers.
const LIVE_LEASE_MS: i64 = 60_000;

/// One reserved turn: the writer holds `(owner, fence_token)` and must present
/// both at commit time.
#[derive(Debug, Clone)]
pub struct ReservedTurnClaim {
    pub logical_turn_key: String,
    pub coverage_digest: String,
    pub completeness: Completeness,
    pub fence_token: i64,
    pub next_revision: i64,
}

/// Outcome of a live reservation pass over one snapshot's normalized turns.
#[derive(Debug, Default)]
pub struct LiveReservationOutcome {
    pub reserved: Vec<ReservedTurnClaim>,
    /// Turns already covered by equivalent-or-better committed content.
    pub skipped_covered: usize,
    /// Turns currently reserved by another live writer (unexpired lease).
    pub skipped_inflight: usize,
    /// Turns whose committed `complete` content differs from this snapshot's
    /// `complete` content — flagged `conflicted` for doctor, never rewritten.
    pub conflicted: usize,
}

impl LiveReservationOutcome {
    /// Nothing to write: every turn is covered / in flight / conflicted.
    pub fn is_noop(&self) -> bool {
        self.reserved.is_empty()
    }
}

struct ExistingClaim {
    coverage_digest: String,
    completeness: String,
    revision: i64,
    state: String,
    lease_expires_at: Option<i64>,
    fence_token: Option<i64>,
}

async fn read_claim(
    conn: &impl ConnectionTrait,
    session_id: &str,
    logical_turn_key: &str,
) -> Result<Option<ExistingClaim>> {
    let row = conn
        .query_one(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT coverage_digest, completeness, revision, state,
                    lease_expires_at, fence_token
             FROM agent_coverage_claim
             WHERE session_id = ? AND logical_turn_key = ?
               AND coverage_schema_version = ?",
            [
                session_id.into(),
                logical_turn_key.into(),
                COVERAGE_SCHEMA_VERSION.into(),
            ],
        ))
        .await
        .context("query agent_coverage_claim")?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(ExistingClaim {
        coverage_digest: row.try_get_by("coverage_digest")?,
        completeness: row.try_get_by("completeness")?,
        revision: row.try_get_by("revision")?,
        state: row.try_get_by("state")?,
        lease_expires_at: row.try_get_by("lease_expires_at").ok().flatten(),
        fence_token: row.try_get_by("fence_token").ok().flatten(),
    }))
}

/// Insert a brand-new `reserved_live` claim. Returns the reservation, or
/// `None` when a concurrent writer won the INSERT race (unique violation) —
/// the caller re-reads and re-decides.
async fn try_insert_fresh_claim(
    conn: &impl ConnectionTrait,
    session_id: &str,
    turn: &NormalizedTurn,
    digest: &str,
    owner: &str,
    now_ms: i64,
) -> Result<Option<ReservedTurnClaim>> {
    let result = conn
        .execute(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_coverage_claim (
                session_id, logical_turn_key, coverage_schema_version,
                coverage_digest, completeness, revision, state,
                owner, lease_expires_at, fence_token, source_channel,
                created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, 0, 'reserved_live', ?, ?, 1, 'live', ?, ?)
             ON CONFLICT(session_id, logical_turn_key, coverage_schema_version)
             DO NOTHING",
            [
                session_id.into(),
                turn.logical_turn_key.clone().into(),
                COVERAGE_SCHEMA_VERSION.into(),
                digest.into(),
                turn.completeness.as_db_str().into(),
                owner.into(),
                (now_ms + LIVE_LEASE_MS).into(),
                now_ms.into(),
                now_ms.into(),
            ],
        ))
        .await
        .context("insert agent_coverage_claim reservation")?;
    if result.rows_affected() == 1 {
        Ok(Some(ReservedTurnClaim {
            logical_turn_key: turn.logical_turn_key.clone(),
            coverage_digest: digest.to_string(),
            completeness: turn.completeness,
            fence_token: 1,
            next_revision: 1,
        }))
    } else {
        Ok(None)
    }
}

/// Conditionally re-own an existing claim row (upgrade / takeover /
/// re-reserve). All prior identifying fields are in the WHERE so a concurrent
/// mutation makes this a 0-row no-op the caller re-reads after.
#[allow(clippy::too_many_arguments)]
async fn try_reown_claim(
    conn: &impl ConnectionTrait,
    session_id: &str,
    logical_turn_key: &str,
    expected_state: &str,
    expected_fence: Option<i64>,
    new_digest: &str,
    new_completeness: Completeness,
    owner: &str,
    now_ms: i64,
) -> Result<Option<(i64, i64)>> {
    let new_fence = expected_fence.unwrap_or(0) + 1;
    let expected_fence_value: sea_orm::Value = expected_fence.into();
    let result = conn
        .execute(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE agent_coverage_claim
             SET state = 'reserved_live', owner = ?, lease_expires_at = ?,
                 fence_token = ?, coverage_digest = ?, completeness = ?,
                 source_channel = 'live', updated_at = ?
             WHERE session_id = ? AND logical_turn_key = ?
               AND coverage_schema_version = ?
               AND state = ? AND fence_token IS ?",
            [
                owner.into(),
                (now_ms + LIVE_LEASE_MS).into(),
                new_fence.into(),
                new_digest.into(),
                new_completeness.as_db_str().into(),
                now_ms.into(),
                session_id.into(),
                logical_turn_key.into(),
                COVERAGE_SCHEMA_VERSION.into(),
                expected_state.into(),
                expected_fence_value,
            ],
        ))
        .await
        .context("re-own agent_coverage_claim")?;
    if result.rows_affected() == 1 {
        Ok(Some((new_fence, now_ms + LIVE_LEASE_MS)))
    } else {
        Ok(None)
    }
}

/// Mark a committed-complete-vs-different-complete collision `conflicted`
/// (ADR-DR-09: never silently overwrite committed complete content).
async fn try_mark_conflicted(
    conn: &impl ConnectionTrait,
    session_id: &str,
    logical_turn_key: &str,
    expected_digest: &str,
    now_ms: i64,
) -> Result<()> {
    conn.execute(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_coverage_claim
         SET state = 'conflicted', updated_at = ?
         WHERE session_id = ? AND logical_turn_key = ?
           AND coverage_schema_version = ?
           AND state = 'catalog_committed' AND coverage_digest = ?",
        [
            now_ms.into(),
            session_id.into(),
            logical_turn_key.into(),
            COVERAGE_SCHEMA_VERSION.into(),
            expected_digest.into(),
        ],
    ))
    .await
    .context("mark agent_coverage_claim conflicted")?;
    Ok(())
}

/// Reserve the turns of one live snapshot (ADR-DR-09 arbitration). Bounded:
/// each turn takes at most two decision rounds (initial read + one re-read
/// after losing a conditional write race).
pub async fn reserve_live_turn_claims(
    conn: &DatabaseConnection,
    session_id: &str,
    turns: &[NormalizedTurn],
    owner: &str,
    now_ms: i64,
) -> Result<LiveReservationOutcome> {
    let mut outcome = LiveReservationOutcome::default();
    for turn in turns {
        let digest = turn.digest_hex();
        let mut rounds = 0;
        loop {
            rounds += 1;
            let existing = read_claim(conn, session_id, &turn.logical_turn_key).await?;
            let decision =
                decide_and_attempt(conn, session_id, turn, &digest, owner, now_ms, existing)
                    .await?;
            match decision {
                AttemptOutcome::Reserved(claim) => {
                    outcome.reserved.push(claim);
                    break;
                }
                AttemptOutcome::SkipCovered => {
                    outcome.skipped_covered += 1;
                    break;
                }
                AttemptOutcome::SkipInflight => {
                    outcome.skipped_inflight += 1;
                    break;
                }
                AttemptOutcome::Conflicted => {
                    outcome.conflicted += 1;
                    break;
                }
                AttemptOutcome::LostRace if rounds < 3 => continue,
                AttemptOutcome::LostRace => {
                    // Two consecutive lost races: someone very active owns
                    // this turn right now; treat as in-flight, next event
                    // will retry.
                    outcome.skipped_inflight += 1;
                    break;
                }
            }
        }
    }
    Ok(outcome)
}

enum AttemptOutcome {
    Reserved(ReservedTurnClaim),
    SkipCovered,
    SkipInflight,
    Conflicted,
    LostRace,
}

async fn decide_and_attempt(
    conn: &DatabaseConnection,
    session_id: &str,
    turn: &NormalizedTurn,
    digest: &str,
    owner: &str,
    now_ms: i64,
    existing: Option<ExistingClaim>,
) -> Result<AttemptOutcome> {
    let Some(existing) = existing else {
        return Ok(
            match try_insert_fresh_claim(conn, session_id, turn, digest, owner, now_ms).await? {
                Some(claim) => AttemptOutcome::Reserved(claim),
                None => AttemptOutcome::LostRace,
            },
        );
    };

    match existing.state.as_str() {
        "catalog_committed" => {
            if existing.coverage_digest == digest {
                // Byte-identical content (any completeness): pure no-op.
                return Ok(AttemptOutcome::SkipCovered);
            }
            match (existing.completeness.as_str(), turn.completeness) {
                ("incomplete", Completeness::Complete) => {
                    // Upgrade path: incomplete → complete advances the turn's
                    // current revision (ADR-DR-16).
                    match try_reown_claim(
                        conn,
                        session_id,
                        &turn.logical_turn_key,
                        "catalog_committed",
                        existing.fence_token,
                        digest,
                        turn.completeness,
                        owner,
                        now_ms,
                    )
                    .await?
                    {
                        Some((fence, _lease)) => Ok(AttemptOutcome::Reserved(ReservedTurnClaim {
                            logical_turn_key: turn.logical_turn_key.clone(),
                            coverage_digest: digest.to_string(),
                            completeness: turn.completeness,
                            fence_token: fence,
                            next_revision: existing.revision + 1,
                        })),
                        None => Ok(AttemptOutcome::LostRace),
                    }
                }
                ("complete", Completeness::Complete) => {
                    // complete → different complete: never auto-overwrite.
                    try_mark_conflicted(
                        conn,
                        session_id,
                        &turn.logical_turn_key,
                        &existing.coverage_digest,
                        now_ms,
                    )
                    .await?;
                    Ok(AttemptOutcome::Conflicted)
                }
                // A (different) incomplete snapshot never downgrades or
                // replaces committed content.
                (_, Completeness::Incomplete) => Ok(AttemptOutcome::SkipCovered),
                // incomplete → incomplete with different digest: keep the
                // committed one; a later complete parse upgrades it.
                _ => Ok(AttemptOutcome::SkipCovered),
            }
        }
        "reserved_live" | "reserved_import" => {
            let lease_live = existing.lease_expires_at.is_some_and(|t| t > now_ms);
            if lease_live {
                return Ok(AttemptOutcome::SkipInflight);
            }
            // Expired lease: fenced takeover (stale holder's later writes
            // fail their fence check and roll back).
            match try_reown_claim(
                conn,
                session_id,
                &turn.logical_turn_key,
                &existing.state,
                existing.fence_token,
                digest,
                turn.completeness,
                owner,
                now_ms,
            )
            .await?
            {
                Some((fence, _lease)) => Ok(AttemptOutcome::Reserved(ReservedTurnClaim {
                    logical_turn_key: turn.logical_turn_key.clone(),
                    coverage_digest: digest.to_string(),
                    completeness: turn.completeness,
                    fence_token: fence,
                    next_revision: existing.revision + 1,
                })),
                None => Ok(AttemptOutcome::LostRace),
            }
        }
        "abandoned" => {
            match try_reown_claim(
                conn,
                session_id,
                &turn.logical_turn_key,
                "abandoned",
                existing.fence_token,
                digest,
                turn.completeness,
                owner,
                now_ms,
            )
            .await?
            {
                Some((fence, _lease)) => Ok(AttemptOutcome::Reserved(ReservedTurnClaim {
                    logical_turn_key: turn.logical_turn_key.clone(),
                    coverage_digest: digest.to_string(),
                    completeness: turn.completeness,
                    fence_token: fence,
                    next_revision: existing.revision + 1,
                })),
                None => Ok(AttemptOutcome::LostRace),
            }
        }
        // Conflicted rows stay parked for doctor; never auto-resolved here.
        _ => Ok(AttemptOutcome::Conflicted),
    }
}

/// The transactional commit plan for one gated checkpoint write: applied by
/// `HistoryManager` INSIDE the ref-CAS transaction (ADR-DR-10 — ref update,
/// catalog row, coverage revisions and claim advances all commit or all roll
/// back together).
pub struct LiveClaimCommitPlan {
    pub session_id: String,
    pub checkpoint_id: String,
    pub owner: String,
    pub parent_commit: Option<String>,
    pub created_at: i64,
    pub now_ms: i64,
    pub claims: Vec<ReservedTurnClaim>,
}

#[async_trait]
impl TracesTxnExtra for LiveClaimCommitPlan {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
        // Catalog row first (claim advance references checkpoint_id). The
        // `ON CONFLICT DO NOTHING` backstop keeps a crash-retry idempotent —
        // but within one transaction the row is always fresh.
        txn.execute(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, scope, parent_commit, tree_oid,
                metadata_blob_oid, traces_commit, created_at
             ) VALUES (?, ?, 'committed', ?, ?, ?, ?, ?)
             ON CONFLICT(checkpoint_id) DO NOTHING",
            [
                self.checkpoint_id.clone().into(),
                self.session_id.clone().into(),
                self.parent_commit.clone().into(),
                ctx.tree_oid.clone().into(),
                ctx.metadata_blob_oid.clone().into(),
                ctx.commit_hash.clone().into(),
                self.created_at.into(),
            ],
        ))
        .await
        .context("insert agent_checkpoint row in ref transaction")?;

        for claim in &self.claims {
            // Append-only revision history (ADR-DR-16).
            txn.execute(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "INSERT INTO agent_coverage_revision (
                    session_id, logical_turn_key, coverage_schema_version,
                    revision, checkpoint_id, coverage_digest, completeness,
                    source_channel, created_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, 'live', ?)",
                [
                    self.session_id.clone().into(),
                    claim.logical_turn_key.clone().into(),
                    COVERAGE_SCHEMA_VERSION.into(),
                    claim.next_revision.into(),
                    self.checkpoint_id.clone().into(),
                    claim.coverage_digest.clone().into(),
                    claim.completeness.as_db_str().into(),
                    self.now_ms.into(),
                ],
            ))
            .await
            .context("insert agent_coverage_revision in ref transaction")?;

            // Advance the claim — owner + fence + state guarded. Zero rows
            // means our reservation was fenced out; the WHOLE transaction
            // (ref update included) must roll back (ADR-DR-10).
            let advanced = txn
                .execute(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "UPDATE agent_coverage_claim
                     SET state = 'catalog_committed', revision = ?,
                         coverage_digest = ?, completeness = ?,
                         checkpoint_id = ?, traces_commit = ?,
                         owner = NULL, lease_expires_at = NULL, updated_at = ?
                     WHERE session_id = ? AND logical_turn_key = ?
                       AND coverage_schema_version = ?
                       AND state = 'reserved_live'
                       AND owner = ? AND fence_token = ?",
                    [
                        claim.next_revision.into(),
                        claim.coverage_digest.clone().into(),
                        claim.completeness.as_db_str().into(),
                        self.checkpoint_id.clone().into(),
                        ctx.commit_hash.clone().into(),
                        self.now_ms.into(),
                        self.session_id.clone().into(),
                        claim.logical_turn_key.clone().into(),
                        COVERAGE_SCHEMA_VERSION.into(),
                        self.owner.clone().into(),
                        claim.fence_token.into(),
                    ],
                ))
                .await
                .context("advance agent_coverage_claim in ref transaction")?;
            if advanced.rows_affected() != 1 {
                bail!(
                    "coverage claim for turn '{}' was fenced out during commit \
                     (stale reservation); rolling back checkpoint transaction",
                    claim.logical_turn_key
                );
            }
        }
        Ok(())
    }
}
