use std::cell::RefCell;
use std::collections::HashMap;

use super::*;

fn args(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_owned).collect()
}

/// 假的系统：记下跑过与启动过的命令。
#[derive(Default)]
struct Fake {
    folders: Vec<PathBuf>,
    files: Vec<PathBuf>,
    /// `open` 的结果；None 时按起不来。
    open: Option<bool>,
    start_error: Option<String>,
    registry: HashMap<(Hive, String), String>,
    ran: RefCell<Vec<(String, Vec<OsString>)>>,
    started: RefCell<Vec<(PathBuf, Vec<OsString>)>>,
}

impl Desktop for Fake {
    async fn is_folder(&self, path: &Path) -> bool {
        self.folders.iter().any(|folder| folder == path)
    }

    async fn is_file(&self, path: &Path) -> bool {
        self.files.iter().any(|file| file == path)
    }

    async fn run(&self, program: &str, args: &[OsString]) -> Result<bool, String> {
        self.ran
            .borrow_mut()
            .push((program.to_owned(), args.to_vec()));
        self.open
            .ok_or_else(|| "No such file or directory".to_owned())
    }

    async fn start(&self, program: &Path, args: &[OsString]) -> Result<(), String> {
        self.started
            .borrow_mut()
            .push((program.to_path_buf(), args.to_vec()));
        self.start_error.clone().map_or(Ok(()), Err)
    }

    async fn install_location(&self, hive: Hive, key: &str) -> Option<String> {
        self.registry.get(&(hive, key.to_owned())).cloned()
    }
}

fn os(items: &[&str]) -> Vec<OsString> {
    items.iter().map(OsString::from).collect()
}

fn no_env(_: &str) -> Option<String> {
    None
}

fn test_env(name: &str) -> Option<String> {
    (name == "ZCODE_ENV").then(|| "Test".to_owned())
}

fn repo() -> Fake {
    Fake {
        folders: vec![PathBuf::from("/w/repo")],
        ..Fake::default()
    }
}

async fn run_open(
    folder: Option<&str>,
    platform: Platform,
    var: &impl Fn(&str) -> Option<String>,
    fake: &Fake,
) -> Result<(), String> {
    open(folder, Path::new("/w/repo/src"), platform, var, fake).await
}

#[test]
fn app_is_a_verb_only_in_first_place() {
    assert_eq!(parse(&args("app")), Some(Ok(Command::Open(None))));
    assert_eq!(
        parse(&args("app ../x")),
        Some(Ok(Command::Open(Some("../x".to_owned()))))
    );
    assert_eq!(parse(&args("-p app")), None);
    assert_eq!(parse(&args("apps")), None);
    assert_eq!(parse(&[]), None);
}

#[test]
fn parse_follows_commander() {
    assert_eq!(parse(&args("app -h")), Some(Ok(Command::Help)));
    // 帮助先于未知选项与多余的参数。
    assert_eq!(
        parse(&args("app a b --nope --help")),
        Some(Ok(Command::Help))
    );
    assert_eq!(
        parse(&args("app --nope")),
        Some(Err("error: unknown option '--nope'".to_owned()))
    );
    assert_eq!(
        parse(&args("app a b")),
        Some(Err(
            "error: too many arguments for 'app'. Expected 1 argument but got 2.".to_owned()
        ))
    );
    // `--` 之后都是位置参数，`-` 本身也是。
    assert_eq!(
        parse(&args("app -- -h")),
        Some(Ok(Command::Open(Some("-h".to_owned()))))
    );
    assert_eq!(
        parse(&args("app -")),
        Some(Ok(Command::Open(Some("-".to_owned()))))
    );
    assert_eq!(
        parse(&args("app x -- y")),
        Some(Err(
            "error: too many arguments for 'app'. Expected 1 argument but got 2.".to_owned()
        ))
    );
}

#[tokio::test]
async fn help_goes_to_stdout() {
    let mut out = Vec::new();
    let code = command(Command::Help, &Default::default(), &mut out)
        .await
        .unwrap();
    assert_eq!(code, 0);
    assert!(
        String::from_utf8(out)
            .unwrap()
            .starts_with("Usage: zcode app [options] [folder]\n")
    );
}

#[test]
fn the_backend_picks_the_flavor() {
    assert_eq!(identity(&no_env), PRODUCTION);
    assert_eq!(identity(&test_env), PREVIEW);
    let production = |name: &str| (name == "ZCODE_ENV").then(|| "production".to_owned());
    assert_eq!(identity(&production), PRODUCTION);
}

#[tokio::test]
async fn the_folder_resolves_against_the_cwd() {
    let fake = repo();
    run_open(
        Some(".."),
        Platform::Mac,
        &no_env,
        &Fake {
            open: Some(true),
            ..fake
        },
    )
    .await
    .unwrap();
    let fake = Fake {
        open: Some(true),
        ..repo()
    };
    run_open(Some("../."), Platform::Mac, &no_env, &fake)
        .await
        .unwrap();
    run_open(Some("/w/repo"), Platform::Mac, &no_env, &fake)
        .await
        .unwrap();
    let ran = fake.ran.borrow();
    assert!(
        ran.iter()
            .all(|(_, args)| args.last() == Some(&OsString::from("/w/repo")))
    );
}

#[tokio::test]
async fn a_missing_folder_fails_before_looking_for_the_app() {
    let fake = Fake {
        open: Some(true),
        ..repo()
    };
    assert_eq!(
        run_open(Some("nope"), Platform::Mac, &no_env, &fake).await,
        Err("no folder “nope”".to_owned())
    );
    // 没给参数时写解析后的路径（cwd 不是文件夹的情形）。
    assert_eq!(
        open(None, Path::new("/gone"), Platform::Mac, &no_env, &fake).await,
        Err("no folder “/gone”".to_owned())
    );
    assert!(fake.ran.borrow().is_empty());
}

#[tokio::test]
async fn mac_asks_launch_services_for_a_new_instance() {
    let fake = Fake {
        open: Some(true),
        ..repo()
    };
    run_open(Some(".."), Platform::Mac, &test_env, &fake)
        .await
        .unwrap();
    assert_eq!(
        *fake.ran.borrow(),
        vec![(
            "open".to_owned(),
            os(&[
                "-n",
                "-b",
                "dev.zcode.app.preview",
                "--args",
                "--open-workspace",
                "/w/repo"
            ])
        )]
    );
    assert!(fake.started.borrow().is_empty());
}

#[tokio::test]
async fn mac_failures() {
    let fake = Fake {
        open: Some(false),
        ..repo()
    };
    assert_eq!(
        run_open(Some(".."), Platform::Mac, &no_env, &fake).await,
        Err("can't find the ZCode desktop app (no app with id dev.zcode.app)".to_owned())
    );
    let fake = repo();
    assert_eq!(
        run_open(Some(".."), Platform::Mac, &no_env, &fake).await,
        Err("couldn't run open: No such file or directory".to_owned())
    );
}

#[tokio::test]
async fn linux_starts_the_app_under_opt() {
    let fake = Fake {
        files: vec![PathBuf::from("/opt/ZCode Preview/zcode-preview")],
        ..repo()
    };
    run_open(Some(".."), Platform::Linux, &test_env, &fake)
        .await
        .unwrap();
    assert_eq!(
        *fake.started.borrow(),
        vec![(
            PathBuf::from("/opt/ZCode Preview/zcode-preview"),
            os(&["--open-workspace", "/w/repo"])
        )]
    );
}

#[tokio::test]
async fn linux_failures() {
    // 装的是另一个身份时不退过去。
    let fake = Fake {
        files: vec![PathBuf::from("/opt/ZCode Preview/zcode-preview")],
        ..repo()
    };
    assert_eq!(
        run_open(Some(".."), Platform::Linux, &no_env, &fake).await,
        Err("can't find the ZCode desktop app (no /opt/ZCode/zcode)".to_owned())
    );
    let fake = Fake {
        files: vec![PathBuf::from("/opt/ZCode/zcode")],
        start_error: Some("Permission denied (os error 13)".to_owned()),
        ..repo()
    };
    assert_eq!(
        run_open(Some(".."), Platform::Linux, &no_env, &fake).await,
        Err("couldn't start /opt/ZCode/zcode: Permission denied (os error 13)".to_owned())
    );
}

fn windows(registry: &[(Hive, &str)], files: &[&str]) -> Fake {
    Fake {
        registry: registry
            .iter()
            .map(|(hive, dir)| {
                (
                    (*hive, PRODUCTION.windows_guid.to_owned()),
                    (*dir).to_owned(),
                )
            })
            .collect(),
        files: files.iter().map(PathBuf::from).collect(),
        ..repo()
    }
}

fn local_app_data(name: &str) -> Option<String> {
    (name == "LOCALAPPDATA").then(|| "C:\\Users\\dev\\AppData\\Local".to_owned())
}

async fn windows_started(fake: &Fake, var: &impl Fn(&str) -> Option<String>) -> PathBuf {
    run_open(Some(".."), Platform::Windows, var, fake)
        .await
        .unwrap();
    let started = fake.started.borrow();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].1, os(&["--open-workspace", "/w/repo"]));
    started[0].0.clone()
}

#[tokio::test]
async fn windows_reads_the_installer_registry_first() {
    // 只为自己安装的先于为所有用户安装的；结尾的分隔符去掉。
    let fake = windows(
        &[
            (Hive::CurrentUser, "C:\\Users\\dev\\Apps\\ZCode\\"),
            (Hive::LocalMachine, "C:\\Program Files\\ZCode"),
        ],
        &[
            "C:\\Users\\dev\\Apps\\ZCode\\ZCode.exe",
            "C:\\Program Files\\ZCode\\ZCode.exe",
        ],
    );
    assert_eq!(
        windows_started(&fake, &local_app_data).await,
        PathBuf::from("C:\\Users\\dev\\Apps\\ZCode\\ZCode.exe")
    );
    // 登记的位置里没有可执行文件时往下找。
    let fake = windows(
        &[
            (Hive::CurrentUser, "C:\\Gone"),
            (Hive::LocalMachine, "C:\\Program Files\\ZCode"),
        ],
        &["C:\\Program Files\\ZCode\\ZCode.exe"],
    );
    assert_eq!(
        windows_started(&fake, &local_app_data).await,
        PathBuf::from("C:\\Program Files\\ZCode\\ZCode.exe")
    );
}

#[tokio::test]
async fn windows_falls_back_to_local_app_data() {
    let fake = windows(
        &[(Hive::CurrentUser, "")],
        &["C:\\Users\\dev\\AppData\\Local\\Programs\\ZCode\\ZCode.exe"],
    );
    assert_eq!(
        windows_started(&fake, &local_app_data).await,
        PathBuf::from("C:\\Users\\dev\\AppData\\Local\\Programs\\ZCode\\ZCode.exe")
    );
}

#[tokio::test]
async fn windows_without_the_app() {
    let fake = windows(&[], &[]);
    let expected = Err(
        "can't find the ZCode desktop app (not in the registry or %LOCALAPPDATA%\\Programs\\ZCode)"
            .to_owned(),
    );
    assert_eq!(
        run_open(Some(".."), Platform::Windows, &local_app_data, &fake).await,
        expected
    );
    assert_eq!(
        run_open(Some(".."), Platform::Windows, &no_env, &fake).await,
        expected
    );
    // Preview 的位置在它自己的 GUID 下；正式版登记过也不用。
    let fake = windows(
        &[(Hive::CurrentUser, "C:\\ZCode")],
        &["C:\\ZCode\\ZCode.exe"],
    );
    let preview = |name: &str| test_env(name).or_else(|| local_app_data(name));
    assert_eq!(
        run_open(Some(".."), Platform::Windows, &preview, &fake).await,
        Err(
            "can't find the ZCode Preview desktop app (not in the registry or %LOCALAPPDATA%\\Programs\\ZCode Preview)"
                .to_owned()
        )
    );
}
