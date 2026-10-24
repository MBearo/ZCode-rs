use super::*;

fn prefix(command: &str) -> Option<String> {
    spec_prefix(command)
}

#[test]
fn the_depth_table_lengthens_known_commands() {
    assert_eq!(
        prefix("docker compose up -d").as_deref(),
        Some("docker compose up")
    );
    assert_eq!(
        prefix("kubectl get pods -n prod").as_deref(),
        Some("kubectl get pods")
    );
    assert_eq!(
        prefix("gcloud compute instances list --zone x").as_deref(),
        Some("gcloud compute instances list")
    );
    assert_eq!(prefix("git push origin main").as_deref(), Some("git push"));
    assert_eq!(prefix("aws s3 cp a s3://b").as_deref(), Some("aws s3 cp a"));
}

#[test]
fn options_and_paths_end_the_prefix() {
    assert_eq!(prefix("git -C /repo status").as_deref(), Some("git"));
    assert_eq!(prefix("mytool --flag").as_deref(), Some("mytool"));
    assert_eq!(prefix("python3 -c 'print(1)'").as_deref(), Some("python3"));
    assert_eq!(prefix("mytool src/main.rs").as_deref(), Some("mytool"));
    assert_eq!(prefix("mytool file.txt").as_deref(), Some("mytool"));
    assert_eq!(prefix("mytool a:b.c:d").as_deref(), Some("mytool a:b.c:d"));
    assert_eq!(prefix("curl https://example.com").as_deref(), Some("curl"));
    assert_eq!(prefix("mytool sub x").as_deref(), Some("mytool sub"));
}

#[test]
fn wrappers_keep_their_arguments_up_to_the_command() {
    assert_eq!(
        prefix("timeout 10 mytool run x").as_deref(),
        Some("timeout 10 mytool run")
    );
    assert_eq!(
        prefix("nohup mytool run x").as_deref(),
        Some("nohup mytool run")
    );
    assert_eq!(
        prefix("time mytool run").as_deref(),
        Some("time mytool run")
    );
    assert_eq!(
        prefix("nice -n 10 mytool build x").as_deref(),
        Some("nice mytool build")
    );
    assert_eq!(prefix("nice -n 10").as_deref(), Some("nice"));
}

#[test]
fn specs_without_a_command_argument() {
    // 参数可变：深度 1。
    assert_eq!(prefix("pyright -p proj src").as_deref(), Some("pyright"));
    assert_eq!(prefix("alias ll='ls -l'").as_deref(), Some("alias"));
    // 第一个参数必填：深度 2。
    assert_eq!(prefix("sleep 5").as_deref(), Some("sleep 5"));
}

#[test]
fn assignments_stay_in_front() {
    assert_eq!(
        prefix("FOO=1 mytool build x").as_deref(),
        Some("FOO=1 mytool build")
    );
    assert_eq!(prefix("FOO=1").as_deref(), None);
}
