use super::*;

/// 接上之前读到 None；只接一次，第二次交回新值、留着第一个。
#[test]
fn a_late_value_binds_once() {
    let late = Late::<u32>::default();
    assert_eq!(late.get(), None);
    assert_eq!(late.bind(1), Ok(()));
    assert_eq!(late.bind(2), Err(2));
    assert_eq!(late.get(), Some(&1));
}
