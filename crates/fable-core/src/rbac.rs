//! Authorization as pure functions (spec §14 A-4, A-5; §5 S-7).
//!
//! Two layers:
//!
//! 1. **Global roles** ([`GlobalRole`]) on the [`Principal`]: `admin` is the
//!    implicit owner of every collection and may use every connection;
//!    `user` may create collections and use what it is granted.
//! 2. **Collection ACLs** ([`AclEntry`], rows of `collection_principals`)
//!    grant an Entra user or group a [`CollectionRole`]. A `public`
//!    collection gives every authenticated caller an implicit `viewer`.
//!
//! API-key principals (A-3, A-5) never carry admin powers, see only the
//! collections in their scopes, get at most `viewer` (keys are search-only)
//! and cannot create collections or use connections.
//!
//! Connections carry [`UsableBy`] (S-7), checked by [`can_use_connection`].
//!
//! Nothing here touches the database: callers load the ACL rows and pass
//! them in. [`RbacError`] distinguishes "no access" (HTTP 404, existence
//! not leaked) from "insufficient role" (HTTP 403).

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::principal::{AuthMethod, Principal};

/// A role on one collection, ordered `Viewer < Editor < Owner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionRole {
    /// Search and read.
    Viewer,
    /// Manage sources and uploads, trigger syncs, indexing settings.
    Editor,
    /// Manage the ACL, plus everything an editor can do.
    Owner,
}

impl CollectionRole {
    /// All roles, lowest first.
    pub const ALL: [CollectionRole; 3] = [
        CollectionRole::Viewer,
        CollectionRole::Editor,
        CollectionRole::Owner,
    ];

    /// Stable lowercase name, as stored in `collection_principals.role`.
    pub fn as_str(self) -> &'static str {
        match self {
            CollectionRole::Viewer => "viewer",
            CollectionRole::Editor => "editor",
            CollectionRole::Owner => "owner",
        }
    }
}

impl fmt::Display for CollectionRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error for an unknown enum value read from the database or a request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {what} {value:?}; expected one of: {expected}")]
pub struct ParseRbacEnumError {
    what: &'static str,
    value: String,
    expected: &'static str,
}

impl FromStr for CollectionRole {
    type Err = ParseRbacEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        CollectionRole::ALL
            .into_iter()
            .find(|r| r.as_str() == s)
            .ok_or_else(|| ParseRbacEnumError {
                what: "collection role",
                value: s.to_owned(),
                expected: "owner, editor, viewer",
            })
    }
}

/// Who an ACL entry names (`collection_principals.principal_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalType {
    /// An Entra user (or service principal) object id.
    User,
    /// An Entra group object id.
    Group,
}

impl PrincipalType {
    /// Stable lowercase name, as stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            PrincipalType::User => "user",
            PrincipalType::Group => "group",
        }
    }
}

impl fmt::Display for PrincipalType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PrincipalType {
    type Err = ParseRbacEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(PrincipalType::User),
            "group" => Ok(PrincipalType::Group),
            _ => Err(ParseRbacEnumError {
                what: "principal type",
                value: s.to_owned(),
                expected: "user, group",
            }),
        }
    }
}

/// Collection visibility (`collections.visibility`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// ACL only.
    #[default]
    Private,
    /// Every authenticated caller gets an implicit `viewer`.
    Public,
}

impl Visibility {
    /// Stable lowercase name, as stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Private => "private",
            Visibility::Public => "public",
        }
    }
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Visibility {
    type Err = ParseRbacEnumError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "private" => Ok(Visibility::Private),
            "public" => Ok(Visibility::Public),
            _ => Err(ParseRbacEnumError {
                what: "visibility",
                value: s.to_owned(),
                expected: "private, public",
            }),
        }
    }
}

/// One `collection_principals` row.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AclEntry {
    /// User or group.
    pub principal_type: PrincipalType,
    /// Entra object id of the user or group.
    pub principal_id: String,
    /// Granted role.
    pub role: CollectionRole,
}

impl AclEntry {
    /// Whether this entry names `principal` directly or via one of its groups.
    pub fn applies_to(&self, principal: &Principal) -> bool {
        match self.principal_type {
            PrincipalType::User => self.principal_id == principal.oid,
            PrincipalType::Group => principal.group_ids.contains(&self.principal_id),
        }
    }
}

/// What the RBAC functions need to know about one collection.
#[derive(Debug, Clone, Copy)]
pub struct CollectionAccess<'a> {
    /// Collection id (checked against API-key scopes).
    pub id: Uuid,
    /// Visibility.
    pub visibility: Visibility,
    /// The collection's ACL rows.
    pub acl: &'a [AclEntry],
}

/// Why an RBAC check failed. Maps to the §13 failure shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RbacError {
    /// The caller has no access at all: respond 404 so existence is not leaked.
    #[error("not found")]
    NoAccess,
    /// The caller can see the resource but lacks the role: respond 403.
    #[error("requires collection role {required}, caller has {actual}")]
    InsufficientRole {
        /// Role the operation needs.
        required: CollectionRole,
        /// Role the caller has.
        actual: CollectionRole,
    },
}

/// The caller's highest role on a collection, or `None` without access.
///
/// SSO callers: admins are implicit owners; otherwise the highest of the
/// direct user grant, group grants and (for public collections) `viewer`.
///
/// API-key callers (A-5): the collection must be in the key's scopes, the
/// admin bypass does not apply, and the result is capped at `viewer`.
/// Keys carry no groups, so only the owner's direct grants (and public
/// visibility) count.
pub fn effective_collection_role(
    principal: &Principal,
    collection: &CollectionAccess<'_>,
) -> Option<CollectionRole> {
    let key_scopes = match &principal.auth {
        AuthMethod::Sso => None,
        AuthMethod::ApiKey { scopes, .. } => Some(scopes),
    };
    if let Some(scopes) = key_scopes {
        if !scopes.contains(&collection.id) {
            return None;
        }
    } else if principal.is_admin() {
        return Some(CollectionRole::Owner);
    }

    let granted = collection
        .acl
        .iter()
        .filter(|e| e.applies_to(principal))
        .map(|e| e.role)
        .max();
    let public = (collection.visibility == Visibility::Public).then_some(CollectionRole::Viewer);
    let role = granted.max(public)?;

    Some(if key_scopes.is_some() {
        role.min(CollectionRole::Viewer)
    } else {
        role
    })
}

/// Require at least `required` on a collection; returns the caller's role.
///
/// No access → [`RbacError::NoAccess`] (404); some access but below
/// `required` → [`RbacError::InsufficientRole`] (403).
pub fn require_collection_role(
    principal: &Principal,
    collection: &CollectionAccess<'_>,
    required: CollectionRole,
) -> Result<CollectionRole, RbacError> {
    let actual = effective_collection_role(principal, collection).ok_or(RbacError::NoAccess)?;
    if actual >= required {
        Ok(actual)
    } else {
        Err(RbacError::InsufficientRole { required, actual })
    }
}

/// Whether the caller may create collections: any interactively signed-in
/// caller with a global role. API keys never can (A-3).
pub fn can_create_collections(principal: &Principal) -> bool {
    principal.is_sso() && !principal.global_roles.is_empty()
}

/// Who may use a connection (S-7), stored as `connections.usable_by` plus
/// `connections.usable_by_groups`.
///
/// JSON shape: `{"kind": "admins"}`, `{"kind": "all_users"}` or
/// `{"kind": "groups", "group_ids": ["…"]}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", try_from = "UsableByRaw")]
pub enum UsableBy {
    /// Global admins only (the default).
    #[default]
    Admins,
    /// Every caller with a global role.
    AllUsers,
    /// Members of any of these Entra groups (and admins). Never empty.
    Groups {
        /// Entra group object ids.
        group_ids: BTreeSet<String>,
    },
}

/// Unvalidated wire form of [`UsableBy`].
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum UsableByRaw {
    Admins,
    AllUsers,
    Groups { group_ids: Vec<String> },
}

impl TryFrom<UsableByRaw> for UsableBy {
    type Error = UsableByError;

    fn try_from(raw: UsableByRaw) -> Result<Self, Self::Error> {
        match raw {
            UsableByRaw::Admins => Ok(UsableBy::Admins),
            UsableByRaw::AllUsers => Ok(UsableBy::AllUsers),
            UsableByRaw::Groups { group_ids } => UsableBy::groups(group_ids),
        }
    }
}

/// Error building a [`UsableBy`] from database columns or input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UsableByError {
    /// Unknown `usable_by` value.
    #[error("unknown usable_by {0:?}; expected one of: admins, all_users, groups")]
    UnknownKind(String),
    /// `groups` without any group id.
    #[error("usable_by groups needs at least one group id")]
    NoGroups,
    /// `admins` / `all_users` with group ids.
    #[error("usable_by {0} takes no group ids")]
    UnexpectedGroups(&'static str),
}

impl UsableBy {
    /// The `connections.usable_by` column value.
    pub fn kind_str(&self) -> &'static str {
        match self {
            UsableBy::Admins => "admins",
            UsableBy::AllUsers => "all_users",
            UsableBy::Groups { .. } => "groups",
        }
    }

    /// Group ids (empty unless [`UsableBy::Groups`]), for
    /// `connections.usable_by_groups`.
    pub fn group_ids(&self) -> impl Iterator<Item = &str> {
        let groups = match self {
            UsableBy::Groups { group_ids } => Some(group_ids),
            _ => None,
        };
        groups.into_iter().flatten().map(String::as_str)
    }

    /// A group restriction; blank ids are dropped and an empty set is an error.
    pub fn groups<I, S>(group_ids: I) -> Result<Self, UsableByError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let group_ids: BTreeSet<String> = group_ids
            .into_iter()
            .map(|g| g.as_ref().trim().to_owned())
            .filter(|g| !g.is_empty())
            .collect();
        if group_ids.is_empty() {
            Err(UsableByError::NoGroups)
        } else {
            Ok(UsableBy::Groups { group_ids })
        }
    }

    /// Build from the `usable_by` / `usable_by_groups` columns, enforcing
    /// the same consistency as the table's CHECK constraint.
    pub fn from_columns(kind: &str, group_ids: &[String]) -> Result<Self, UsableByError> {
        let simple = match kind {
            "admins" => UsableBy::Admins,
            "all_users" => UsableBy::AllUsers,
            "groups" => return UsableBy::groups(group_ids),
            other => return Err(UsableByError::UnknownKind(other.to_owned())),
        };
        if group_ids.is_empty() {
            Ok(simple)
        } else {
            Err(UsableByError::UnexpectedGroups(simple.kind_str()))
        }
    }
}

/// Whether the caller may use a connection, e.g. attach a source through it
/// or pick it in indexing settings (S-7).
///
/// Admins may use every connection; `all_users` admits any caller with a
/// global role; `groups` admits members of a listed group. API keys never
/// can (they cannot reach management routes).
pub fn can_use_connection(principal: &Principal, usable_by: &UsableBy) -> bool {
    if !principal.is_sso() || principal.global_roles.is_empty() {
        return false;
    }
    if principal.is_admin() {
        return true;
    }
    match usable_by {
        UsableBy::Admins => false,
        UsableBy::AllUsers => true,
        UsableBy::Groups { group_ids } => !group_ids.is_disjoint(&principal.group_ids),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::principal::GlobalRole;

    const OID: &str = "u1";
    const GROUP: &str = "g-eng";

    fn sso(roles: &[GlobalRole], groups: &[&str]) -> Principal {
        Principal {
            oid: OID.to_owned(),
            name: String::new(),
            email: String::new(),
            group_ids: groups.iter().map(|g| (*g).to_owned()).collect(),
            global_roles: roles.iter().copied().collect(),
            auth: AuthMethod::Sso,
        }
    }

    fn user() -> Principal {
        sso(&[GlobalRole::User], &[])
    }

    fn admin() -> Principal {
        sso(&[GlobalRole::Admin], &[])
    }

    fn key(roles: &[GlobalRole], scopes: &[Uuid]) -> Principal {
        Principal {
            auth: AuthMethod::ApiKey {
                key_id: Uuid::new_v4(),
                scopes: scopes.iter().copied().collect(),
            },
            ..sso(roles, &[])
        }
    }

    fn entry(t: PrincipalType, id: &str, role: CollectionRole) -> AclEntry {
        AclEntry {
            principal_type: t,
            principal_id: id.to_owned(),
            role,
        }
    }

    fn coll(id: Uuid, visibility: Visibility, acl: &[AclEntry]) -> CollectionAccess<'_> {
        CollectionAccess {
            id,
            visibility,
            acl,
        }
    }

    use CollectionRole::{Editor, Owner, Viewer};
    use PrincipalType::{Group, User};
    use Visibility::{Private, Public};

    #[test]
    fn collection_role_order_names_and_serde() {
        assert!(Viewer < Editor && Editor < Owner);
        for r in CollectionRole::ALL {
            assert_eq!(r.as_str().parse::<CollectionRole>().unwrap(), r);
            assert_eq!(r.to_string(), r.as_str());
            assert_eq!(
                serde_json::to_string(&r).unwrap(),
                format!("\"{}\"", r.as_str())
            );
        }
        assert!("admin".parse::<CollectionRole>().is_err());
        assert!("Owner".parse::<CollectionRole>().is_err());
        for t in [User, Group] {
            assert_eq!(t.as_str().parse::<PrincipalType>().unwrap(), t);
        }
        assert!("role".parse::<PrincipalType>().is_err());
        for v in [Private, Public] {
            assert_eq!(v.as_str().parse::<Visibility>().unwrap(), v);
        }
        assert!("internal".parse::<Visibility>().is_err());
        assert_eq!(Visibility::default(), Private);
    }

    /// Every grant (none / user / group) × visibility for a regular user.
    #[test]
    fn effective_role_matrix_for_users() {
        let id = Uuid::new_v4();
        for vis in [Private, Public] {
            let floor = (vis == Public).then_some(Viewer);
            assert_eq!(
                effective_collection_role(&user(), &coll(id, vis, &[])),
                floor
            );
            for role in CollectionRole::ALL {
                let direct = [entry(User, OID, role)];
                assert_eq!(
                    effective_collection_role(&user(), &coll(id, vis, &direct)),
                    Some(role),
                    "{vis} direct {role}"
                );
                let via_group = [entry(Group, GROUP, role)];
                let member = sso(&[GlobalRole::User], &[GROUP]);
                assert_eq!(
                    effective_collection_role(&member, &coll(id, vis, &via_group)),
                    Some(role),
                    "{vis} group {role}"
                );
                // Grants to someone else, or a group the caller isn't in.
                let other = [entry(User, "u2", role), entry(Group, "g-other", role)];
                assert_eq!(
                    effective_collection_role(&member, &coll(id, vis, &other)),
                    floor
                );
            }
        }
    }

    #[test]
    fn user_and_group_ids_are_not_interchangeable() {
        let id = Uuid::new_v4();
        // A group entry whose id equals the caller's oid doesn't match, and
        // a user entry whose id equals one of the caller's groups doesn't either.
        let acl = [entry(Group, OID, Owner), entry(User, GROUP, Owner)];
        let member = sso(&[GlobalRole::User], &[GROUP]);
        assert_eq!(
            effective_collection_role(&member, &coll(id, Private, &acl)),
            None
        );
    }

    #[test]
    fn highest_grant_wins() {
        let id = Uuid::new_v4();
        let member = sso(&[GlobalRole::User], &[GROUP, "g-2"]);
        let acl = [
            entry(Group, GROUP, Viewer),
            entry(User, OID, Editor),
            entry(Group, "g-2", Owner),
        ];
        assert_eq!(
            effective_collection_role(&member, &coll(id, Public, &acl)),
            Some(Owner)
        );
        let acl = [entry(Group, GROUP, Editor), entry(User, OID, Viewer)];
        assert_eq!(
            effective_collection_role(&member, &coll(id, Private, &acl)),
            Some(Editor)
        );
    }

    #[test]
    fn admin_is_implicit_owner_everywhere() {
        let id = Uuid::new_v4();
        for vis in [Private, Public] {
            assert_eq!(
                effective_collection_role(&admin(), &coll(id, vis, &[])),
                Some(Owner)
            );
            let acl = [entry(User, OID, Viewer)];
            assert_eq!(
                effective_collection_role(&admin(), &coll(id, vis, &acl)),
                Some(Owner)
            );
        }
    }

    #[test]
    fn caller_without_global_role_still_gets_explicit_grants() {
        // Global-role gating happens at authentication; the ACL is the ACL.
        let id = Uuid::new_v4();
        let nobody = sso(&[], &[]);
        let acl = [entry(User, OID, Editor)];
        assert_eq!(
            effective_collection_role(&nobody, &coll(id, Private, &acl)),
            Some(Editor)
        );
    }

    #[test]
    fn api_keys_are_scoped_capped_and_never_admin() {
        let id = Uuid::new_v4();
        let other = Uuid::new_v4();
        let acl = [entry(User, OID, Owner)];

        // In scope: owner's direct grant counts but is capped at viewer.
        let k = key(&[GlobalRole::User], &[id]);
        assert_eq!(
            effective_collection_role(&k, &coll(id, Private, &acl)),
            Some(Viewer)
        );
        // Out of scope: no access even with a grant or public visibility.
        let k = key(&[GlobalRole::User], &[other]);
        assert_eq!(
            effective_collection_role(&k, &coll(id, Private, &acl)),
            None
        );
        assert_eq!(effective_collection_role(&k, &coll(id, Public, &[])), None);
        // In scope but the owner has lost access: none.
        let k = key(&[GlobalRole::User], &[id]);
        assert_eq!(effective_collection_role(&k, &coll(id, Private, &[])), None);
        // Public collections in scope stay readable.
        assert_eq!(
            effective_collection_role(&k, &coll(id, Public, &[])),
            Some(Viewer)
        );
        // Admin bypass never applies to keys.
        let k = key(&[GlobalRole::Admin], &[id]);
        assert_eq!(effective_collection_role(&k, &coll(id, Private, &[])), None);
        // Empty scopes: nothing.
        let k = key(&[GlobalRole::User], &[]);
        assert_eq!(effective_collection_role(&k, &coll(id, Public, &acl)), None);
    }

    /// Full required × granted matrix, plus no access.
    #[test]
    fn require_role_matrix() {
        let id = Uuid::new_v4();
        for granted in CollectionRole::ALL {
            let acl = [entry(User, OID, granted)];
            for required in CollectionRole::ALL {
                let got = require_collection_role(&user(), &coll(id, Private, &acl), required);
                if granted >= required {
                    assert_eq!(got, Ok(granted));
                } else {
                    assert_eq!(
                        got,
                        Err(RbacError::InsufficientRole {
                            required,
                            actual: granted
                        })
                    );
                }
            }
        }
        for required in CollectionRole::ALL {
            assert_eq!(
                require_collection_role(&user(), &coll(id, Private, &[]), required),
                Err(RbacError::NoAccess)
            );
            assert_eq!(
                require_collection_role(&admin(), &coll(id, Private, &[]), required),
                Ok(Owner)
            );
        }
    }

    #[test]
    fn public_visibility_caps_at_viewer_and_returns_403_above_it() {
        let id = Uuid::new_v4();
        let c = coll(id, Public, &[]);
        assert_eq!(require_collection_role(&user(), &c, Viewer), Ok(Viewer));
        assert_eq!(
            require_collection_role(&user(), &c, Editor),
            Err(RbacError::InsufficientRole {
                required: Editor,
                actual: Viewer
            })
        );
    }

    #[test]
    fn api_key_cannot_manage_collections() {
        let id = Uuid::new_v4();
        let acl = [entry(User, OID, Owner)];
        let k = key(&[GlobalRole::User], &[id]);
        assert_eq!(
            require_collection_role(&k, &coll(id, Private, &acl), Viewer),
            Ok(Viewer)
        );
        assert!(matches!(
            require_collection_role(&k, &coll(id, Private, &acl), Editor),
            Err(RbacError::InsufficientRole { .. })
        ));
    }

    #[test]
    fn collection_creation_rights() {
        assert!(can_create_collections(&user()));
        assert!(can_create_collections(&admin()));
        assert!(can_create_collections(&Principal::dev()));
        assert!(!can_create_collections(&sso(&[], &[GROUP])));
        assert!(!can_create_collections(&key(&[GlobalRole::User], &[])));
        assert!(!can_create_collections(&key(&[GlobalRole::Admin], &[])));
    }

    #[test]
    fn can_use_connection_matrix() {
        let groups = UsableBy::groups([GROUP, "g-hr"]).unwrap();
        let cases: [(&str, Principal, [bool; 3]); 7] = [
            // (caller, principal, [admins, all_users, groups])
            ("admin", admin(), [true, true, true]),
            ("dev admin", Principal::dev(), [true, true, true]),
            ("user", user(), [false, true, false]),
            (
                "user in group",
                sso(&[GlobalRole::User], &[GROUP]),
                [false, true, true],
            ),
            (
                "user in other group",
                sso(&[GlobalRole::User], &["g-other"]),
                [false, true, false],
            ),
            ("no global role, in group", sso(&[], &[GROUP]), [false; 3]),
            (
                "api key of admin",
                key(&[GlobalRole::Admin], &[]),
                [false; 3],
            ),
        ];
        for (who, p, expected) in cases {
            for (usable_by, want) in [UsableBy::Admins, UsableBy::AllUsers, groups.clone()]
                .iter()
                .zip(expected)
            {
                assert_eq!(
                    can_use_connection(&p, usable_by),
                    want,
                    "{who} / {}",
                    usable_by.kind_str()
                );
            }
        }
        assert!(!can_use_connection(
            &key(&[GlobalRole::User], &[]),
            &UsableBy::AllUsers
        ));
    }

    #[test]
    fn usable_by_columns_round_trip() {
        for u in [
            UsableBy::Admins,
            UsableBy::AllUsers,
            UsableBy::groups(["b", "a"]).unwrap(),
        ] {
            let groups: Vec<String> = u.group_ids().map(str::to_owned).collect();
            assert_eq!(UsableBy::from_columns(u.kind_str(), &groups), Ok(u));
        }
        assert_eq!(UsableBy::default(), UsableBy::Admins);
        assert_eq!(
            UsableBy::from_columns("everyone", &[]),
            Err(UsableByError::UnknownKind("everyone".into()))
        );
        assert_eq!(
            UsableBy::from_columns("groups", &[]),
            Err(UsableByError::NoGroups)
        );
        assert_eq!(
            UsableBy::from_columns("admins", &["g".into()]),
            Err(UsableByError::UnexpectedGroups("admins"))
        );
        assert_eq!(UsableBy::groups([" ", ""]), Err(UsableByError::NoGroups));
        assert_eq!(
            UsableBy::groups([" g1 ", "g1"])
                .unwrap()
                .group_ids()
                .collect::<Vec<_>>(),
            ["g1"]
        );
    }

    #[test]
    fn usable_by_json_shape() {
        assert_eq!(
            serde_json::to_value(UsableBy::Admins).unwrap(),
            serde_json::json!({"kind": "admins"})
        );
        assert_eq!(
            serde_json::to_value(UsableBy::AllUsers).unwrap(),
            serde_json::json!({"kind": "all_users"})
        );
        let g: UsableBy =
            serde_json::from_value(serde_json::json!({"kind": "groups", "group_ids": ["x"]}))
                .unwrap();
        assert_eq!(g, UsableBy::groups(["x"]).unwrap());
        assert!(serde_json::from_value::<UsableBy>(
            serde_json::json!({"kind": "groups", "group_ids": [" "]})
        )
        .is_err());
        assert!(serde_json::from_value::<UsableBy>(serde_json::json!({"kind": "nobody"})).is_err());
    }

    #[test]
    fn rbac_errors_display_without_leaking() {
        assert_eq!(RbacError::NoAccess.to_string(), "not found");
        assert_eq!(
            RbacError::InsufficientRole {
                required: Owner,
                actual: Editor
            }
            .to_string(),
            "requires collection role owner, caller has editor"
        );
    }
}
