use super::*;

#[test]
fn the_roles_subsume_the_previous() {
    // ATH-SEC-003: three roles, ordered; `admin` is exactly `operator` plus more.
    for (caller, required, satisfied) in [
        (Role::Viewer, Role::Viewer, true),
        (Role::Viewer, Role::Operator, false),
        (Role::Viewer, Role::Admin, false),
        (Role::Operator, Role::Viewer, true),
        (Role::Operator, Role::Operator, true),
        (Role::Operator, Role::Admin, false),
        (Role::Admin, Role::Viewer, true),
        (Role::Admin, Role::Operator, true),
        (Role::Admin, Role::Admin, true),
    ] {
        assert_eq!(
            caller.satisfies(required),
            satisfied,
            "{caller:?}/{required:?}"
        );
    }
}

#[test]
fn the_wire_forms_match_the_tables() {
    for (role, wire) in [
        (Role::Viewer, "viewer"),
        (Role::Operator, "operator"),
        (Role::Admin, "admin"),
    ] {
        assert_eq!(role.as_str(), wire);
        assert_eq!(serde_json::to_value(role).expect("serde"), wire);
    }
    assert_eq!(Role::all().len(), 3);
}
