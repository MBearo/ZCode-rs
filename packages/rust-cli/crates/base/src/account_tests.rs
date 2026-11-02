use super::*;

#[test]
fn families_round_trip_their_node_names() {
    for family in [Family::Zai, Family::Bigmodel] {
        assert_eq!(Family::parse(family.as_str()), Some(family));
    }
    assert_eq!(Family::parse("Zai"), None);
    assert_eq!(Family::Bigmodel.display_name(), "BigModel");
}
