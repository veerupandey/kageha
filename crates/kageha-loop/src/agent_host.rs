//! Application composition only. Profile data and writes belong to kageha-agents.
use crate::webui::Database;
use kageha_agents::repository::{sqlite::SqliteRepository, MemoryRepository, Repository};
use rusqlite::Connection;
use std::{
    io,
    sync::{Arc, LockResult, Mutex, MutexGuard},
};
pub(crate) struct DatabaseState {
    connection: Mutex<Connection>,
    pub(crate) profiles: Arc<dyn Repository>,
    /// The Run_Journal; off until `ui_learning::install` opens the learning store.
    pub(crate) learning: crate::ui_learning::Journal,
    /// The background reflection worker's wake-up signal; off until `ui_reflection::install`.
    pub(crate) reflection: crate::ui_reflection::Reflector,
}
impl DatabaseState {
    pub(crate) fn open(connection: Connection) -> io::Result<Database> {
        crate::ui_store::initialize(&connection).map_err(io::Error::other)?;
        crate::ui_agents::initialize(&connection).map_err(io::Error::other)?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS ui_agent_repository_identity(scope TEXT PRIMARY KEY);CREATE TABLE IF NOT EXISTS ui_agent_repository_cutover(scope TEXT PRIMARY KEY);").map_err(io::Error::other)?;
        let scope = {
            let tx = connection
                .unchecked_transaction()
                .map_err(io::Error::other)?;
            let rows: Vec<String> = {
                let mut q = tx
                    .prepare("SELECT scope FROM ui_agent_repository_identity")
                    .map_err(io::Error::other)?;
                let v = q
                    .query_map([], |r| r.get(0))
                    .map_err(io::Error::other)?
                    .collect::<Result<_, _>>()
                    .map_err(io::Error::other)?;
                v
            };
            let id = match rows.as_slice() {
                [] => {
                    let id = format!("webui-agents:{}", uuid::Uuid::new_v4());
                    tx.execute("INSERT INTO ui_agent_repository_identity VALUES (?)", [&id])
                        .map_err(io::Error::other)?;
                    id
                }
                [id] => id.clone(),
                _ => return Err(io::Error::other("Ambiguous agent repository identity")),
            };
            tx.commit().map_err(io::Error::other)?;
            id
        };
        let cutover: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ui_agent_repository_cutover WHERE scope=?)",
                [&scope],
                |r| r.get(0),
            )
            .map_err(io::Error::other)?;
        let mut source = crate::agent_export::export(&connection)?;
        source.source = format!("legacy:{scope}");
        let profiles: Arc<dyn Repository> = if let Some(path) = connection
            .path()
            .filter(|p| !p.is_empty() && *p != ":memory:")
        {
            let file = std::path::Path::new(path);
            let parent = file.parent().unwrap_or(std::path::Path::new("."));
            let owned_path = parent.join("components/agents/profiles.sqlite");
            if cutover && !owned_path.exists() {
                return Err(io::Error::other("Owned agent store is missing; restore the component backup instead of rebuilding from stale legacy profiles"));
            }
            Arc::new(SqliteRepository::open(&owned_path, &scope)?)
        } else {
            Arc::new(MemoryRepository::new("ephemeral-webui")?)
        };
        // Completed imports are idempotent even after later owned-store writes.
        // A changed legacy source is a blocker, never an implicit overwrite.
        if cutover
            && !profiles
                .import_status(&source.source)?
                .is_some_and(|s| s.complete())
        {
            return Err(io::Error::other(
                "Owned agent store has lost its cutover checkpoint; restore the component backup",
            ));
        }
        profiles.import(&source, 4096)?;
        for original in &source.entries {
            let owned = profiles
                .entry(original.kind, &original.id)?
                .ok_or_else(|| {
                    io::Error::other(
                        "Owned profile history is missing; restore the component backup",
                    )
                })?;
            if !owned.revisions.starts_with(&original.revisions) {
                return Err(io::Error::other("Owned profile history no longer contains its imported revisions; restore or reconcile the component backup"));
            }
        }
        if profiles.get("main")?.is_none() {
            return Err(io::Error::other("Owned default agent is missing; restore its existing revision before starting the WebUI"));
        }
        connection
            .execute(
                "INSERT OR IGNORE INTO ui_agent_repository_cutover VALUES (?)",
                [&scope],
            )
            .map_err(io::Error::other)?;
        Ok(Arc::new(Self {
            connection: Mutex::new(connection),
            profiles,
            learning: Default::default(),
            reflection: Default::default(),
        }))
    }
    pub(crate) fn lock(&self) -> LockResult<MutexGuard<'_, Connection>> {
        // A worker panic must not permanently turn every HTTP request into a
        // 500. SQLite remains transactionally safe here; callers can continue
        // using the recovered connection while the failed turn is reported.
        Ok(self.connection.lock().unwrap_or_else(|poison| poison.into_inner()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kageha_agents::repository::Kind;
    use serde_json::json;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    #[test]
    fn a_panicked_holder_does_not_permanently_poison_the_web_database() {
        let connection = Connection::open_in_memory().unwrap();
        crate::ui_store::initialize(&connection).unwrap();
        crate::ui_agents::initialize(&connection).unwrap();
        let db = DatabaseState::open(connection).unwrap();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _guard = db.connection.lock().unwrap();
            panic!("disposable worker panic");
        }));
        assert!(result.is_err());
        assert!(db.lock().is_ok());
    }

    #[test]
    fn owned_writes_and_deletion_survive_restart_without_changing_legacy_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite");
        let c = Connection::open(&path).unwrap();
        crate::ui_store::initialize(&c).unwrap();
        crate::ui_agents::initialize(&c).unwrap();
        crate::ui_store::put(
            &c,
            "agent",
            "reviewer",
            json!({"id":"reviewer","name":"Legacy","system_prompt":"Original"}),
        )
        .unwrap();
        c.execute(
            "INSERT INTO ui_agent_source_revisions VALUES ('reviewer','legacy-alias',1)",
            [],
        )
        .unwrap();
        let before = crate::agent_export::export(&c).unwrap().digest().unwrap();
        let db = DatabaseState::open(c).unwrap();
        assert!(DatabaseState::open(Connection::open(&path).unwrap()).is_err());
        let mut v = db.profiles.get("reviewer").unwrap().unwrap().body().clone();
        v["system_prompt"] = json!("Owned edit");
        db.profiles
            .write(Kind::Agent, "reviewer", 1, Some(v), 2.0)
            .unwrap();
        assert_eq!(
            crate::agent_export::export(&db.lock().unwrap())
                .unwrap()
                .digest()
                .unwrap(),
            before
        );
        drop(db);
        let db = DatabaseState::open(Connection::open(&path).unwrap()).unwrap();
        assert_eq!(
            db.profiles.get("reviewer").unwrap().unwrap().instructions(),
            "Owned edit"
        );
        assert_eq!(
            db.profiles
                .entry(Kind::Agent, "reviewer")
                .unwrap()
                .unwrap()
                .revisions[0]
                .source_revision
                .as_deref(),
            Some("legacy-alias")
        );
        db.profiles
            .write(Kind::Agent, "reviewer", 2, None, 3.0)
            .unwrap();
        drop(db);
        let db = DatabaseState::open(Connection::open(&path).unwrap()).unwrap();
        assert!(db.profiles.get("reviewer").unwrap().is_none());
        assert_eq!(
            crate::agent_export::export(&db.lock().unwrap())
                .unwrap()
                .digest()
                .unwrap(),
            before
        );
        drop(db);
        let c = Connection::open(&path).unwrap();
        crate::ui_store::put(
            &c,
            "agent",
            "reviewer",
            json!({"id":"reviewer","name":"Unexpected legacy edit"}),
        )
        .unwrap();
        assert!(DatabaseState::open(c).is_err());
    }

    #[test]
    fn moved_state_keeps_identity_and_missing_owned_data_cannot_reseed() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("one");
        std::fs::create_dir_all(&state).unwrap();
        let db = DatabaseState::open(Connection::open(state.join("app.sqlite")).unwrap()).unwrap();
        db.profiles
            .write(
                Kind::Agent,
                "custom",
                0,
                Some(json!({"id":"custom","name":"Owned custom"})),
                1.0,
            )
            .unwrap();
        let before = db.profiles.export().unwrap().digest().unwrap();
        drop(db);
        let moved = dir.path().join("two");
        std::fs::rename(&state, &moved).unwrap();
        let db = DatabaseState::open(Connection::open(moved.join("app.sqlite")).unwrap()).unwrap();
        assert_eq!(db.profiles.export().unwrap().digest().unwrap(), before);
        drop(db);
        let owned = moved.join("components/agents");
        let saved = moved.join("agents-backup");
        std::fs::rename(&owned, &saved).unwrap();
        assert!(DatabaseState::open(Connection::open(moved.join("app.sqlite")).unwrap()).is_err());
        assert!(!owned.join("profiles.sqlite").exists());
        std::fs::rename(&saved, &owned).unwrap();
        let db = DatabaseState::open(Connection::open(moved.join("app.sqlite")).unwrap()).unwrap();
        assert!(db.profiles.get("custom").unwrap().is_some());
        drop(db);
        // A recreated empty component file must not be accepted as the old store.
        std::fs::rename(&owned, &saved).unwrap();
        std::fs::create_dir_all(&owned).unwrap();
        let c = Connection::open(moved.join("app.sqlite")).unwrap();
        let scope: String = c
            .query_row("SELECT scope FROM ui_agent_repository_identity", [], |r| {
                r.get(0)
            })
            .unwrap();
        drop(SqliteRepository::open(&owned.join("profiles.sqlite"), &scope).unwrap());
        assert!(DatabaseState::open(Connection::open(moved.join("app.sqlite")).unwrap()).is_err());
    }
}
