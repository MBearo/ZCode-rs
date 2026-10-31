use super::*;

#[test]
fn dots_fold_lexically() {
    let fold = |text: &str| normalize(Path::new(text));
    assert_eq!(fold("/a/./b/../c"), PathBuf::from("/a/c"));
    assert_eq!(fold("/a/b/.."), PathBuf::from("/a"));
    assert_eq!(fold("/.."), PathBuf::from("/"));
    assert_eq!(fold("a/../../b"), PathBuf::from("b"));
    assert_eq!(
        fold("/w/repo/.git/worktrees/x/../.."),
        PathBuf::from("/w/repo/.git")
    );
}
