//! Required outcomes and trusted proof references shared by all task hosts.
use kageha_contracts::execution::{BlockedOutcome, CompletionCheck, CompletionStatus, OutcomeSummary};
use serde::{Deserialize, Serialize};
use std::{collections::{BTreeMap, BTreeSet}, io};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind { Answer, Artifact, Action, Validation }
/// Who introduced an outcome. A missing origin (v1 contracts) is `User`, the stricter choice.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Origin { #[default] User, Agent }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RequiredOutcome {
    pub id: String,
    pub description: String,
    pub kind: OutcomeKind,
    /// How the outcome will be proven.
    #[serde(default)]
    pub verify: String,
    #[serde(default)]
    pub origin: Origin,
    /// The precise host-checkable obligation. Legacy outcomes load without one;
    /// hosts must request it before accepting action or validation evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<crate::verification::Verification>,
}
/// A recorded change to an outcome. Amendments never remove outcomes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Amendment {
    pub id: String,
    pub before: RequiredOutcome,
    pub after: RequiredOutcome,
    pub reason: String,
    pub weakened: bool,
    pub at: f64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum BlockerCategory { MissingAccess, MissingInput, ExternalFailure, PolicyDenied }
/// An accepted report that an outcome cannot be completed in this request.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Blocker {
    pub outcome_id: String,
    pub category: BlockerCategory,
    pub reason: String,
    pub references: Vec<String>,
    pub at: f64,
    /// True only for a host-created blocker for an unavailable proof capability.
    #[serde(default)]
    pub missing_capability: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub outcome_id: String,
    pub reference: String,
    pub turn: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Contract {
    #[serde(default)]
    pub outcomes: Vec<RequiredOutcome>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    #[serde(default)]
    pub history: Vec<Amendment>,
    #[serde(default)]
    pub blockers: Vec<Blocker>,
    #[serde(default)]
    pub task_type: Option<String>,
}
pub const OUTCOME_UNSATISFIABLE: &str = "outcome_unsatisfiable";
impl OutcomeKind {
    pub fn as_str(self) -> &'static str {
        match self { Self::Answer => "answer", Self::Artifact => "artifact", Self::Action => "action", Self::Validation => "validation" }
    }
    /// The capability a turn lacks when it cannot prove this kind.
    pub fn missing_capability(self) -> &'static str {
        match self {
            Self::Answer => "a final answer",
            Self::Artifact => "a tool that creates a workspace file (for example write_file, edit_file, generate_image or text_to_speech)",
            Self::Action => "a tool that returns a verified action receipt (browser input or computer_execute)",
            Self::Validation => "a tool that returns a validation result (remote_exec or browser_validate)",
        }
    }
}
/// An outcome the current tools cannot prove. The requirement is still stored.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Rejected {
    pub id: String,
    pub kind: OutcomeKind,
    pub code: &'static str,
    pub missing_capability: &'static str,
    /// Provable kinds that could be used if the original declaration was mistaken.
    pub alternatives: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct RecordResult {
    pub accepted: Vec<String>,
    pub rejected: Vec<Rejected>,
}
impl Contract {
    /// Requirements may be appended, but a completion claim cannot weaken them.
    /// Outcomes whose kind no available tool can prove are reported as unsatisfiable,
    /// but still stored so the gate cannot silently treat them as completed.
    /// Invalid or conflicting input fails the whole call.
    pub fn record(&mut self, outcomes: Vec<RequiredOutcome>, provable: &BTreeSet<OutcomeKind>) -> io::Result<RecordResult> {
        let mut next: BTreeMap<_, _> = self.outcomes.iter().map(|item| (item.id.clone(), item.clone())).collect();
        let mut submitted = BTreeMap::<String, RequiredOutcome>::new();
        let alternatives: Vec<String> = provable.iter().map(|kind| kind.as_str().to_owned()).collect();
        let mut result = RecordResult::default();
        for outcome in outcomes {
            validate_outcome(&outcome)?;
            if next.get(&outcome.id).or_else(|| submitted.get(&outcome.id)).is_some_and(|prior| prior != &outcome) {
                return Err(io::Error::other("A recorded required outcome cannot be replaced or weakened"));
            }
            if submitted.insert(outcome.id.clone(), outcome.clone()).is_some() { continue; }
            if provable.contains(&outcome.kind) {
                result.accepted.push(outcome.id.clone());
                next.insert(outcome.id.clone(), outcome);
            } else {
                result.rejected.push(Rejected { id: outcome.id.clone(), kind: outcome.kind, code: OUTCOME_UNSATISFIABLE,
                    missing_capability: outcome.kind.missing_capability(), alternatives: alternatives.clone() });
                next.insert(outcome.id.clone(), outcome);
            }
        }
        if next.len() > 64 { return Err(io::Error::other("At most 64 required outcomes are supported")); }
        self.outcomes = next.into_values().collect();
        Ok(result)
    }
    pub fn attach(&mut self, evidence: Evidence) -> io::Result<()> {
        if !self.outcomes.iter().any(|outcome| outcome.id == evidence.outcome_id)
            || evidence.reference.is_empty() || evidence.reference.len() > 1024
            || evidence.turn.is_empty() || evidence.turn.len() > 256 {
            return Err(io::Error::other("Evidence must identify an existing required outcome and recorded result"));
        }
        if !self.evidence.contains(&evidence) { self.evidence.push(evidence); }
        if self.evidence.len() > 256 { return Err(io::Error::other("Completion evidence exceeds limit")); }
        Ok(())
    }
    /// Changes the kind, description or `verify` text of an existing outcome and records
    /// the change in `history`. The id cannot change and nothing is removed.
    /// `origin` is always kept from the stored outcome, so an amendment cannot turn a
    /// user requirement into an agent one (whatever `next.origin` says).
    /// Feasibility of the new kind and acceptance of the reason are Host concerns.
    pub fn amend(&mut self, id: &str, next: RequiredOutcome, reason: &str) -> io::Result<Amendment> {
        if !bounded_text(reason, MAX_TEXT) {
            return Err(io::Error::other("An amendment needs a non-empty bounded reason"));
        }
        if next.id != id { return Err(io::Error::other("An amendment cannot change the outcome id")); }
        validate_outcome(&next)?;
        if next.verify.len() > MAX_TEXT || next.verify.contains('\0') {
            return Err(io::Error::other("Outcome verify text must be at most 4000 characters and contain no null bytes"));
        }
        let Some(index) = self.outcomes.iter().position(|outcome| outcome.id == id) else {
            return Err(io::Error::other("An amendment must name an existing required outcome"));
        };
        let before = self.outcomes[index].clone();
        let after = RequiredOutcome { origin: before.origin, ..next };
        if after == before { return Err(io::Error::other("An amendment must change the outcome")); }
        if self.history.len() >= MAX_HISTORY { return Err(io::Error::other("Outcome amendment history exceeds limit")); }
        let amendment = Amendment { id: id.to_owned(), weakened: weakens(&before, &after), before,
            after: after.clone(), reason: reason.to_owned(), at: now() };
        self.outcomes[index] = after;
        // Proof and blockers were accepted against the previous requirement.
        // Even a same-kind description change needs fresh evidence attachment.
        self.evidence.retain(|evidence| evidence.outcome_id != id);
        self.blockers.retain(|blocker| blocker.outcome_id != id);
        self.history.push(amendment.clone());
        Ok(amendment)
    }
    /// Stores a blocker for an existing outcome. Only one blocker per outcome is kept:
    /// a later report replaces the earlier one (replace-latest), so the gate and the
    /// final answer always see the current reason. Checking that references resolve to
    /// failed attempts and the ladder step is the Host's job; this validates shape only.
    pub fn block(&mut self, blocker: Blocker) -> io::Result<()> {
        if !self.outcomes.iter().any(|outcome| outcome.id == blocker.outcome_id) {
            return Err(io::Error::other("A blocker must name an existing required outcome"));
        }
        if !bounded_text(&blocker.reason, MAX_TEXT) || !blocker.at.is_finite()
            || blocker.references.len() > MAX_BLOCKER_REFERENCES
            || blocker.references.iter().any(|reference| !bounded_text(reference, 1024)) {
            return Err(io::Error::other(
                "A blocker needs a non-empty bounded reason and at most 64 non-empty bounded references"));
        }
        match self.blockers.iter_mut().find(|item| item.outcome_id == blocker.outcome_id) {
            Some(existing) => *existing = blocker,
            None => self.blockers.push(blocker),
        }
        Ok(())
    }
    /// Weakened amendments of user-originated outcomes, in the order they were made.
    /// The final-answer builder lists these under "Changed requirements" (Requirement 5.3).
    pub fn weakened_amendments_for_user(&self) -> Vec<&Amendment> {
        self.history.iter().filter(|item| item.weakened && item.before.origin == Origin::User).collect()
    }
}
const MAX_TEXT: usize = 4000;
const MAX_HISTORY: usize = 256;
const MAX_BLOCKER_REFERENCES: usize = 64;
fn bounded_text(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.len() <= max && !text.contains('\0')
}
fn validate_outcome(outcome: &RequiredOutcome) -> io::Result<()> {
    if !crate::planning::identifier(&outcome.id) {
        return Err(io::Error::other(format!(
            "Outcome id {:?} is invalid: must be a valid identifier (1-64 alphanumeric, dash, or underscore characters)",
            outcome.id
        )));
    }
    if !bounded_text(&outcome.description, MAX_TEXT) {
        return Err(io::Error::other(
            "Each required outcome needs a non-empty description at most 4000 characters without null bytes"
        ));
    }
    if let Some(proof) = &outcome.proof {
        if !proof.valid() {
            return Err(io::Error::other(format!(
                "Outcome proof for {:?} is invalid: verify required fields (e.g. browser_fields needs target_id, document_id, and 1..32 unique assertions with property 'value'|'checked'|'text'|'validity.valid'; browser_postcondition needs target_id, document_id, selector, text)",
                outcome.id
            )));
        }
        let matched = matches!(
            (&outcome.kind, proof),
            (OutcomeKind::Action, crate::verification::Verification::BrowserSubmission {..} | crate::verification::Verification::BrowserPostcondition {..} | crate::verification::Verification::ComputerAction {..} | crate::verification::Verification::CredentialRotation {..})
            | (OutcomeKind::Validation, crate::verification::Verification::BrowserFields {..} | crate::verification::Verification::Command {..})
            | (OutcomeKind::Artifact, crate::verification::Verification::RemoteFile {..})
        );
        if !matched {
            return Err(io::Error::other(format!(
                "Outcome kind {:?} does not match proof type: action outcomes require browser_submission, browser_postcondition, credential_rotation or computer_action; validation outcomes require browser_fields or command",
                outcome.kind
            )));
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn valid_outcome(outcome: &RequiredOutcome) -> bool {
    validate_outcome(outcome).is_ok()
}
/// Strength order used to detect weakening: `action > validation > artifact > answer`.
fn strength(kind: OutcomeKind) -> u8 {
    match kind { OutcomeKind::Answer => 0, OutcomeKind::Artifact => 1, OutcomeKind::Validation => 2, OutcomeKind::Action => 3 }
}
/// An amendment weakens an outcome when its kind moves down the strength order or its
/// `verify` text shrinks below half its previous length (counted in characters).
fn weakens(before: &RequiredOutcome, after: &RequiredOutcome) -> bool {
    before.proof.as_ref().is_some_and(|prior|after.proof.as_ref().is_none_or(|next|!next.covers(prior)))
        || strength(after.kind) < strength(before.kind)
        || after.verify.chars().count() * 2 < before.verify.chars().count()
}
fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |elapsed| elapsed.as_secs_f64())
}
impl BlockerCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingAccess => "missing_access", Self::MissingInput => "missing_input",
            Self::ExternalFailure => "external_failure", Self::PolicyDenied => "policy_denied",
        }
    }
}
/// The Completion_Gate without a reviewer. See [`evaluate_reviewed`].
pub fn evaluate(contract: &Contract, verified: &BTreeMap<String, Vec<String>>, candidate: &str, blocker: Option<&str>) -> CompletionCheck {
    evaluate_reviewed(contract, verified, candidate, blocker, &BTreeMap::new())
}
/// Whether the reviewer model checks outcomes of this kind before `Ready` (Requirement 14.4).
pub fn reviewed_kind(_kind: OutcomeKind) -> bool {
    true
}
/// The Completion_Gate.
///
/// - `verified` is recomputed by a trusted host from actual artifacts/receipts; neither
///   assistant prose nor a model-supplied boolean is verification. It proves
///   `artifact`, `action` and `validation` outcomes.
/// - An `answer` outcome is verified iff `candidate.trim()` is non-empty.
/// - `reviewer_gaps` is the reviewer model's verdict: the id of each required
///   outcome it failed, with the gap it named (empty when no reviewer is configured). Such
///   an outcome is not verified, and the Continue instruction names its gap.
/// - An unverified outcome with an accepted blocker in `contract.blockers` is excluded from
///   the unmet set. Verification wins over a blocker reported earlier.
/// - `blocker` is a pending input or submission blocker raised by the Host, such as an
///   attempted submission without a verified confirmation receipt. It always yields
///   `Blocked`, even when every outcome is verified or blocked, so an unconfirmed
///   irreversible action is never reported as complete (Requirement 19.2).
///
/// Status order: `Blocked` (a pending `blocker`), `Ready` (all verified), `Partial` (all
/// verified or blocked, at least one blocked), else `Continue`.
pub fn evaluate_reviewed(contract: &Contract, verified: &BTreeMap<String, Vec<String>>, candidate: &str,
    blocker: Option<&str>, reviewer_gaps: &BTreeMap<String, String>) -> CompletionCheck {
    let answered = !candidate.trim().is_empty();
    let is_verified = |outcome: &RequiredOutcome| {
        let proven = match outcome.kind {
            OutcomeKind::Answer => answered,
            _ => verified.get(&outcome.id).is_some_and(|refs| !refs.is_empty()),
        };
        proven && !(reviewed_kind(outcome.kind) && reviewer_gaps.contains_key(&outcome.id))
    };
    let blocker_for = |outcome: &RequiredOutcome| contract.blockers.iter().find(|item| item.outcome_id == outcome.id);
    let mut blocked = Vec::new();
    let mut unmet = Vec::new();
    for outcome in contract.outcomes.iter().filter(|outcome| !is_verified(outcome)) {
        match blocker_for(outcome) { Some(item) => blocked.push((outcome, item)), None => unmet.push(outcome) }
    }
    // Structured parts for the runtime's final message, filled for every status.
    let check = |status, instruction| CompletionCheck {
        status, instruction, evidence: verified.values().flatten().cloned().collect(),
        unmet: unmet.iter().map(|outcome| OutcomeSummary { id: outcome.id.clone(), description: outcome.description.clone() }).collect(),
        blocked: blocked.iter().map(|(outcome, item)| BlockedOutcome { id: outcome.id.clone(),
            description: outcome.description.clone(), category: item.category.as_str().into(), reason: item.reason.clone() }).collect(),
        changed_requirements: changed_requirements(contract),
        ..Default::default()
    };
    if let Some(reason) = blocker {
        return check(CompletionStatus::Blocked, reason.into());
    }
    if unmet.is_empty() && blocked.is_empty() {
        return check(CompletionStatus::Ready, String::new());
    }
    if unmet.is_empty() {
        return check(CompletionStatus::Partial, format!(
            "Blocked outcomes: {}.",
            blocked.iter().map(|(outcome, item)| format!("{}: {} ({}: {})",
                outcome.id, outcome.description, item.category.as_str(), item.reason)).collect::<Vec<_>>().join("; ")
        ));
    }
    check(CompletionStatus::Continue, continue_instruction(&unmet, reviewer_gaps))
}
/// One line per weakened amendment of a user-origin outcome (Requirement 5.3), for the
/// "Changed requirements" section of the final answer.
pub fn changed_requirements(contract: &Contract) -> Vec<String> {
    let describe = |outcome: &RequiredOutcome| {
        let verify = outcome.verify.trim();
        let mut text = format!("{} \"{}\"", outcome.kind.as_str(), outcome.description.trim());
        if !verify.is_empty() { text.push_str(&format!(" (verify: {verify})")); }
        text
    };
    contract.weakened_amendments_for_user().into_iter().map(|item| format!("{}: changed from {} to {}. Reason: {}",
        item.id, describe(&item.before), describe(&item.after), item.reason.trim())).collect()
}
/// The Continue instruction (Requirements 6.6, 8.2): only the unmet outcomes, one per line,
/// then a request to close those gaps or call `report_blocker`.
fn continue_instruction(unmet: &[&RequiredOutcome], reviewer_gaps: &BTreeMap<String, String>) -> String {
    let mut text = String::from("Unmet outcomes:");
    for outcome in unmet {
        text.push_str(&format!("\n- {}: {}", outcome.id, outcome.description));
        let verify = outcome.verify.trim();
        if !verify.is_empty() { text.push_str(&format!(" (verify: {verify})")); }
        if let Some(gap) = reviewer_gaps.get(&outcome.id).filter(|_| reviewed_kind(outcome.kind)) {
            text.push_str(&format!(" Reviewer: {}", gap.trim()));
        }
    }
    text.push_str("\nYour previous answer is above. Close only these gaps using tools, or call report_blocker for each outcome you cannot complete. report_blocker is the only accepted way to report a blocker. Do not rewrite the whole answer. Attach recorded proof with record_outcome_evidence. A successful intermediate tool call or final-answer claim does not complete artifact, action or validation outcomes.");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    fn outcome(id: &str) -> RequiredOutcome { RequiredOutcome { id:id.into(), description:"Create the requested deliverable".into(), kind:OutcomeKind::Artifact, verify:String::new(), origin:Origin::User, proof:None } }
    #[test]
    fn v1_contracts_load_with_defaulted_v2_fields() {
        let v1=r#"{"outcomes":[{"id":"deliver","description":"Write the report","kind":"artifact"}],
            "evidence":[{"outcome_id":"deliver","reference":"report.md","turn":"t1"}]}"#;
        let contract:Contract=serde_json::from_str(v1).unwrap();
        assert_eq!(contract.outcomes[0].origin,Origin::User);
        assert_eq!(contract.outcomes[0].verify,"");
        assert!(contract.history.is_empty() && contract.blockers.is_empty() && contract.task_type.is_none());
        // Strictness is kept: unknown outcome fields are still rejected.
        assert!(serde_json::from_str::<RequiredOutcome>(r#"{"id":"a","description":"d","kind":"answer","extra":1}"#).is_err());
    }
    #[test]
    fn v2_contract_round_trips() {
        let answer=RequiredOutcome{id:"reply".into(),description:"Answer the question".into(),kind:OutcomeKind::Answer,verify:"Non-empty final answer".into(),origin:Origin::Agent,proof:None};
        let contract=Contract{outcomes:vec![answer.clone()],evidence:vec![],
            history:vec![Amendment{id:"reply".into(),before:answer.clone(),after:answer.clone(),reason:"clarified".into(),weakened:false,at:1.5}],
            blockers:vec![Blocker{outcome_id:"reply".into(),category:BlockerCategory::MissingAccess,reason:"No login".into(),references:vec!["call-1".into()],at:2.0,missing_capability:false}],
            task_type:Some("research".into())};
        let text=serde_json::to_string(&contract).unwrap();
        assert!(text.contains(r#""kind":"answer""#) && text.contains(r#""origin":"agent""#) && text.contains(r#""category":"missing_access""#));
        let back:Contract=serde_json::from_str(&text).unwrap();
        assert_eq!(back.outcomes,contract.outcomes); assert_eq!(back.history,contract.history);
        assert_eq!(back.blockers,contract.blockers); assert_eq!(back.task_type,contract.task_type);
    }
    fn all() -> BTreeSet<OutcomeKind> { BTreeSet::from([OutcomeKind::Answer,OutcomeKind::Artifact,OutcomeKind::Action,OutcomeKind::Validation]) }
    fn of(id:&str,kind:OutcomeKind)->RequiredOutcome { RequiredOutcome{kind,..outcome(id)} }
    #[test]
    fn requirements_cannot_be_replaced_or_dropped_by_recording_a_subset() {
        let mut contract=Contract::default(); contract.record(vec![outcome("first"),outcome("second")],&all()).unwrap();
        contract.record(vec![outcome("first")],&all()).unwrap(); assert_eq!(contract.outcomes.len(),2);
        let mut changed=outcome("first"); changed.description="Less work".into();
        assert!(contract.record(vec![changed],&all()).is_err());
    }
    #[test]
    fn unprovable_outcomes_are_reported_and_all_requirements_are_preserved() {
        let provable=BTreeSet::from([OutcomeKind::Answer,OutcomeKind::Validation]);
        let mut contract=Contract::default();
        let reply=RequiredOutcome{verify:"Non-empty answer".into(),origin:Origin::Agent,..of("reply",OutcomeKind::Answer)};
        let result=contract.record(vec![reply.clone(),of("file",OutcomeKind::Artifact),of("check",OutcomeKind::Validation),of("submit",OutcomeKind::Action)],&provable).unwrap();
        assert_eq!(result.accepted,vec!["reply","check"]);
        assert_eq!(result.rejected.iter().map(|item|(item.id.as_str(),item.kind)).collect::<Vec<_>>(),
            vec![("file",OutcomeKind::Artifact),("submit",OutcomeKind::Action)]);
        let rejected=&result.rejected[0];
        assert_eq!(rejected.code,"outcome_unsatisfiable");
        assert!(rejected.missing_capability.contains("write_file"));
        assert_eq!(rejected.alternatives,vec!["answer","validation"]);
        assert_eq!(contract.outcomes,vec![of("check",OutcomeKind::Validation),of("file",OutcomeKind::Artifact),reply,of("submit",OutcomeKind::Action)]);
        let json=serde_json::to_value(&result).unwrap();
        assert_eq!(json["rejected"][1]["code"],"outcome_unsatisfiable"); assert_eq!(json["rejected"][1]["kind"],"action");
    }
    #[test]
    fn rejecting_every_outcome_is_a_result_and_validation_and_append_only_still_fail() {
        let mut contract=Contract::default();
        let result=contract.record(vec![outcome("file")],&BTreeSet::from([OutcomeKind::Answer])).unwrap();
        assert!(result.accepted.is_empty() && result.rejected.len()==1 && contract.outcomes == vec![outcome("file")]);
        // An identical stored outcome remains intact and is reported unavailable
        // whenever the current tools cannot prove it.
        contract.record(vec![outcome("file")],&all()).unwrap();
        assert_eq!(contract.record(vec![outcome("file")],&BTreeSet::new()).unwrap().rejected[0].id,"file");
        let mut changed=outcome("file"); changed.kind=OutcomeKind::Answer;
        assert!(contract.record(vec![changed],&all()).is_err());
        let mut conflicting=outcome("other"); conflicting.description="Different".into();
        assert!(contract.record(vec![outcome("other"),conflicting],&BTreeSet::new()).is_err());
        assert!(contract.record(vec![of("bad id!",OutcomeKind::Answer)],&all()).is_err());
        assert!(contract.record(vec![RequiredOutcome{description:" ".into(),..outcome("blank")}],&all()).is_err());
        assert!(contract.record((0..64).map(|index|outcome(&format!("o{index}"))).collect(),&all()).is_err());
        // Unprovable outcomes remain requirements and count toward the cap.
        assert!(contract.record((0..70).map(|index|outcome(&format!("o{index}"))).collect(),&BTreeSet::new()).is_err());
    }
    #[test]
    fn proof_is_host_owned_and_every_required_outcome_must_be_verified() {
        let mut contract=Contract::default(); contract.record(vec![outcome("first"),outcome("second")],&all()).unwrap();
        contract.attach(Evidence{outcome_id:"first".into(),reference:"invented".into(),turn:"turn".into()}).unwrap();
        assert_eq!(evaluate(&contract,&BTreeMap::new(),"done",None).status,CompletionStatus::Continue);
        let mut verified=BTreeMap::from([("first".into(),vec!["artifact-one".into()])]);
        let check=evaluate(&contract,&verified,"done",None);
        assert_eq!(check.status,CompletionStatus::Continue);
        // Only unmet outcomes are listed.
        assert!(check.instruction.contains("second: ") && !check.instruction.contains("first: "));
        assert_eq!(evaluate(&contract,&BTreeMap::from([("first".into(),vec![]),("second".into(),vec![])]),"done",None).status,CompletionStatus::Continue);
        verified.insert("second".into(),vec!["artifact-two".into()]);
        let check=evaluate(&contract,&verified,"done",None);
        assert_eq!((check.status,check.instruction.as_str()),(CompletionStatus::Ready,""));
        assert_eq!(check.evidence,vec!["artifact-one","artifact-two"]);
    }
    #[test]
    fn answer_outcomes_are_verified_by_a_non_empty_candidate_unless_the_reviewer_rejects_them() {
        let contract=recorded(vec![of("reply",OutcomeKind::Answer)]);
        for candidate in [""," \n\t"] { assert_eq!(evaluate(&contract,&BTreeMap::new(),candidate,None).status,CompletionStatus::Continue); }
        // Host evidence does not stand in for the answer itself.
        assert_eq!(evaluate(&contract,&BTreeMap::from([("reply".into(),vec!["ref".into()])]),"",None).status,CompletionStatus::Continue);
        assert_eq!(evaluate(&contract,&BTreeMap::new(),"The answer",None).status,CompletionStatus::Ready);
        let rejected=BTreeMap::from([("reply".to_owned(),"Name the interview date.".to_owned())]);
        let check=evaluate_reviewed(&contract,&BTreeMap::new(),"The answer",None,&rejected);
        assert_eq!(check.status,CompletionStatus::Continue);
        assert!(check.instruction.contains("- reply: Create the requested deliverable Reviewer: Name the interview date."),"{}",check.instruction);
        assert_eq!(check.unmet.iter().map(|item|item.id.as_str()).collect::<Vec<_>>(),["reply"]);
        assert_eq!(evaluate_reviewed(&contract,&BTreeMap::new(),"The answer",None,&BTreeMap::from([("other".to_owned(),"gap".to_owned())])).status,CompletionStatus::Ready);
    }
    #[test]
    fn a_reviewer_gap_rejects_unsupported_claims_for_every_outcome_kind() {
        let contract=recorded(vec![outcome("file"),of("submit",OutcomeKind::Action),of("tests",OutcomeKind::Validation)]);
        let verified=BTreeMap::from([("file".into(),vec!["report.md".into()]),("submit".into(),vec!["receipt".into()]),("tests".into(),vec!["sh-1".into()])]);
        let gaps=BTreeMap::from([("file".to_owned(),"The report has no summary.".to_owned()),
            ("submit".to_owned(),"Receipt is for a fill.".to_owned()),("tests".to_owned(),"Command only printed a claim.".to_owned())]);
        let check=evaluate_reviewed(&contract,&verified,"Done",None,&gaps);
        assert_eq!(check.status,CompletionStatus::Continue);
        assert_eq!(check.unmet.iter().map(|item|item.id.as_str()).collect::<Vec<_>>(),["file","submit","tests"]);
        assert!(check.instruction.contains("Reviewer: The report has no summary.") && check.instruction.contains("Receipt is for a fill."),"{}",check.instruction);
        assert_eq!(evaluate_reviewed(&contract,&verified,"Done",None,&BTreeMap::new()).status,CompletionStatus::Ready);
    }
    #[test]
    fn blocked_outcomes_are_excluded_from_the_unmet_set_and_make_the_result_partial() {
        let mut contract=recorded(vec![outcome("file"),outcome("submit"),of("reply",OutcomeKind::Answer)]);
        contract.block(blocker("submit","No login for the portal")).unwrap();
        let verified=BTreeMap::from([("file".into(),vec!["report.md".into()])]);
        let check=evaluate(&contract,&verified,"Here is the report",None);
        assert_eq!(check.status,CompletionStatus::Partial);
        assert!(check.instruction.contains("submit: Create the requested deliverable (missing_access: No login for the portal)"),"{}",check.instruction);
        assert!(!check.instruction.contains("file:") && !check.instruction.contains("reply:"));
        assert_eq!(check.evidence,vec!["report.md"]);
        // A pending submission blocker outranks Partial.
        assert_eq!(evaluate(&contract,&verified,"Here is the report",Some("pending")).status,CompletionStatus::Blocked);
        // An unmet outcome that is not blocked keeps the gate in Continue and is the only one listed.
        let check=evaluate(&contract,&BTreeMap::new(),"Here is the report",None);
        assert_eq!(check.status,CompletionStatus::Continue);
        assert!(check.instruction.contains("file: ") && !check.instruction.contains("submit:"));
        // Verification wins over an earlier blocker.
        let all_verified=BTreeMap::from([("file".into(),vec!["report.md".into()]),("submit".into(),vec!["receipt".into()])]);
        assert_eq!(evaluate(&contract,&all_verified,"Here is the report",None).status,CompletionStatus::Ready);
        // Every outcome blocked, including an unanswered answer outcome, is still Partial.
        let mut contract=recorded(vec![of("reply",OutcomeKind::Answer)]);
        contract.block(blocker("reply","Source unreachable")).unwrap();
        assert_eq!(evaluate(&contract,&BTreeMap::new(),"",None).status,CompletionStatus::Partial);
    }
    #[test]
    fn continue_instruction_lists_only_unmet_outcomes_with_verify_text_and_names_report_blocker() {
        let mut contract=recorded(vec![
            RequiredOutcome{verify:"report.md exists".into(),..outcome("file")},
            RequiredOutcome{verify:"Booking receipt".into(),..of("submit",OutcomeKind::Action)},
            of("check",OutcomeKind::Validation),
            RequiredOutcome{verify:"Non-empty answer".into(),..of("reply",OutcomeKind::Answer)}]);
        contract.block(blocker("submit","No login")).unwrap();
        let verified=BTreeMap::from([("file".into(),vec!["report.md".into()])]);
        let check=evaluate(&contract,&verified,"The answer",None);
        assert_eq!(check.status,CompletionStatus::Continue);
        let lines:Vec<&str>=check.instruction.lines().collect();
        assert_eq!(lines[0],"Unmet outcomes:");
        // Verified (file, reply) and blocked (submit) outcomes are not listed; empty verify text is omitted.
        assert_eq!(lines[1],"- check: Create the requested deliverable");
        assert!(lines[2].starts_with("Your previous answer is above."),"{}",check.instruction);
        assert_eq!(lines.len(),3);
        for phrase in ["call report_blocker for each outcome you cannot complete","report_blocker is the only accepted way to report a blocker",
            "Do not rewrite the whole answer","final-answer claim does not complete artifact, action or validation outcomes"] {
            assert!(check.instruction.contains(phrase),"{phrase}: {}",check.instruction);
        }
        assert!(!check.instruction.contains("honestly"));
        let check=evaluate(&contract,&BTreeMap::new(),"",None);
        assert!(check.instruction.contains("\n- file: Create the requested deliverable (verify: report.md exists)\n"),"{}",check.instruction);
        assert!(check.instruction.contains("\n- reply: Create the requested deliverable (verify: Non-empty answer)\n"));
        assert!(!check.instruction.contains("submit"));
    }
    #[test]
    fn a_pending_input_or_submission_blocker_is_blocked_even_when_every_outcome_is_verified() {
        let contract=recorded(vec![outcome("first")]);
        let reason="Original submission receipt remains uncertain";
        let check=evaluate(&contract,&BTreeMap::new(),"done",Some(reason));
        assert_eq!((check.status,check.instruction.as_str()),(CompletionStatus::Blocked,reason));
        let verified=BTreeMap::from([("first".into(),vec!["receipt".into()])]);
        let check=evaluate(&contract,&verified,"done",Some(reason));
        assert_eq!((check.status,check.instruction.as_str()),(CompletionStatus::Blocked,reason));
        assert_eq!(check.evidence,vec!["receipt"]);
        // With no recorded outcomes at all, an unconfirmed submission still blocks.
        assert_eq!(evaluate(&Contract::default(),&BTreeMap::new(),"done",Some(reason)).status,CompletionStatus::Blocked);
        assert_eq!(evaluate(&contract,&verified,"done",None).status,CompletionStatus::Ready);
    }
    fn recorded(items: Vec<RequiredOutcome>) -> Contract { let mut contract=Contract::default(); contract.record(items,&all()).unwrap(); contract }
    fn verified(id:&str,kind:OutcomeKind,verify:&str)->RequiredOutcome { RequiredOutcome{verify:verify.into(),..of(id,kind)} }
    #[test]
    fn amend_replaces_the_outcome_and_records_before_after_and_reason() {
        let mut contract=recorded(vec![verified("file",OutcomeKind::Artifact,"report.md exists"),outcome("other")]);
        let next=RequiredOutcome{description:"Write and check the report".into(),..verified("file",OutcomeKind::Validation,"report.md passes lint")};
        let amendment=contract.amend("file",next.clone(),"User asked for a lint check").unwrap();
        assert_eq!(amendment.before,verified("file",OutcomeKind::Artifact,"report.md exists"));
        assert_eq!(amendment.after,next); assert_eq!(amendment.reason,"User asked for a lint check");
        assert!(!amendment.weakened && amendment.at>0.0);
        assert_eq!(contract.history,vec![amendment]);
        assert_eq!(contract.outcomes.len(),2);
        assert_eq!(contract.outcomes.iter().find(|item|item.id=="file"),Some(&next));
    }
    #[test]
    fn changed_requirements_invalidate_only_their_prior_proof_and_blockers() {
        let mut contract=recorded(vec![outcome("file"), outcome("other")]);
        for id in ["file", "other"] {
            contract.attach(Evidence{outcome_id:id.into(), reference:"proof".into(), turn:"t".into()}).unwrap();
            contract.block(Blocker{outcome_id:id.into(), category:BlockerCategory::MissingInput,
                reason:"Missing input".into(), references:vec!["attempt".into()], at:1.0, missing_capability:false}).unwrap();
        }
        let next=RequiredOutcome{description:"A different deliverable".into(),..outcome("file")};
        contract.amend("file",next,"User changed the request").unwrap();
        assert_eq!(contract.evidence.len(),1);
        assert_eq!(contract.evidence[0].outcome_id,"other");
        assert_eq!(contract.blockers.len(),1);
        assert_eq!(contract.blockers[0].outcome_id,"other");
    }
    #[test]
    fn amend_rejects_missing_reason_id_change_unknown_outcome_invalid_and_no_op_input() {
        let mut contract=recorded(vec![outcome("file")]);
        let next=RequiredOutcome{description:"Changed".into(),..outcome("file")};
        for reason in [""," \n","bad\0reason"] { assert!(contract.amend("file",next.clone(),reason).is_err()); }
        assert!(contract.amend("file",next.clone(),&"x".repeat(4001)).is_err());
        assert!(contract.amend("file",RequiredOutcome{id:"renamed".into(),..next.clone()},"rename").is_err());
        assert!(contract.amend("missing",RequiredOutcome{id:"missing".into(),..next.clone()},"new").is_err());
        assert!(contract.amend("file",RequiredOutcome{description:" ".into(),..next.clone()},"blank").is_err());
        assert!(contract.amend("file",RequiredOutcome{verify:"v\0".into(),..next.clone()},"nul").is_err());
        assert!(contract.amend("file",outcome("file"),"nothing changes").is_err());
        assert!(contract.history.is_empty()); assert_eq!(contract.outcomes,vec![outcome("file")]);
    }
    #[test]
    fn amend_keeps_the_stored_origin() {
        let mut contract=recorded(vec![outcome("file")]);
        let next=RequiredOutcome{origin:Origin::Agent,..verified("file",OutcomeKind::Answer,"")};
        let amendment=contract.amend("file",next,"Only an answer is possible").unwrap();
        assert_eq!(amendment.after.origin,Origin::User); assert_eq!(contract.outcomes[0].origin,Origin::User);
        let mut agent=recorded(vec![RequiredOutcome{origin:Origin::Agent,..outcome("plan")}]);
        let amendment=agent.amend("plan",verified("plan",OutcomeKind::Validation,"tests pass"),"Stronger proof").unwrap();
        assert_eq!(amendment.after.origin,Origin::Agent);
    }
    #[test]
    fn weakened_follows_the_kind_order_and_halved_verify_text() {
        use OutcomeKind::*;
        let order=[Action,Validation,Artifact,Answer];
        for (i,from) in order.iter().enumerate() { for (j,to) in order.iter().enumerate() {
            if i==j { continue; }
            let mut contract=recorded(vec![verified("o",*from,"same")]);
            assert_eq!(contract.amend("o",verified("o",*to,"same"),"kind change").unwrap().weakened,j>i,"{from:?} -> {to:?}");
        } }
        let mut contract=recorded(vec![verified("o",Artifact,"0123456789")]);
        assert!(!contract.amend("o",verified("o",Artifact,"01234"),"exactly half").unwrap().weakened);
        assert!(contract.amend("o",verified("o",Artifact,"01"),"under half").unwrap().weakened);
        assert!(!contract.amend("o",verified("o",Artifact,"a longer verification method"),"longer").unwrap().weakened);
        assert_eq!(contract.history.len(),3);
        assert_eq!(contract.history[1].before,verified("o",Artifact,"01234"));
        // Empty prior verify text cannot shrink.
        let mut empty=recorded(vec![outcome("o")]);
        assert!(!empty.amend("o",RequiredOutcome{description:"New".into(),..outcome("o")},"reword").unwrap().weakened);
    }
    #[test]
    fn weakened_amendments_for_user_lists_only_weakened_user_outcomes() {
        let mut contract=recorded(vec![verified("user",OutcomeKind::Action,"receipt"),
            RequiredOutcome{origin:Origin::Agent,..verified("agent",OutcomeKind::Action,"receipt")}]);
        contract.amend("user",verified("user",OutcomeKind::Validation,"receipt"),"No submit access").unwrap();
        contract.amend("agent",verified("agent",OutcomeKind::Answer,"receipt"),"Own step").unwrap();
        contract.amend("user",verified("user",OutcomeKind::Action,"receipt"),"Access restored").unwrap();
        let listed=contract.weakened_amendments_for_user();
        assert_eq!(listed.len(),1); assert_eq!((listed[0].id.as_str(),listed[0].reason.as_str()),("user","No submit access"));
    }
    fn blocker(id:&str,reason:&str)->Blocker { Blocker{outcome_id:id.into(),category:BlockerCategory::MissingAccess,reason:reason.into(),references:vec!["call-1".into()],at:1.0,missing_capability:false} }
    #[test]
    fn block_stores_and_replaces_the_latest_blocker_per_outcome() {
        let mut contract=recorded(vec![outcome("first"),outcome("second")]);
        contract.block(blocker("first","No login")).unwrap();
        contract.block(blocker("second","No input")).unwrap();
        let later=Blocker{category:BlockerCategory::PolicyDenied,at:2.0,..blocker("first","Approval denied")};
        contract.block(later.clone()).unwrap();
        assert_eq!(contract.blockers,vec![later,blocker("second","No input")]);
        assert_eq!(contract.outcomes.len(),2);
    }
    #[test]
    fn block_validates_structure_only() {
        let mut contract=recorded(vec![outcome("first")]);
        assert!(contract.block(blocker("missing","No login")).is_err());
        for reason in ["","  ","a\0b"] { assert!(contract.block(blocker("first",reason)).is_err()); }
        assert!(contract.block(blocker("first",&"x".repeat(4001))).is_err());
        assert!(contract.block(Blocker{references:vec!["".into()],..blocker("first","r")}).is_err());
        assert!(contract.block(Blocker{references:vec!["x".repeat(1025)],..blocker("first","r")}).is_err());
        assert!(contract.block(Blocker{references:(0..65).map(|i|format!("call-{i}")).collect(),..blocker("first","r")}).is_err());
        assert!(contract.block(Blocker{at:f64::NAN,..blocker("first","r")}).is_err());
        assert!(contract.blockers.is_empty());
        // Reference resolution is the Host's check, so unresolved or empty lists pass here.
        contract.block(Blocker{references:(0..64).map(|i|format!("call-{i}")).collect(),..blocker("first","r")}).unwrap();
        contract.block(Blocker{references:vec![],..blocker("first","r")}).unwrap();
        assert_eq!(contract.blockers.len(),1);
    }
    #[test]
    fn checks_carry_unmet_blocked_and_changed_requirements_for_the_final_message() {
        let mut contract=recorded(vec![verified("file",OutcomeKind::Artifact,"report.md exists"),outcome("submit"),of("reply",OutcomeKind::Answer),
            RequiredOutcome{origin:Origin::Agent,..verified("plan",OutcomeKind::Validation,"tests pass")}]);
        contract.block(blocker("submit","No login")).unwrap();
        contract.amend("file",RequiredOutcome{description:"Summarise in the answer".into(),..verified("file",OutcomeKind::Answer,"In the answer")},"No file tool").unwrap();
        // Not weakened, and agent-origin weakening, are not listed.
        contract.amend("reply",RequiredOutcome{description:"Answer the question".into(),..of("reply",OutcomeKind::Answer)},"Reworded").unwrap();
        contract.amend("plan",verified("plan",OutcomeKind::Answer,"tests pass"),"Own step").unwrap();
        let changed=vec!["file: changed from artifact \"Create the requested deliverable\" (verify: report.md exists) to answer \"Summarise in the answer\" (verify: In the answer). Reason: No file tool".to_owned()];
        let check=evaluate(&contract,&BTreeMap::new(),"",None);
        assert_eq!(check.status,CompletionStatus::Continue);
        assert_eq!(check.unmet.iter().map(|item|item.id.as_str()).collect::<Vec<_>>(),["file","plan","reply"]);
        assert_eq!(check.unmet[0].description,"Summarise in the answer");
        assert_eq!(check.blocked,vec![BlockedOutcome{id:"submit".into(),description:"Create the requested deliverable".into(),
            category:"missing_access".into(),reason:"No login".into()}]);
        assert_eq!(check.changed_requirements,changed);
        // Partial and Ready checks keep the changed requirements; Ready has nothing unmet.
        let check=evaluate(&contract,&BTreeMap::new(),"The answer",None);
        assert_eq!((check.status,check.unmet.len(),check.blocked.len()),(CompletionStatus::Partial,0,1));
        assert_eq!(check.changed_requirements,changed);
        let verified=BTreeMap::from([("submit".into(),vec!["receipt".into()])]);
        let check=evaluate(&contract,&verified,"The answer",None);
        assert_eq!(check.status,CompletionStatus::Ready);
        assert!(check.unmet.is_empty() && check.blocked.is_empty());
        assert_eq!(check.changed_requirements,changed);
        // A pending blocker still reports what is unmet.
        let check=evaluate(&contract,&BTreeMap::new(),"",Some("pending"));
        assert_eq!((check.status,check.unmet.len()),(CompletionStatus::Blocked,3));
        assert!(evaluate(&recorded(vec![outcome("x")]),&BTreeMap::new(),"",None).changed_requirements.is_empty());
    }
    #[test]
    fn ordinary_answers_have_no_completion_review_or_extra_model_call() {
        assert_eq!(evaluate(&Contract::default(),&BTreeMap::new(),"",None).status,CompletionStatus::Ready);
        assert_eq!(evaluate(&Contract::default(),&BTreeMap::new(),"answer",None).status,CompletionStatus::Ready);
    }
}
