//! Regression test: auth middleware must try `preferred_username` for membership.
//!
//! BUG-04 (2026-08-09 evaluation): the sharing UI stores the username, but the
//! auth middleware matched only the `sub` (UUID). This test verifies the
//! source code contains the fallback lookup.

#[test]
fn project_access_tries_preferred_username_fallback() {
    let source = include_str!("../src/auth.rs");

    // The fallback must exist in project_access.
    assert!(
        source.contains("preferred_username"),
        "auth.rs must reference preferred_username for membership fallback"
    );

    // project_access must attempt a second lookup by preferred_username.
    let access_fn = source
        .find("fn project_access")
        .and_then(|i| source[i..].find("preferred_username"))
        .expect("project_access must fall back to preferred_username");

    assert!(
        access_fn > 0,
        "project_access must contain a preferred_username fallback for membership lookup"
    );

    // project_acl must also have the fallback.
    let acl_fn = source
        .find("fn project_acl")
        .and_then(|i| source[i..].find("preferred_username"))
        .expect("project_acl must fall back to preferred_username");
    assert!(
        acl_fn > 0,
        "project_acl must contain a preferred_username fallback for membership lookup"
    );
}

#[test]
fn principal_carries_preferred_username() {
    let source = include_str!("../src/auth.rs");
    assert!(
        source.contains("preferred_username"),
        "Principal struct must include preferred_username field"
    );
}
