use serde_json::{json, Value};
pub fn board(value: &Value) -> Value {
    let items:Vec<_>=value["steps"].as_array().into_iter().flatten().map(|s|json!({"id":s["id"],"text":s["title"],"done":s["status"]=="completed","status":s.get("status").cloned().unwrap_or(json!("pending")),"evidence":s["evidence"].as_array().map(|v|v.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")),"depends_on":s["depends_on"]})).collect();
    let done = items.iter().filter(|v| v["done"] == true).count();
    let total = items.iter().filter(|v| v["status"] != "superseded").count();
    let outcomes = value["outcomes"].as_array().cloned().unwrap_or_default();
    let review = value["migration"]["interrupted"] == true;
    let verified = if review {
        0
    } else if outcomes.is_empty() {
        done
    } else {
        outcomes.iter().filter(|o| o["done"] == true).count()
    };
    let required = outcomes
        .iter()
        .filter(|o| o["required"] != false)
        .all(|o| o["done"] == true);
    json!({"version":1,"revision":value["revision"],"turn_id":value["turn_id"],"objective":value["objective"],"done":done,"total":total,"complete":done==total&&total>0&&required&&!review,"items":items,"outcomes":outcomes,"verified":verified,"outcome_total":if outcomes.is_empty(){total}else{outcomes.len()},"history_requires_review":review,"notice":value["notice"]})
}
pub fn markdown(value: &Value) -> String {
    let mut text = format!(
        "# Plan\n\n{}\n\n",
        value["objective"].as_str().unwrap_or("")
    );
    for step in value["steps"].as_array().into_iter().flatten() {
        text += &format!(
            "- [{}] {} — {}\n",
            if step["status"] == "completed" {
                "x"
            } else {
                " "
            },
            step["id"].as_str().unwrap_or(""),
            step["title"].as_str().unwrap_or("")
        );
    }
    if value["outcomes"].as_array().is_some_and(|v| !v.is_empty()) {
        text += "\n## Required outcomes\n\n";
        for outcome in value["outcomes"].as_array().into_iter().flatten() {
            text += &format!(
                "- {} — {}\n",
                outcome["id"].as_str().unwrap_or(""),
                outcome["text"].as_str().unwrap_or("")
            );
        }
    }
    text
}
