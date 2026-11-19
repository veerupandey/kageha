//! Typed System One adapter over the shared authenticated, deadline-bound transport.
use crate::*;
use kageha_contracts::decisions::{DecisionPort, Request, Result as DecisionResult};
pub fn is_typed_protocol(protocol: &str) -> bool {
    matches!(protocol, "clef" | "system_one" | "openai_decisions" | "gemini_decisions")
}

pub struct HttpDecisions(HttpModel);
impl HttpDecisions {
    pub fn new(model: HttpModel) -> io::Result<Self> {
        if model.providers.iter().any(|p| !is_typed_protocol(&p.protocol)) {
            return Err(io::Error::other("A typed decision transport is required"));
        }
        Ok(Self(model))
    }
}
impl DecisionPort for HttpDecisions {
    fn decide(&self, request: &Request, control: &Control<'_>) -> io::Result<DecisionResult> {
        request.validate()?;
        // Conservative byte bounds mirror Python's guard and prevent silent
        // provider truncation from dropping task constraints or field evidence.
        let context = self
            .0
            .providers
            .iter()
            .map(|p| {
                if p.model.ends_with("clef-flash") {
                    24_576usize
                } else if p.protocol == "clef" {
                    65_536
                } else {
                    32_768
                }
            })
            .min()
            .unwrap_or(0);
        if serde_json::to_vec(request)?.len() > context.saturating_sub(2048) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Typed decision context exceeds the backend limit",
            ));
        }
        let reply = self.0.complete(
            ModelRequest {
                messages: vec![json!({"role":"user","content":serde_json::to_string(request)?})],
                tools: vec![],
                role: Default::default(),
            },
            control,
        )?;
        if !reply.tool_calls.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Typed decisions cannot invoke tools"));
        }
        let body: Value = serde_json::from_str(
            reply
                .content
                .as_str()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid typed provider response"))?,
        )
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid typed provider JSON"))?;
        DecisionResult::parse(request, &body).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

pub(crate) fn gemini_payload(request: &Request, model: &str) -> io::Result<Value> {
    let payload = request.payload(model)?;
    Ok(json!({
        "model": model,
        "input": serde_json::to_string(&request.state)?,
        "questions": payload["questions"]
    }))
}

pub(crate) fn openai_payload(request: &Request, model: &str) -> io::Result<Value> {
    request.validate()?;
    Ok(
        json!({"model":model,"input":[{"role":"user","content":[{"type":"input_json","data":request.state}]}],"questions":request.questions.iter().map(|(id,q)|json!({"type":"choice","name":id,"instructions":q.instructions,"choices":q.criteria.iter().map(|(value,description)|json!({"value":value,"description":description})).collect::<Vec<_>>()})).collect::<Vec<_>>()}),
    )
}

// Normalize only the typed endpoint's array format. Refusals, duplicate names and
// option values fail closed; the shared parser validates distributions/coverage.
pub(crate) fn openai_result(body: &Value) -> io::Result<Value> {
    let rows = body["answers"]
        .as_array()
        .ok_or_else(|| io::Error::other("Missing decision answers"))?;
    let mut answers = serde_json::Map::new();
    for row in rows {
        if row["type"] != "choice" {
            return Err(io::Error::other("Unsupported or refused decision answer"));
        }
        let name = row["name"]
            .as_str()
            .ok_or_else(|| io::Error::other("Missing decision name"))?;
        let mut probabilities = serde_json::Map::new();
        for option in row["probabilities"]
            .as_array()
            .ok_or_else(|| io::Error::other("Missing decision probabilities"))?
        {
            let value = option["value"]
                .as_str()
                .ok_or_else(|| io::Error::other("Unexpected option value type"))?;
            if probabilities
                .insert(value.into(), option["probability"].clone())
                .is_some()
            {
                return Err(io::Error::other("Duplicate decision option"));
            }
        }
        if answers.insert(name.into(),json!({"type":"choice","choice":row["choice"],"confidence":row["confidence"],"probabilities":probabilities})).is_some(){return Err(io::Error::other("Duplicate decision name"));}
    }
    Ok(json!({"answers":answers,"usage":body["usage"]}))
}

impl HttpDecisions {
    /// Convenience registry for decision endpoints only. Credentials stay server-side.
    /// Applications needing custom gateways/resolvers can use `new(HttpModel)`.
    pub fn from_env(model: &str) -> io::Result<Self> {
        let get = |name: &str| crate::catalog::credential(name, |key| std::env::var(key).ok());
        let (url, key, model, protocol) = match model {
            "gemini-decision-1" | "gemini/gemini-decision-1" => (
                "https://generativelanguage.googleapis.com/v1beta/decisions".into(),
                get("GEMINI_API_KEY"),
                "gemini-decision-1".into(),
                "gemini_decisions",
            ),
            "gpt-6-luna" | "openai-decisions/gpt-6-luna" => (
                "https://api.openai.com/v1/decisions".into(),
                get("OPENAI_API_KEY"),
                "gpt-6-luna".into(),
                "openai_decisions",
            ),
            "clef" | "clef-flash" | "@cf/cloudflare/clef" | "@cf/cloudflare/clef-flash" => {
                let account = get("CLOUDFLARE_ACCOUNT_ID");
                if account.len() != 32 || !account.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(io::Error::other("Configure a valid Cloudflare account"));
                }
                let model = if model.starts_with("@cf/") {
                    model.to_owned()
                } else {
                    format!("@cf/cloudflare/{model}")
                };
                (
                    format!(
                        "https://api.cloudflare.com/client/v4/accounts/{account}/ai/run/{model}"
                    ),
                    get("CLOUDFLARE_API_TOKEN"),
                    model,
                    "clef",
                )
            }
            "typesafe/jev" | "typesafe/jev-1.14.0" => {
                let native = get("TYPESAFE_API_KEY");
                if native.is_empty() {
                    (
                        "https://api.commandcode.ai/provider/v1/systemone".into(),
                        get("CMD_API_KEY"),
                        model.into(),
                        "system_one",
                    )
                } else {
                    (
                        "https://api.typesafe.ai/v1/systemone".into(),
                        native,
                        model.trim_start_matches("typesafe/").into(),
                        "system_one",
                    )
                }
            }
            _ => {
                return Err(io::Error::other(
                    "Unknown decision model; register an explicit typed provider",
                ))
            }
        };
        if key.is_empty() {
            return Err(io::Error::other(
                "Decision provider credentials are not configured",
            ));
        }
        Self::new(HttpModel::new(vec![Provider {
            url,
            key,
            model,
            protocol: protocol.into(),
        }])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn openai_refusals_and_duplicate_typed_answers_fail_closed() {
        assert!(openai_result(&json!({"answers":[{"type":"refusal","name":"q"}]})).is_err());
        let answer = json!({"type":"choice","name":"q","choice":"yes","confidence":0.9,"probabilities":[{"value":"yes","probability":1.0},{"value":"no","probability":0.0}]});
        assert!(openai_result(&json!({"answers":[answer.clone(),answer.clone()]})).is_err());
        let mut duplicate = answer;
        duplicate["probabilities"] =
            json!([{"value":"yes","probability":0.5},{"value":"yes","probability":0.5}]);
        assert!(openai_result(&json!({"answers":[duplicate]})).is_err());
    }
}
