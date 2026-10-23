use super::*;

fn round_trip<T>(value: &T)
where
    T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
{
    let text = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&text).unwrap();
    assert_eq!(&back, value);
}

#[test]
fn rewind_dtos_round_trip() {
    round_trip(&RewindPrepare {
        files: vec![
            RestoreFile {
                path: EnvPath::new("/work/a.txt"),
                expected: Some("ab".to_owned()),
                restore: Some(b"old".to_vec()),
            },
            RestoreFile {
                path: EnvPath::new("/work/new.txt"),
                expected: Some("cd".to_owned()),
                restore: None,
            },
        ],
    });
    round_trip(&RewindPlan::Conflicts {
        files: vec![RewindConflict {
            path: EnvPath::new("/work/a.txt"),
            current: None,
            message: None,
        }],
    });
    round_trip(&RewindStatus::Applied {
        files: vec![RestoredFile {
            path: EnvPath::new("/work/a.txt"),
            action: RestoreAction::Restore,
        }],
    });
    round_trip(&RewindStatus::Failed {
        message: "conflict".to_owned(),
        conflicts: Vec::new(),
    });
}

#[test]
fn content_hash_is_sha256_hex() {
    // Node: createHash("sha256").update("abc").digest("hex")
    assert_eq!(
        content_hash(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
