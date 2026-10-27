//! System One contract: fixed questions, provider distributions, no generated confidence.
use crate::execution::Control;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io, sync::Arc};
pub type Backends = BTreeMap<String, Arc<dyn DecisionPort + Send + Sync>>;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Question {
    pub instructions: String,
    pub criteria: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub state: Value,
    pub questions: BTreeMap<String, Question>,
}
impl Request {
    pub fn validate(&self) -> io::Result<()> {
        if self.questions.is_empty()
            || self.questions.len() > 64
            || serde_json::to_vec(self)?.len() > 128 * 1024
        {
            return Err(io::Error::other("Invalid typed decision request size"));
        }
        for (id, q) in &self.questions {
            if id.is_empty()
                || id.len() > 100
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                || q.instructions.is_empty()
                || q.instructions.len() > 4096
                || q.criteria.len() < 2
                || q.criteria.len() > 255
                || q.criteria.iter().any(|(key, text)| {
                    key.is_empty() || key.len() > 128 || text.is_empty() || text.len() > 4096
                })
            {
                return Err(io::Error::other("Invalid typed question"));
            }
        }
        Ok(())
    }
    pub fn payload(&self, model: &str) -> io::Result<Value> {
        self.validate()?;
        Ok(
            json!({"model":model,"state":self.state,"questions":self.questions.iter().map(|(id,q)|(id.clone(),json!({"type":"choice","instructions":q.instructions,"criteria":q.criteria}))).collect::<BTreeMap<_,_>>()}),
        )
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Answer {
    pub choice: String,
    pub probabilities: BTreeMap<String, f64>,
    /// Provider-returned score; never inferred from probabilities.
    #[serde(default)]
    pub confidence: Option<f64>,
}
impl Answer {
    pub fn probability(&self) -> f64 {
        self.probabilities[&self.choice]
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Result {
    pub answers: BTreeMap<String, Answer>,
    pub usage: Value,
}
impl Result {
    pub fn validate(&self, request: &Request) -> io::Result<()> {
        if self.answers.values().any(|a| {
            a.confidence
                .is_some_and(|c| !c.is_finite() || !(0.0..=1.0).contains(&c))
        }) {
            return Err(io::Error::other("Invalid provider confidence"));
        }
        let raw = json!({"answers":self.answers.iter().map(|(id,a)|(id.clone(),json!({"type":"choice","choice":a.choice,"probabilities":a.probabilities,"confidence":a.confidence}))).collect::<BTreeMap<_,_>>()});
        Self::parse(request, &raw).map(|_| ())
    }

    pub fn parse(request: &Request, body: &Value) -> io::Result<Self> {
        request.validate()?;
        if body["success"] == false {
            return Err(io::Error::other("Typed decision provider rejected request"));
        }
        let body = body.get("result").unwrap_or(body);
        let answers = body["answers"]
            .as_object()
            .ok_or_else(|| io::Error::other("Missing typed decision answers"))?;
        if answers.len() != request.questions.len()
            || answers.keys().any(|k| !request.questions.contains_key(k))
        {
            return Err(io::Error::other("Typed question coverage mismatch"));
        }
        let mut parsed = BTreeMap::new();
        for (id, q) in &request.questions {
            let raw = &answers[id];
            if raw["type"] != "choice" {
                return Err(io::Error::other("Unexpected typed answer type"));
            }
            let choice = raw["choice"]
                .as_str()
                .ok_or_else(|| io::Error::other("Missing typed choice"))?;
            let probabilities: BTreeMap<String, f64> =
                serde_json::from_value(raw["probabilities"].clone())
                    .map_err(|_| io::Error::other("Invalid typed probabilities"))?;
            if !q.criteria.contains_key(choice)
                || probabilities.len() != q.criteria.len()
                || probabilities.iter().any(|(k, p)| {
                    !q.criteria.contains_key(k) || !p.is_finite() || !(0.0..=1.0).contains(p)
                })
                || (probabilities.values().sum::<f64>() - 1.0).abs() > 0.01
                || probabilities.values().any(|p| *p > probabilities[choice])
            {
                return Err(io::Error::other("Invalid typed probability distribution"));
            }
            let confidence = match raw.get("confidence") {
                None | Some(Value::Null) => None,
                Some(v) => Some(
                    v.as_f64()
                        .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                        .ok_or_else(|| io::Error::other("Invalid provider confidence"))?,
                ),
            };
            parsed.insert(
                id.clone(),
                Answer {
                    choice: choice.into(),
                    probabilities,
                    confidence,
                },
            );
        }
        let usage = json!({"input_tokens":body["usage"]["input_tokens"].as_u64(),"output_tokens":body["usage"]["output_tokens"].as_u64()});
        Ok(Self {
            answers: parsed,
            usage,
        })
    }
}
pub trait DecisionPort {
    fn decide(&self, request: &Request, control: &Control<'_>) -> io::Result<Result>;
}
