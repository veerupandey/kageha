//! Built-in decision endpoints and their settings inventory.
use super::*;
use kageha_models::decisions::is_typed_protocol;
use rusqlite::Connection;

struct Model {
    id: &'static str,
    model: &'static str,
    label: &'static str,
    provider: &'static str,
    protocol: &'static str,
}

const MODELS: &[Model] = &[
    Model {
        id: "cloudflare/clef",
        model: "@cf/cloudflare/clef",
        label: "Clef",
        provider: "cloudflare",
        protocol: "clef",
    },
    Model {
        id: "cloudflare/clef-flash",
        model: "@cf/cloudflare/clef-flash",
        label: "Clef-flash",
        provider: "cloudflare",
        protocol: "clef",
    },
    Model {
        id: "commandcode/typesafe/jev-1.14.0",
        model: "typesafe/jev-1.14.0",
        label: "TypeSafe Jev 1.14.0",
        provider: "commandcode",
        protocol: "system_one",
    },
    Model {
        id: "openai-decisions/gpt-6-luna",
        model: "gpt-6-luna",
        label: "GPT-6 Luna Decisions",
        provider: "openai",
        protocol: "openai_decisions",
    },
    Model {
        id: "gemini/gemini-decision-1",
        model: "gemini-decision-1",
        label: "Gemini Decision 1",
        provider: "gemini",
        protocol: "gemini_decisions",
    },
    Model {
        id: "commandcode/typesafe/jev",
        model: "typesafe/jev",
        label: "TypeSafe Jev (rolling)",
        provider: "commandcode",
        protocol: "system_one",
    },
];

impl Model {
    fn connection(&self) -> (&str, &str) {
        match self.protocol {
            "clef" => ("", "CLOUDFLARE_API_TOKEN"),
            "system_one" => ("https://api.commandcode.ai/provider/v1", "CMD_API_KEY"),
            "openai_decisions" => ("https://api.openai.com/v1", "OPENAI_API_KEY"),
            "gemini_decisions" => (
                "https://generativelanguage.googleapis.com/v1beta",
                "GEMINI_API_KEY",
            ),
            _ => unreachable!(),
        }
    }

    fn base(&self, configured: Option<&str>) -> Option<String> {
        if self.protocol == "clef" {
            if let Some((prefix, _)) = configured
                .filter(|s| !s.contains('$'))
                .and_then(|s| s.split_once("/ai/"))
            {
                return Some(format!("{prefix}/ai/run/{}", self.model));
            }
            let account =
                kageha_models::catalog::credential("CLOUDFLARE_ACCOUNT_ID", ui_settings::model_env);
            return (account.len() == 32 && account.bytes().all(|b| b.is_ascii_hexdigit())).then(
                || {
                    format!(
                        "https://api.cloudflare.com/client/v4/accounts/{account}/ai/run/{}",
                        self.model
                    )
                },
            );
        }
        let base = configured
            .unwrap_or(self.connection().0)
            .trim_end_matches('/')
            .trim_end_matches("/chat/completions")
            .trim_end_matches("/responses");
        Some(match self.protocol {
            "gemini_decisions" => base.trim_end_matches("/openai").to_owned(),
            "system_one" if base.ends_with("/provider") => format!("{base}/v1"),
            _ => base.to_owned(),
        })
    }
}

pub(crate) fn register_typed(db: &Connection) -> io::Result<()> {
    let rows = ui_store::list(db, "model").map_err(io::Error::other)?;
    for spec in MODELS {
        if let Some(existing) = rows.iter().find(|r| r["id"] == spec.id) {
            // Older generated Cloudflare rows can contain an unexpanded account ID.
            let unresolved = existing["decision_only"] == true
                && spec.protocol == "clef"
                && existing["base_url"]
                    .as_str()
                    .is_some_and(|base| base.contains('$'));
            if !unresolved {
                continue;
            }
        }
        let source = rows.iter().find(|r| {
            r["provider"] == spec.provider
                && !is_typed_protocol(r["protocol"].as_str().unwrap_or(""))
        });
        let Some(base) = spec.base(source.and_then(|r| r["base_url"].as_str())) else {
            continue;
        };
        let env = source
            .and_then(|r| r["api_key_env"].as_str())
            .unwrap_or(spec.connection().1);
        ui_store::put(
            db,
            "model",
            spec.id,
            json!({"id":spec.id, "provider":spec.provider, "model":spec.model,
            "protocol":spec.protocol, "base_url":base, "api_key_env":env, "decision_only":true}),
        )
        .map_err(io::Error::other)?;
    }
    Ok(())
}

pub(crate) fn inventory(db: &Connection, rows: &[Value], decisions: bool) -> Vec<Value> {
    let describe = |row: Option<&Value>, spec: Option<&Model>| {
        let id = spec.map(|s| s.id).or_else(|| row?["id"].as_str());
        let typed = row.map_or(spec.is_some(), |r| {
            is_typed_protocol(r["protocol"].as_str().unwrap_or(""))
        });
        let ready =
            row.is_some_and(|r| !typed || ui_settings::endpoint_ready(db, r).unwrap_or(false));
        let reason = if ready {
            String::new()
        } else if let Some(spec) = spec {
            if spec.protocol == "clef" {
                "Configure CLOUDFLARE_ACCOUNT_ID and CLOUDFLARE_API_TOKEN.".to_owned()
            } else {
                format!(
                    "Configure {}.",
                    row.and_then(|r| r["api_key_env"].as_str())
                        .unwrap_or(spec.connection().1)
                )
            }
        } else {
            "Check the provider connection.".to_owned()
        };
        json!({"id":id, "provider": row.and_then(|r| r["provider"].as_str()).or_else(|| spec.map(|s|s.provider)),
            "model":row.and_then(|r|r["model"].as_str()).or_else(||spec.map(|s|s.model)),
            "label":spec.map(|s|s.label).or_else(||row?["label"].as_str()).or(id),
            "decision_backend":typed, "ready":ready, "reason":reason,
            "alias":id == Some("commandcode/typesafe/jev")})
    };
    if !decisions {
        return rows.iter().map(|r| describe(Some(r), None)).collect();
    }
    let mut models: Vec<_> = MODELS
        .iter()
        .map(|spec| describe(rows.iter().find(|r| r["id"] == spec.id), Some(spec)))
        .collect();
    models.extend(
        rows.iter()
            .filter(|r| !MODELS.iter().any(|spec| r["id"] == spec.id))
            .map(|r| describe(Some(r), None)),
    );
    models.retain(|model| model["decision_backend"] == true);
    models
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_inventory_excludes_chat_models_and_keeps_custom_decision_backends() {
        let db = Connection::open_in_memory().unwrap();
        ui_store::initialize(&db).unwrap();
        let rows = vec![
            json!({"id":"chat/luna","protocol":"openai_compat"}),
            json!({"id":"custom/decision","protocol":"system_one"}),
        ];
        let decisions = inventory(&db, &rows, true);
        assert!(decisions.iter().all(|model| model["decision_backend"] == true));
        assert!(decisions.iter().any(|model| model["id"] == "custom/decision"));
        assert!(!decisions.iter().any(|model| model["id"] == "chat/luna"));
        assert!(inventory(&db, &rows, false).iter().any(|model| model["id"] == "chat/luna"));
    }

    #[test]
    fn inventories_five_models_and_registers_the_native_gemini_route_idempotently() {
        let db = Connection::open_in_memory().unwrap();
        ui_store::initialize(&db).unwrap();
        let names: Vec<_> = inventory(&db, &[], true)
            .into_iter()
            .filter(|m| m["alias"] == false && m["ready"] == false && m["decision_backend"] == true)
            .map(|m| m["label"].clone())
            .collect();
        assert_eq!(
            json!(names),
            json!([
                "Clef",
                "Clef-flash",
                "TypeSafe Jev 1.14.0",
                "GPT-6 Luna Decisions",
                "Gemini Decision 1"
            ])
        );
        let chat = json!({"id":"gemini/test","provider":"gemini","protocol":"openai_compat","base_url":"https://proxy.example/v1beta/openai","api_key_env":"TEST_GEMINI_KEY"});
        ui_store::put(&db, "model", "gemini/test", chat).unwrap();
        register_typed(&db).unwrap();
        let row = ui_store::get(&db, "model", "gemini/gemini-decision-1")
            .unwrap()
            .unwrap();
        assert_eq!(row["base_url"], "https://proxy.example/v1beta");
        assert_eq!(row["api_key_env"], "TEST_GEMINI_KEY");
        assert_eq!(row["protocol"], "gemini_decisions");
        register_typed(&db).unwrap();
        assert_eq!(
            ui_store::get(&db, "model", "gemini/gemini-decision-1")
                .unwrap()
                .unwrap(),
            row
        );
    }
}
