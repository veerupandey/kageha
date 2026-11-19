//! Opt-in real-provider evaluation. Synthetic labeled cases only; no browser input.
use kageha_browser_decisions::{
    routing, typed, Mode, Models, Observation, Override, Policy, Scope, Session,
};
use kageha_contracts::execution::Control;
use kageha_models::{decisions::HttpDecisions, HttpModel, Provider};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead},
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::{Duration, Instant},
};
#[derive(Deserialize)]
struct Case {
    id: String,
    family: String,
    split: String,
    objective: String,
    controls: Vec<Value>,
    values: BTreeMap<String, Value>,
    expected: Vec<routing::Action>,
    #[serde(default)]
    pending: bool,
    #[serde(default)]
    trusted_click_targets: BTreeSet<String>,
}
struct Capture {
    backend: HttpDecisions,
    answer: Mutex<Option<typed::Result>>,
}
impl typed::DecisionPort for Capture {
    fn decide(&self, r: &typed::Request, c: &Control<'_>) -> io::Result<typed::Result> {
        let a = self.backend.decide(r, c)?;
        *self.answer.lock().unwrap() = Some(a.clone());
        Ok(a)
    }
}
struct Replay(typed::Result);
impl typed::DecisionPort for Replay {
    fn decide(&self, _: &typed::Request, c: &Control<'_>) -> io::Result<typed::Result> {
        c.check()?;
        Ok(self.0.clone())
    }
}
fn session(threshold: f64, backend: Arc<dyn typed::DecisionPort + Send + Sync>) -> Session {
    let confidence_mode = std::env::var("KAGEHA_EVAL_CONFIDENCE_MODE").as_deref() == Ok("1");
    let mut p = Policy {
        confidence_cutoff: confidence_mode.then_some(threshold),
        model: "evaluation".into(),
        threshold,
        timeout_s: 10.,
        ..Policy::default()
    };
    p.overrides.insert(
        "browser_action".into(),
        Override {
            mode: Some(Mode::Active),
            ..Override::default()
        },
    );
    Session::new(
        Scope {
            workspace: "synthetic-evaluation".into(),
            client: "evaluation".into(),
            run: "one-case".into(),
        },
        p,
        Models::new(),
    )
    .unwrap()
    .with_decision_backends(BTreeMap::from([("evaluation".into(), backend)]))
}
fn main() -> io::Result<()> {
    let provider = Provider {
        url: std::env::var("KAGEHA_EVAL_URL").map_err(io::Error::other)?,
        key: std::env::var("KAGEHA_EVAL_KEY").map_err(io::Error::other)?,
        model: std::env::var("KAGEHA_EVAL_MODEL").map_err(io::Error::other)?,
        protocol: std::env::var("KAGEHA_EVAL_PROTOCOL").map_err(io::Error::other)?,
    };
    let model = provider.model.clone();
    let capture = Arc::new(Capture {
        backend: HttpDecisions::new(HttpModel::new(vec![provider])?)?,
        answer: Mutex::new(None),
    });
    let flag = AtomicBool::new(false);
    for line in io::stdin().lock().lines() {
        let case: Case = serde_json::from_str(&line?)?;
        let request = routing::Request {
            observation_id: case.id.clone(),
            observation: Observation {
                resource: "synthetic-tab".into(),
                binding: "synthetic-profile".into(),
                document: "synthetic-document".into(),
                service_epoch: "fixture-epoch".into(),
                evidence: json!({"text":"Synthetic browser fixture"}),
            },
            controls: case.controls,
            values: case.values,
            allowed_actions: BTreeSet::from(["fill".into(), "check".into(), "click".into()]),
            trusted_click_targets: case.trusted_click_targets,
            pending_actions: case.pending,
        };
        *capture.answer.lock().unwrap() = None;
        let started = Instant::now();
        let control = Control {
            cancelled: &flag,
            deadline: Some(Instant::now() + Duration::from_secs(12)),
        };
        let original = session(0.5, capture.clone()).route(&case.objective, &request, &control);
        let elapsed_ms = started.elapsed().as_millis();
        let answer = capture.answer.lock().unwrap().clone();
        let mut thresholds = Vec::new();
        for threshold in [
            0.5, 0.7, 0.8, 0.85, 0.9, 0.95, 0.97, 0.98, 0.99, 0.995, 0.999, 1.0,
        ] {
            let outcome = answer.as_ref().map(|a| {
                session(threshold, Arc::new(Replay(a.clone()))).route(
                    &case.objective,
                    &request,
                    &Control {
                        cancelled: &flag,
                        deadline: None,
                    },
                )
            });
            let actions = outcome
                .as_ref()
                .and_then(|o| o.as_ref().ok())
                .map(|o| o.actions.clone())
                .unwrap_or_default();
            let wrong = actions
                .iter()
                .filter(|a| !case.expected.contains(a))
                .count();
            thresholds.push(json!({"threshold":threshold,"accepted":actions.len(),"correct":actions.len()-wrong,"wrong":wrong,"actions":actions}));
        }
        println!(
            "{}",
            json!({"id":case.id,"family":case.family,"split":case.split,"model":model,"batch_threshold":Policy::default().batch_threshold,"score_metric":if std::env::var("KAGEHA_EVAL_CONFIDENCE_MODE").as_deref()==Ok("1"){"provider_confidence"}else{"selected_probability"},"elapsed_ms":elapsed_ms,"expected":case.expected,"probabilities":answer.as_ref().map(|a|&a.answers),"provider_ok":answer.is_some(),"status":original.ok().map(|o|o.status),"thresholds":thresholds})
        );
    }
    Ok(())
}
