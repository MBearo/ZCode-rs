use super::*;

fn args(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

/// Host 给的参数（`app-server --stdio --surface desktop`）与它的两种写法；不是这个子命令时不认。
#[test]
fn the_host_arguments_select_the_app_server() {
    assert_eq!(
        parse(&args("app-server --stdio --surface desktop")),
        Some(Ok(Parsed {
            surface: Some("desktop".to_owned())
        }))
    );
    assert_eq!(
        parse(&args("app-server --surface=desktop --stdio")),
        Some(Ok(Parsed {
            surface: Some("desktop".to_owned())
        }))
    );
    assert_eq!(
        parse(&args("app-server --stdio")),
        Some(Ok(Parsed { surface: None }))
    );
    assert!(matches!(parse(&args("app-server")), Some(Err(_))));
    assert!(matches!(
        parse(&args("app-server --stdio --surface")),
        Some(Err(_))
    ));
    assert!(matches!(
        parse(&args("app-server --stdio -p")),
        Some(Err(_))
    ));
    assert_eq!(parse(&args("-p hello")), None);
    assert_eq!(parse(&[]), None);
}

#[test]
fn the_database_id_hides_the_path_and_is_stable_in_a_process() {
    let path = std::path::Path::new("/Users/dev/.zcode/cli/db/db.sqlite");
    let id = database_id(path);
    assert_eq!(id, database_id(path));
    assert!(!id.contains("Users"));
    assert!(id.len() <= 128);
}
