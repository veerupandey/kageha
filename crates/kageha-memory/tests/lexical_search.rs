use kageha_contracts::{MemoryAccess, MemoryPort, PutMemory};
use kageha_memory::sqlite::SqliteMemory;

fn access(namespace: &str, actor: &str) -> MemoryAccess {
    MemoryAccess {
        namespace: namespace.into(),
        actor: actor.into(),
    }
}

fn note(access: MemoryAccess, id: &str, body: &str) -> PutMemory {
    PutMemory {
        request_id: format!("put-{id}-{}", access.actor),
        access,
        id: id.into(),
        kind: "note".into(),
        body: body.into(),
        metadata: Default::default(),
        acl: Default::default(),
        expected_revision: None,
    }
}

#[tokio::test]
async fn search_ranks_full_then_partial_matches_and_searches_ids() {
    let temp = tempfile::tempdir().unwrap();
    let memory = SqliteMemory::open(&temp.path().join("memory.sqlite")).unwrap();
    let owner = access("workspace-a", "main");
    for (id, body) in [
        ("partial", "postal preferences for packages"),
        ("full", "postal preferences and address"),
        ("literal", "Confirmed postal address in profile"),
        ("user-profile", "Verified home address in personal details"),
        ("unrelated", "Favorite hiking trails"),
    ] {
        memory.put(note(owner.clone(), id, body)).await.unwrap();
    }
    let results = memory.search(&owner, "postal address", 10).await.unwrap();
    let ids: Vec<_> = results.iter().map(|record| record.id.as_str()).collect();
    assert_eq!(&ids[..2], ["literal", "full"]);
    assert!(ids.contains(&"partial"));
    assert!(!ids.contains(&"unrelated"));
    let profile = memory.search(&owner, "user profile", 10).await.unwrap();
    assert_eq!(profile.first().unwrap().id, "user-profile");
    let profile_by_details = memory
        .search(&owner, "home address postal code", 10)
        .await
        .unwrap();
    assert!(profile_by_details
        .iter()
        .any(|record| record.id == "user-profile"));
    assert_eq!(
        memory.search(&owner, "postal address", 1).await.unwrap()[0].id,
        "literal"
    );
    assert!(memory
        .search(&owner, "unknown unrelated terms", 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn partial_and_id_matches_respect_owner_namespace_acl_and_tombstones() {
    let temp = tempfile::tempdir().unwrap();
    let memory = SqliteMemory::open(&temp.path().join("memory.sqlite")).unwrap();
    let owner = access("workspace-a", "main");
    let reader = access("workspace-a", "reader");
    let outsider = access("workspace-a", "outsider");
    let other = access("workspace-b", "main");
    let mut shared = note(owner.clone(), "shared-profile", "postal preferences");
    shared.acl.insert("reader".into());
    memory.put(shared).await.unwrap();
    memory
        .put(note(owner.clone(), "private-profile", "postal preferences"))
        .await
        .unwrap();
    memory
        .put(note(other.clone(), "other-profile", "postal preferences"))
        .await
        .unwrap();
    assert_eq!(
        memory
            .search(&reader, "profile postal address", 10)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(memory
        .search(&outsider, "profile", 10)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(memory.search(&other, "profile", 10).await.unwrap().len(), 1);
    memory
        .delete(kageha_contracts::DeleteMemory {
            request_id: "delete-shared".into(),
            access: owner,
            id: "shared-profile".into(),
            expected_revision: None,
        })
        .await
        .unwrap();
    assert!(memory
        .search(&reader, "shared profile", 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn postal_queries_find_residential_candidates_without_bypassing_access() {
    let temp = tempfile::tempdir().unwrap();
    let memory = SqliteMemory::open(&temp.path().join("memory.sqlite")).unwrap();
    let owner = access("workspace-a", "main");
    let outsider = access("workspace-a", "other");
    memory
        .put(note(
            owner.clone(),
            "user-profile",
            "Home address: 123 Example Street, Vancouver, BC A1A 1A1",
        ))
        .await
        .unwrap();
    memory
        .put(note(owner.clone(), "exact", "Postal code: B2B 2B2"))
        .await
        .unwrap();
    memory
        .put(note(
            owner.clone(),
            "email",
            "Email address for home insurance enquiries",
        ))
        .await
        .unwrap();
    memory
        .put(note(owner.clone(), "unrelated", "Postal service updates"))
        .await
        .unwrap();
    let results = memory.search(&owner, "postal code", 10).await.unwrap();
    let ids: Vec<_> = results.iter().map(|record| record.id.as_str()).collect();
    assert_eq!(ids[0], "exact");
    assert!(ids.contains(&"user-profile"));
    // The residential-address fallback needs an address marker, and an email
    // address alone must not masquerade as a residential address.
    assert!(!ids.contains(&"email"));
    assert!(memory
        .search(&outsider, "postal code", 10)
        .await
        .unwrap()
        .is_empty());
    let results = memory
        .search(&owner, "what is my home address and postal code", 10)
        .await
        .unwrap();
    assert!(results.iter().any(|record| record.id == "user-profile"));

    let reader = access("workspace-a", "reader");
    let mut shared = note(
        owner.clone(),
        "shared-address",
        "Residential address: 42 Example Street",
    );
    shared.acl.insert("reader".into());
    memory.put(shared).await.unwrap();
    assert_eq!(
        memory.search(&reader, "postal code", 10).await.unwrap()[0].id,
        "shared-address"
    );
    memory
        .delete(kageha_contracts::DeleteMemory {
            request_id: "delete-shared-address".into(),
            access: owner,
            id: "shared-address".into(),
            expected_revision: None,
        })
        .await
        .unwrap();
    assert!(memory
        .search(&reader, "postal code", 10)
        .await
        .unwrap()
        .is_empty());
}
