//! Offline projection into the workspace-owned catalog; no application table sharing.
use kageha_migration::{
    documents::{Document, DocumentSink, Domain},
    Importer, Report,
};
use kageha_workspace::projects::{import::Archive, Projects};
use rusqlite::{Connection, OptionalExtension};
use std::{io, path::Path};
struct Projection {
    catalog: Projects,
    chats: Connection,
}
fn archive(d: &Document) -> io::Result<Archive> {
    serde_json::from_value(
        d.value
            .clone()
            .ok_or_else(|| io::Error::other("Project snapshot is missing"))?,
    )
    .map_err(io::Error::other)
}
impl DocumentSink for Projection {
    fn apply(&mut self, d: &Document) -> io::Result<()> {
        let archive = archive(d)?;
        if self.catalog.has_import("instance", &d.source_id)? {
            self.catalog
                .import_archive("instance", &d.source_id, &d.source_digest, &archive)?;
            return Ok(());
        }
        for chat in archive.sessions.keys() {
            let exists = self
                .chats
                .query_row("SELECT 1 FROM ui_sessions WHERE id=?", [chat], |r| {
                    r.get::<_, u8>(0)
                })
                .optional()
                .map_err(io::Error::other)?
                .is_some();
            if !exists {
                return Err(io::Error::other(format!(
                    "Import chat history {chat} before its project associations"
                )));
            }
        }
        self.catalog
            .import_archive("instance", &d.source_id, &d.source_digest, &archive)?;
        Ok(())
    }
    fn verify(&mut self, d: &Document) -> io::Result<()> {
        self.catalog
            .verify_archive("instance", &d.source_id, &d.source_digest, &archive(d)?)
    }
}
pub fn run(source: &Path, state: &Path) -> io::Result<Report> {
    let destination = crate::history_migration::destination(source, state)?;
    let mut projection = Projection {
        catalog: Projects::open(
            &destination
                .state
                .join("components/workspace/projects.sqlite"),
        )?,
        chats: destination.db,
    };
    Importer::open(
        &destination.source,
        &destination.state.join("document-import-projects-v1"),
    )?
    .run_documents(Domain::Projects, &mut projection)
}
