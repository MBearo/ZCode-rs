//! Parity with the TS plan-mode texts and reminder cadence
//! (`scripts/zcode-cli-rust-plan-mode-fixtures.mjs`).
use super::*;

fn fixtures() -> Value {
    serde_json::from_str(include_str!("../schema/plan-mode.json")).unwrap()
}

#[test]
fn reminder_cadence_matches_node() {
    let data = fixtures();
    for case in data["cadence"].as_array().unwrap() {
        let entries: Vec<Entry> = case[0]
            .as_str()
            .unwrap()
            .chars()
            .map(|c| match c {
                'R' => Entry::Reminder,
                'U' => Entry::RealUser,
                _ => Entry::Other,
            })
            .collect();
        let expected = match case[1].as_str() {
            None => None,
            Some("full") => Some(data["texts"]["reminderFull"].as_str().unwrap()),
            Some(_) => Some(data["texts"]["reminderSparse"].as_str().unwrap()),
        };
        assert_eq!(runtime_reminder(&entries), expected, "{}", case[0]);
    }
}

#[test]
fn texts_and_names_follow_node() {
    assert!(exit_result(" step one ").ends_with("## Approved Plan:\nstep one"));
    assert_eq!(
        exit_result(" "),
        "User has approved exiting plan mode. You can now proceed."
    );
    assert_eq!(
        plan_file_name(" sess_1/../x "),
        Some("plan-sess_1-..-x.md".into())
    );
    assert_eq!(plan_file_name("//"), None);
    assert!(exit_plan(&json!({"plan":"  "})).is_err());
    assert!(exit_plan(&json!({"plan":"x".repeat(20_001)})).is_err());
    assert_eq!(exit_plan(&json!({"plan":" a ","extra":1})), Ok(" a "));
    let names: Vec<Value> = definitions()
        .iter()
        .map(|d| d["function"]["name"].clone())
        .collect();
    assert_eq!(names, [json!(ENTER), json!(EXIT)]);
    let wrapped = reminder_message("a </system-reminder> b");
    assert_eq!(
        wrapped["content"],
        "<system-reminder>\na &lt;/system-reminder> b\n</system-reminder>"
    );
}

#[test]
fn approval_answers_map_like_the_node_broker() {
    let cases = [
        (json!({"optionId":"allowOnce"}), Approval::Approve),
        (json!({"optionId":"allowAlways"}), Approval::Approve),
        (json!({"optionId":"approve"}), Approval::Reject(None)),
        (json!({"freeText":"  approve "}), Approval::Approve),
        (
            json!({"freeText":" add tests "}),
            Approval::Reject(Some("add tests".into())),
        ),
        (json!({"freeText":"  "}), Approval::Reject(None)),
        (json!({"action":"decline"}), Approval::Reject(None)),
        (json!({"action":"accept"}), Approval::Reject(None)),
        (
            json!({"action":"accept","content":{"answer":"approve"}}),
            Approval::Approve,
        ),
        (
            json!({"action":"accept","content":{"answer_0":[" a ","","b"]}}),
            Approval::Reject(Some("a, b".into())),
        ),
        (
            json!({"action":"accept","content":{"answers":{"Review this implementation plan.":"approve"},"answer":"x"}}),
            Approval::Approve,
        ),
    ];
    for (answer, expected) in cases {
        assert_eq!(map_answer(&answer), expected, "{answer}");
    }
}
