//! Ruling 0297: what a registered directory identity actually proves.
//!
//! The defect these checks exist for was executed, not theorised. In an
//! actual estate on ext4, through the frozen CLI: a Work's owned
//! execution directory was registered as `dev 64512, ino 30833`; `rm
//! -rf` followed by `mkdir` at the same address returned `ino 30833`
//! again, because the kernel hands a just-freed inode straight back to
//! the next create in the same parent. `(dev, ino)` matched, and `wirk
//! work clean` removed a directory this estate had not created, exit 0.
//!
//! These checks are on the comparison itself rather than on an
//! allocation: whether the inode number comes back is the filesystem's
//! business and no control may depend on it. The real-process
//! counterpart — a directory genuinely recreated at a Work's own
//! address, refused by the running daemon whether or not the number was
//! reused — is `wirk/tests/owned_deterministic_custody.rs`.

use std::fs;

use wirk_core::{DirectoryIdentity, IdentityProof, directory_identity, identity_proof};

fn identity(dev: u64, ino: u64, created: Option<u64>) -> DirectoryIdentity {
    DirectoryIdentity { dev, ino, created }
}

/// The same object: everything agrees.
#[test]
fn the_same_object_proves_itself() {
    let registered = identity(64512, 30833, Some(1_700_000_000_000_000_000));
    assert_eq!(
        identity_proof(&registered, &registered),
        IdentityProof::SameObject
    );
}

/// A different inode is a different object, and says so with both
/// pairs, whatever the creation times are.
#[test]
fn a_different_inode_is_a_different_object() {
    let registered = identity(64512, 30833, Some(1));
    let present = identity(64512, 30834, Some(1));
    let IdentityProof::DifferentObject { detail } = identity_proof(&registered, &present) else {
        panic!("a different inode number is a different object");
    };
    assert!(detail.contains("64512:30833"), "{detail}");
    assert!(detail.contains("64512:30834"), "{detail}");
}

/// **The observed defect.** The inode number came back; the object did
/// not. This is the case `(dev, ino)` alone reported as a match.
#[test]
fn a_reused_inode_with_a_later_birth_time_is_a_different_object() {
    let registered = identity(64512, 30833, Some(1_700_000_000_000_000_000));
    let recreated = identity(64512, 30833, Some(1_700_000_000_000_000_001));
    let IdentityProof::DifferentObject { detail } = identity_proof(&registered, &recreated) else {
        panic!("a recreated directory on a reused inode is not the registered object");
    };
    assert!(
        detail.contains("the same inode number") && detail.contains("reused"),
        "the refusal must say what it actually found: {detail}"
    );
}

/// A registration written before creation time was recorded cannot tell
/// a recreation from the original — and that is reported as such, never
/// as a match. Every caller treats this as unproven, execution and
/// reattachment included (ruling 0297; ruling 0300 closed the exception
/// reattachment used to hold for it).
#[test]
fn a_registration_without_a_creation_time_is_indistinguishable_not_matching() {
    let registered = identity(64512, 30833, None);
    let present = identity(64512, 30833, Some(1_700_000_000_000_000_000));
    let IdentityProof::Indistinguishable { reason } = identity_proof(&registered, &present) else {
        panic!("without a recorded creation time the two cannot be told apart");
    };
    assert!(
        reason.contains("before a creation time was recorded"),
        "{reason}"
    );
}

/// The platform limit in the other direction: the filesystem the
/// directory is on now reports no creation time. Honest, and not a
/// match.
#[test]
fn a_filesystem_that_reports_no_creation_time_is_indistinguishable_not_matching() {
    let registered = identity(64512, 30833, Some(1_700_000_000_000_000_000));
    let present = identity(64512, 30833, None);
    let IdentityProof::Indistinguishable { reason } = identity_proof(&registered, &present) else {
        panic!("a filesystem with no birth time cannot settle this either");
    };
    assert!(
        reason.contains("does not report a creation time"),
        "{reason}"
    );
}

/// What the running product actually reads off this host: two
/// directories created in the same parent, and a third that replaces
/// the first at its own address.
///
/// This asserts the *distinction*, not an allocation: it never requires
/// the inode number to be reused, and it is exactly as meaningful if it
/// is not.
#[test]
fn a_real_recreated_directory_is_never_the_registered_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    let address = dir.path().join("owned");
    fs::create_dir(&address).expect("create the directory this estate would own");
    let registered = directory_identity(&address).expect("the created directory");

    fs::remove_dir_all(&address).expect("remove it, as an operator would");
    fs::create_dir(&address).expect("recreate a different directory at the same address");
    let present = directory_identity(&address).expect("the recreated directory");

    match identity_proof(&registered, &present) {
        IdentityProof::DifferentObject { .. } => {}
        IdentityProof::Indistinguishable { reason } => {
            // Only reachable where this filesystem records no birth
            // time. Then the product refuses removal for that reason,
            // which is the honest outcome — never a silent match.
            assert!(
                registered.created.is_none() || present.created.is_none(),
                "indistinguishable was reported while both sides carry a creation time: {reason}"
            );
        }
        IdentityProof::SameObject => panic!(
            "a recreated directory was reported as the registered object: registered {:?}, \
             present {:?}",
            registered, present
        ),
    }
}

/// And the positive case on the same real filesystem: an untouched
/// directory keeps proving itself, so ordinary owned cleanup is not
/// collateral damage of the correction above.
#[test]
fn an_untouched_directory_keeps_proving_itself() {
    let dir = tempfile::tempdir().expect("tempdir");
    let address = dir.path().join("owned");
    fs::create_dir(&address).expect("create");
    let registered = directory_identity(&address).expect("the created directory");
    fs::write(address.join("report.md"), b"work happened here").expect("ordinary work in it");
    let present = directory_identity(&address).expect("the same directory");
    assert_eq!(
        identity_proof(&registered, &present),
        IdentityProof::SameObject,
        "ordinary work in a directory does not change which object it is"
    );
}
