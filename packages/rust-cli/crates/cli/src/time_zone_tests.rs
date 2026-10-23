use super::*;

#[test]
fn tz_values_name_the_zone() {
    assert_eq!(name("Asia/Shanghai"), "Asia/Shanghai");
    assert_eq!(name(":Asia/Shanghai"), "Asia/Shanghai");
    assert_eq!(name("/usr/share/zoneinfo/Europe/Berlin"), "Europe/Berlin");
    assert_eq!(name(":/var/db/timezone/zoneinfo/UTC"), "UTC");
    assert_eq!(name("CST-8"), "CST-8");
}

#[test]
fn utc_spellings_are_recognized() {
    for tz in ["UTC", ":UTC", "Etc/UTC", "GMT", "UTC0", "Etc/GMT-0", "Zulu"] {
        assert!(is_utc(tz), "{tz}");
    }
    for tz in ["Asia/Shanghai", "Etc/GMT-8", "CST-8", "EST5EDT"] {
        assert!(!is_utc(tz), "{tz}");
    }
}

#[test]
fn a_utc_tz_needs_no_system_lookup() {
    let zone = system(Some("Etc/UTC"));
    assert_eq!(zone.name(), "Etc/UTC");
    assert_eq!(zone.offset_minutes(1_760_000_000_000), 0);
}
