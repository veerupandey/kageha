//! Host-independent projection of the shared agent session feed.
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub struct AgentsView {
    pub workers: BTreeMap<String, Value>,
    pub sequence: u64,
    tools: BTreeMap<String, Value>,
    snapshot_floor: u64,
}
pub fn clean(value: &str, limit: usize) -> String {
    let text: String = value.chars().filter(|c| !c.is_control()).collect();
    if text.chars().count() > limit {
        text.chars()
            .take(limit.saturating_sub(1))
            .chain(['…'])
            .collect()
    } else {
        text
    }
}
pub fn terminal(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "cancelled" | "interrupted")
}
pub fn status(state: &str) -> &str {
    match state {
        "running" => "Working",
        "queued" => "Queued",
        "waiting_input" => "Needs input",
        "succeeded" => "Finished",
        "failed" => "Failed",
        "cancelled" => "Cancelled",
        "cancelling" => "Stopping",
        "interrupted" => "Interrupted",
        "reconciling" => "Needs review",
        _ => "Waiting",
    }
}
fn elapsed(worker: &Value, now: f64) -> u64 {
    if let Some(seconds) = worker["progress"]["elapsed_s"]
        .as_f64()
        .or_else(|| worker["elapsed_s"].as_f64())
    {
        return seconds.max(0.0).min(u64::MAX as f64) as u64;
    }
    let started = worker["started_at"]
        .as_f64()
        .or_else(|| worker["created_at"].as_f64())
        .unwrap_or(now);
    let end = if terminal(worker["state"].as_str().unwrap_or("")) {
        worker["ended_at"]
            .as_f64()
            .or_else(|| worker["finished_at"].as_f64())
            .unwrap_or(started)
    } else {
        now
    };
    (end - started).max(0.0).min(u64::MAX as f64) as u64
}
pub fn duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60)
    } else if seconds >= 60 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}
impl AgentsView {
    /// State projection only: no input, approval or execution is admitted here.
    pub fn apply(&mut self, event: &str, packet: &Value) -> bool {
        if event == "snapshot" {
            self.snapshot_floor = packet["sequence"].as_u64().unwrap_or(self.sequence);
            self.tools.clear();
            self.workers.clear();
            for worker in packet["workers"].as_array().into_iter().flatten() {
                if let Some(id) = worker["id"].as_str() {
                    self.workers.insert(id.into(), worker.clone());
                }
            }
            return true;
        }
        let mut changed = false;
        for frame in packet["events"].as_array().into_iter().flatten() {
            let sequence = frame["sequence"].as_u64().unwrap_or(0);
            if sequence <= self.sequence {
                continue;
            }
            self.sequence = sequence;
            if sequence <= self.snapshot_floor {
                continue;
            }
            if frame["kind"] == "worker" {
                if let Some(id) = frame["job"]["id"].as_str() {
                    self.workers.insert(id.into(), frame["job"].clone());
                    changed = true;
                }
            }
            if frame["kind"] == "worker_activity" && frame["activity"]["kind"] == "tool_card" {
                if let Some(id) = frame["job_id"].as_str() {
                    self.tools
                        .insert(id.into(), frame["activity"]["tool_card"].clone());
                    changed = true;
                }
            }
        }
        changed
    }
    /// Parent references are explicit job IDs or stable child-session relationships.
    pub fn tree(&self) -> Vec<(String, usize)> {
        let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut roots = Vec::new();
        for (id, worker) in &self.workers {
            let parent = worker["parent_job_id"]
                .as_str()
                .filter(|p| *p != id && self.workers.contains_key(*p))
                .map(str::to_owned)
                .or_else(|| {
                    worker["parent_session"].as_str().and_then(|s| {
                        self.workers
                            .iter()
                            .find(|(other, w)| *other != id && w["session_id"] == s)
                            .map(|(id, _)| id.clone())
                    })
                });
            if let Some(parent) = parent {
                children.entry(parent).or_default().push(id.clone());
            } else {
                roots.push(id.clone());
            }
        }
        fn visit(
            id: &str,
            depth: usize,
            children: &BTreeMap<String, Vec<String>>,
            seen: &mut BTreeSet<String>,
            out: &mut Vec<(String, usize)>,
        ) {
            if !seen.insert(id.into()) {
                return;
            }
            out.push((id.into(), depth.min(8)));
            if let Some(rows) = children.get(id) {
                for child in rows {
                    visit(child, depth + 1, children, seen, out);
                }
            }
        }
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for root in roots {
            visit(&root, 0, &children, &mut seen, &mut out);
        }
        for id in self.workers.keys() {
            visit(id, 0, &children, &mut seen, &mut out);
        }
        out
    }
    pub fn resolve(&self, query: &str) -> Option<String> {
        if self.workers.contains_key(query) {
            return Some(query.into());
        }
        if let Ok(index) = query.parse::<usize>() {
            return index
                .checked_sub(1)
                .and_then(|i| self.tree().get(i).map(|(id, _)| id.clone()));
        }
        let matches = self
            .workers
            .keys()
            .filter(|id| id.starts_with(query))
            .collect::<Vec<_>>();
        if !query.is_empty() && matches.len() == 1 {
            Some(matches[0].clone())
        } else {
            None
        }
    }
    pub fn current_tool(&self, id: &str) -> Option<&Value> {
        self.workers
            .get(id)
            .and_then(|w| w["progress"].get("current_tool"))
            .filter(|v| v.is_object() && !v["name"].is_null())
            .or_else(|| self.tools.get(id))
    }
    pub fn render(&self, width: usize) -> String {
        self.render_selected(width, None, now())
    }
    pub fn render_selected(&self, width: usize, selected: Option<&str>, now: f64) -> String {
        let active = self
            .workers
            .values()
            .filter(|w| !terminal(w["state"].as_str().unwrap_or("")))
            .count();
        let finished = self
            .workers
            .values()
            .filter(|w| w["state"] == "succeeded")
            .count();
        let mut out = format!("Agents  ·  {active} active  ·  {finished} finished\n");
        if self.workers.is_empty() {
            out.push_str("  No agents yet. Waiting for delegated work.\n");
        }
        for (index, (id, depth)) in self.tree().into_iter().enumerate() {
            let worker = &self.workers[&id];
            let state = worker["state"].as_str().unwrap_or("queued");
            let marker = match state {
                "succeeded" => "✓",
                "failed" | "interrupted" => "!",
                "running" => "●",
                _ => "○",
            };
            let name_width = width.clamp(40, 180).saturating_sub(32 + depth * 2);
            let name = clean(worker["label"].as_str().unwrap_or("Agent"), name_width);
            let branch = if depth > 0 {
                format!("{}└─ ", "  ".repeat(depth - 1))
            } else {
                String::new()
            };
            let focus = if selected == Some(&id) { ">" } else { " " };
            out.push_str(&format!(
                "{focus} {:>2} {branch}{marker} {name}  {} · {}\n",
                index + 1,
                status(state),
                duration(elapsed(worker, now))
            ));
            if let Some(tool) = self.current_tool(&id) {
                let name = clean(
                    tool["label"]
                        .as_str()
                        .or_else(|| tool["name"].as_str())
                        .unwrap_or("tool"),
                    width.saturating_sub(24),
                );
                let action = if !terminal(state)
                    && matches!(tool["status"].as_str(), Some("running" | "in_progress"))
                {
                    "Current tool"
                } else {
                    "Last tool"
                };
                out.push_str(&format!("     {action}: {name}\n"));
            }
            let summary = worker["progress"]["summary"]
                .as_str()
                .or_else(|| worker["progress"]["agent_update"]["summary"].as_str());
            if let Some(summary) = summary.filter(|s| !s.is_empty()) {
                out.push_str(&format!(
                    "     {}\n",
                    clean(summary, width.saturating_sub(6))
                ));
            }
            let steps = worker["progress"]["steps"].as_u64().unwrap_or(0);
            if steps > 0 {
                out.push_str(&format!(
                    "     {steps} tool {} recorded\n",
                    if steps == 1 { "call" } else { "calls" }
                ));
            }
            if let Some(question) = worker["question"]["question"].as_str() {
                out.push_str(&format!(
                    "     Needs input: {}\n",
                    clean(question, width.saturating_sub(18))
                ));
            }
        }
        out
    }
    pub fn inspect(&self, id: &str, width: usize) -> Option<String> {
        let worker = self.workers.get(id)?;
        let mut out = format!(
            "{}\n{}\n",
            clean(worker["label"].as_str().unwrap_or("Agent"), width),
            status(worker["state"].as_str().unwrap_or(""))
        );
        for (label, value) in [
            ("Agent", Some(id)),
            ("Session", worker["session_id"].as_str()),
            ("Parent agent", worker["parent_job_id"].as_str()),
            ("Assignment", worker["objective"].as_str()),
            ("Result", worker["result"]["message"].as_str()),
            (
                "Approval",
                worker["question"]["approval_id"]
                    .as_str()
                    .or_else(|| worker["error"]["approval_id"].as_str()),
            ),
            ("Question", worker["question"]["question"].as_str()),
            ("Resume blocker", worker["resume_blocker"].as_str()),
        ] {
            if let Some(value) = value.filter(|s| !s.is_empty()) {
                out.push_str(&format!("\n{label}\n{}\n", clean(value, 8192)));
            }
        }
        if let Some(tool) = self.current_tool(id) {
            out.push_str(&format!(
                "\nTool\n{} ({})\n",
                clean(tool["name"].as_str().unwrap_or("tool"), 256),
                clean(tool["status"].as_str().unwrap_or("unknown"), 64)
            ));
        }
        Some(out)
    }
}
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or_default()
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn child_status_is_independent_and_replay_is_idempotent() {
        let mut view = AgentsView::default();
        view.apply("snapshot", &json!({"workers":[]}));
        let update = json!({"events":[{"sequence":2,"kind":"worker","job":{"id":"child","label":"Review","state":"running","progress":{"steps":1}}},{"sequence":3,"kind":"done"}]});
        assert!(view.apply("update", &update));
        assert!(view.render(70).contains("1 active"));
        assert!(!view.apply("update", &update));
        assert!(view.render(70).contains("1 tool call recorded"));
        view.apply("update",&json!({"events":[{"sequence":4,"kind":"worker","job":{"id":"child","label":"Review","state":"succeeded"}}]}));
        assert!(view.render(70).contains("1 finished"));
    }
    #[test]
    fn terminal_input_is_sanitized_and_narrow_output_bounded() {
        let mut view = AgentsView::default();
        view.apply("snapshot",&json!({"workers":[{"id":"a","label":"\u{1b}[2J\nInjected title very long very long very long","state":"failed"}]}));
        let text = view.render(40);
        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains("\nInjected"));
        assert!(text.contains("Failed"));
        assert!(text.contains('…'));
    }
    #[test]
    fn replay_before_current_snapshot_cannot_regress_worker() {
        let mut view = AgentsView::default();
        view.apply(
            "snapshot",
            &json!({"sequence":12,"workers":[{"id":"child","state":"succeeded","label":"Checks"}]}),
        );
        assert!(!view.apply("update",&json!({"events":[{"sequence":8,"kind":"worker","job":{"id":"child","state":"running","label":"Checks"}}]})));
        assert_eq!(view.sequence, 8);
        assert_eq!(view.workers["child"]["state"], "succeeded");
        assert!(view.apply("update",&json!({"events":[{"sequence":13,"kind":"worker","job":{"id":"child","state":"running","label":"Checks"}}]})));
    }
    #[test]
    fn nested_tree_keeps_tool_activity_and_cycles_bounded() {
        let mut view = AgentsView::default();
        view.apply("snapshot",&json!({"workers":[{"id":"root","session_id":"s1","state":"running"},{"id":"child","parent_session":"s1","state":"waiting_input","question":{"question":"Allow upload?","approval_id":"p1"}},{"id":"cycle","parent_job_id":"cycle","state":"failed"}]}));
        view.apply("update",&json!({"events":[{"sequence":1,"kind":"worker_activity","job_id":"child","activity":{"kind":"tool_card","tool_card":{"name":"upload","status":"running"}}}]}));
        assert_eq!(view.tree().len(), 3);
        assert_eq!(
            view.tree().iter().find(|(id, _)| id == "child").unwrap().1,
            1
        );
        let text = view.render_selected(80, Some("child"), 10.0);
        assert!(text.contains("Current tool: upload"));
        assert!(text.contains("Needs input: Allow upload?"));
        assert!(view.inspect("child", 80).unwrap().contains("p1"));
        assert_eq!(view.resolve("child"), Some("child".into()));
    }
}
