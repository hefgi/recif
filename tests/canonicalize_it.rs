//! Integration tests for the §6.4 canonicalization invariant, exercising real
//! filesystem symlinks (unit tests cover the pure-logic paths).

use std::path::PathBuf;

use recif::canonicalize::canonicalize_profile_path;
use recif::keychain::path_hash;

/// A symlinked *parent* directory must be resolved, while the literal final
/// component (the profile dir name) is preserved.
#[test]
fn symlinked_parent_resolved_final_name_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let real_parent = tmp.path().join("real_home");
    std::fs::create_dir(&real_parent).unwrap();

    // link -> real_home
    let link_parent = tmp.path().join("link_home");
    std::os::unix::fs::symlink(&real_parent, &link_parent).unwrap();

    // canonicalize link_home/.claude-x
    let input = link_parent.join(".claude-x");
    let got = canonicalize_profile_path(&input).unwrap();

    // Parent resolved to real_home (canonicalized), final name preserved.
    let expected = std::fs::canonicalize(&real_parent)
        .unwrap()
        .join(".claude-x");
    assert_eq!(got, expected);
}

/// The tilde-expanded and absolute forms of the same path must hash equally
/// after canonicalization (guards §11.1 Part C from the other direction: once
/// canonicalized, forms converge).
#[test]
fn canonical_forms_hash_equal() {
    let home = dirs::home_dir().unwrap();
    let via_tilde = canonicalize_profile_path(&PathBuf::from("~/.claude-hasheq")).unwrap();
    let via_abs = canonicalize_profile_path(&home.join(".claude-hasheq")).unwrap();
    assert_eq!(path_hash(&via_tilde), path_hash(&via_abs));
}

/// The load-bearing invariant (§6.4): when the FINAL component is itself a
/// symlink, its literal name is preserved (NOT resolved), because Claude hashes
/// the path string. A refactor that resolved the final component would misroute
/// every symlinked-profile credential slot — this test guards against that.
#[test]
fn symlinked_final_component_name_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let real_home = std::fs::canonicalize(tmp.path()).unwrap();

    // A real target the profile-name symlink points at.
    let target = real_home.join("real-target");
    std::fs::create_dir(&target).unwrap();

    // The profile dir itself is a symlink: .claude-linked -> real-target
    let linked = real_home.join(".claude-linked");
    std::os::unix::fs::symlink(&target, &linked).unwrap();

    let got = canonicalize_profile_path(&linked).unwrap();

    // Must keep the literal final name ".claude-linked", NOT resolve to
    // "real-target".
    assert_eq!(got, real_home.join(".claude-linked"));
    assert_ne!(got, target);
    // And the hash must be of the literal name, differing from the target's.
    assert_ne!(path_hash(&got), path_hash(&target));
}

/// `..` in the parent is collapsed via realpath.
#[test]
fn dotdot_in_parent_collapsed() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = a.join("b");
    std::fs::create_dir_all(&b).unwrap();

    let input = b.join("..").join(".claude-y");
    let got = canonicalize_profile_path(&input).unwrap();
    let expected = std::fs::canonicalize(&a).unwrap().join(".claude-y");
    assert_eq!(got, expected);
}
