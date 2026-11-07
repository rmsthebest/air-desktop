//! Groups: create, invite, remove, rename, leave and delete, and the members
//! of an open group. Port of upstream's group operations on `UserCubit` and
//! `ChatDetailsCubit`, and of `MemberDetailsCubit` (members, and who the room
//! policy lets the user remove).
//!
//! Operations run detached on tokio, as contact operations do (see
//! `contacts::on_tokio`): the core's jobs clean up after themselves, so
//! closing a view mustn't stop them midway.

use aircommon::identifiers::UserId as CoreUserId;
use aircoreclient::clients::CoreUser;
use aircoreclient::db::notification::DbEntityId;
use aircoreclient::{ChatId as CoreChatId, ChatStatus, ChatType};
use gpui::SharedString;
use gpui_kit::{App, Context, Entity, Subscription, Task};
use mimi_room_policy::{MimiProposal, RoleIndex, VerifiedRoomState};
use tls_codec::Serialize as _;
use tracing::error;

use std::sync::Arc;

use super::avatars::{self, Avatars};
use super::chats::model_id;
use super::contacts::{Failure, on_tokio};
use super::messages::core_chat_id;
use super::{Session, SessionEvent};
use crate::command_line::{NameMatch, matches_name, normalize};
use crate::model::{ChatId, Member, UserId};
use crate::runtime::Tokio;

/// Longest group title accepted, in characters. Titles are shown as one
/// bounded line anyway; this keeps what is sent to the group modest.
const MAX_TITLE: usize = 128;

/// Fewest hex digits of a member's tag. Tags grow until they are unique in
/// the group: user ids are chosen by clients, so a member could pick one
/// whose short hash matches another member's.
const TAG_LEN: usize = 8;

impl Session {
    /// Creates a group with only the user in it. Resolves to the new chat.
    pub fn create_group(
        &self,
        title: &str,
        cx: &App,
    ) -> impl Future<Output = Result<ChatId, String>> + use<> {
        let (user, title) = (self.user.clone(), title.to_string());
        on_tokio(cx, "create the group", async move {
            Ok(model_id(&create(&user, &title).await?))
        })
    }

    /// Invites the contact `query` names (see [`matches_name`]) to `chat`.
    /// Resolves to the contact's name.
    pub fn invite(
        &self,
        chat: &ChatId,
        query: &str,
        cx: &App,
    ) -> impl Future<Output = Result<SharedString, String>> + use<> {
        let (user, chat, query) = (self.user.clone(), core_chat_id(chat), query.to_string());
        on_tokio(cx, "invite to the group", invite(user, chat, query))
    }

    pub fn rename_group(
        &self,
        chat: &ChatId,
        title: &str,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let (user, chat, title) = (self.user.clone(), core_chat_id(chat), title.to_string());
        on_tokio(cx, "rename the group", async move {
            rename(&user, chat, &title).await
        })
    }

    /// Sets the group's picture from the file at `path` (`None` removes
    /// it), for every member. The core shrinks it to a small JPEG.
    pub fn set_group_picture(
        &self,
        chat: &ChatId,
        path: Option<std::path::PathBuf>,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let (user, chat) = (self.user.clone(), core_chat_id(chat));
        let what = if path.is_some() {
            "change the group's picture"
        } else {
            "remove the group's picture"
        };
        on_tokio(
            cx,
            what,
            async move { set_picture(&user, chat, path).await },
        )
    }

    /// Leaves the group; it stays in the list, ended, until deleted.
    pub fn leave_group(
        &self,
        chat: &ChatId,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let (user, chat) = (self.user.clone(), core_chat_id(chat));
        on_tokio(
            cx,
            "leave the group",
            async move { leave(&user, chat).await },
        )
    }

    /// Deletes the group: ends it for every member (the core removes them
    /// all), then erases it from all of the user's devices.
    pub fn delete_group(
        &self,
        chat: &ChatId,
        cx: &App,
    ) -> impl Future<Output = Result<(), String>> + use<> {
        let (user, chat) = (self.user.clone(), core_chat_id(chat));
        on_tokio(
            cx,
            "delete the group",
            async move { delete(&user, chat).await },
        )
    }
}

pub(super) async fn create(user: &CoreUser, title: &str) -> Result<CoreChatId, Failure> {
    let title = parse_title(title)?;
    Ok(user.create_chat(title, None, false).await?)
}

pub(super) async fn rename(user: &CoreUser, chat: CoreChatId, title: &str) -> Result<(), Failure> {
    active_group(user, chat).await?;
    Ok(user.set_chat_title(chat, parse_title(title)?).await?)
}

/// See [`Session::set_group_picture`].
pub(super) async fn set_picture(
    user: &CoreUser,
    chat: CoreChatId,
    path: Option<std::path::PathBuf>,
) -> Result<(), Failure> {
    active_group(user, chat).await?;
    let picture = match path {
        // Checked first: the core drops a picture it can't shrink, which
        // removes the group's current one.
        Some(path) => Some(avatars::read_source(path).await?),
        None => None,
    };
    Ok(user.set_chat_picture(chat, picture).await?)
}

/// See [`Members::add_contact`].
pub(super) async fn add_member_contact(
    user: &CoreUser,
    chat: CoreChatId,
    picked: Picked,
) -> Result<ChatId, Failure> {
    active_group(user, chat).await?;
    let name = &picked.name;
    // The core refuses these too, but only as internal errors.
    if user.contact(&picked.id).await.is_some() {
        return Err(Failure::Shown(format!("{name} is already a contact")));
    }
    for id in user.ordered_chat_ids().await? {
        if let Some(found) = user.chat(&id).await
            // In any status: the core refuses while its record exists.
            && found.chat_type == ChatType::TargetedMessageConnection(picked.id.clone())
        {
            return Err(Failure::Shown(format!(
                "You already sent {name} a contact request"
            )));
        }
    }
    // Upstream's post-quantum choice is an experimental switch, off.
    let opened = user.add_contact_from_group(chat, picked.id, false).await?;
    Ok(model_id(&opened))
}

pub(super) async fn leave(user: &CoreUser, chat: CoreChatId) -> Result<(), Failure> {
    active_group(user, chat).await?;
    Ok(user.leave_chat(chat).await?)
}

pub(super) async fn delete(user: &CoreUser, chat: CoreChatId) -> Result<(), Failure> {
    group(user, chat).await?;
    Ok(user.delete_and_erase_chat(chat).await?)
}

/// The chat, if it is a group. The self chat is one in the core (of the
/// user's devices), but never managed as one: leaving or inviting to it
/// would break device sync.
async fn group(user: &CoreUser, chat: CoreChatId) -> Result<aircoreclient::Chat, Failure> {
    let Some(group) = user.chat(&chat).await else {
        return Err(Failure::Shown("The chat is gone".into()));
    };
    let is_self = user
        .chat_is_self_group(&group.group_id)
        .await
        .map_err(Failure::Internal)?;
    if !matches!(group.chat_type, ChatType::Group(_)) || is_self {
        return Err(Failure::Shown("That works in groups only".into()));
    }
    Ok(group)
}

/// The group, if the user is still in it: an ended group can only be
/// deleted.
async fn active_group(user: &CoreUser, chat: CoreChatId) -> Result<aircoreclient::Chat, Failure> {
    let group = group(user, chat).await?;
    if matches!(group.status(), ChatStatus::Inactive(_)) {
        return Err(Failure::Shown("You're no longer in this group".into()));
    }
    Ok(group)
}

fn parse_title(title: &str) -> Result<String, Failure> {
    let title = title.trim();
    if title.is_empty() {
        return Err(Failure::Shown("Groups need a name".into()));
    }
    if title.chars().count() > MAX_TITLE {
        return Err(Failure::Shown(format!(
            "Group names have at most {MAX_TITLE} characters"
        )));
    }
    Ok(title.to_string())
}

pub(super) async fn invite(
    user: CoreUser,
    chat: CoreChatId,
    query: String,
) -> Result<SharedString, Failure> {
    active_group(&user, chat).await?;
    let contacts = user
        .contacts_with_supported_features()
        .await
        .map_err(anyhow::Error::from)?;
    let mut named = Vec::with_capacity(contacts.len());
    for contact in contacts {
        let name = super::display_name(&user, &contact.user_id).await;
        named.push((name, contact));
    }
    // Names are peer-chosen and may imitate each other: never guess.
    let (name, contact) = match matches_name(&query, named.iter().map(|(name, _)| name.as_ref())) {
        NameMatch::One(name) => named
            .iter()
            .find(|(named, _)| named.as_ref() == name)
            .ok_or_else(|| Failure::Internal(anyhow::anyhow!("matched name vanished")))?,
        NameMatch::None => {
            return Err(Failure::Shown(format!("No contact matches {query:?}")));
        }
        NameMatch::Ambiguous(n) => {
            return Err(Failure::Shown(format!(
                "{n} contacts match {query:?}; be more specific"
            )));
        }
    };
    if user
        .chat_participants(chat)
        .await
        .is_some_and(|members| members.contains(&contact.user_id))
    {
        return Err(Failure::Shown(format!("{name} is already in the group")));
    }
    // As upstream: a contact whose features aren't known yet, or whose app
    // can't take part in groups, isn't invited.
    let supported = contact
        .supported_features
        .as_ref()
        .is_some_and(|features| features.encrypted_group_profiles);
    if !supported {
        return Err(Failure::Shown(format!(
            "{name}'s app can't join groups yet"
        )));
    }
    let result = user
        .invite_users(chat, std::slice::from_ref(&contact.user_id))
        .await?;
    if !result.users_not_added.is_empty() {
        return Err(Failure::Shown(format!(
            "{name}'s app can't join this group"
        )));
    }
    Ok(name.clone())
}

/// A member a command resolved (`:kick`, `:contact-member`), acted on
/// once the user confirms.
pub struct Picked {
    pub name: SharedString,
    pub tag: SharedString,
    pub(super) id: CoreUserId,
}

/// The members of one group, kept up to date.
pub struct Members {
    user: CoreUser,
    chat: CoreChatId,
    members: Vec<Member>,
    /// The core's id of each member, in the same order.
    core_ids: Vec<CoreUserId>,
    loaded: bool,
    /// The last load failed: what is shown may be stale.
    failed: bool,
    /// The latest load; a newer one replaces (cancels) it.
    load: Option<Task<()>>,
    /// The members' pictures, kept across loads.
    avatars: Arc<tokio::sync::Mutex<Avatars<CoreUserId>>>,
    _changes: Subscription,
}

impl Members {
    pub fn open(session: &Entity<Session>, chat: &ChatId, cx: &mut Context<Self>) -> Self {
        let chat = core_chat_id(chat);
        // As upstream: the chat (membership, roles) or any user (names).
        let changes = cx.subscribe(session, move |this: &mut Self, _, event, cx| match event {
            SessionEvent::Changed {
                notifications,
                overflowed,
            } => {
                // Membership changes come with the chat; names with users,
                // of which only members matter.
                let touched = notifications.iter().any(|notification| {
                    notification.ops.keys().any(|entity| match entity {
                        DbEntityId::Chat(id) => *id == chat,
                        DbEntityId::User(user) => this.core_ids.contains(user),
                        _ => false,
                    })
                });
                if *overflowed || touched {
                    this.reload(cx);
                }
            }
        });
        // Drawn pictures live on the GPU until dropped.
        cx.on_release(|this: &mut Self, cx| {
            for picture in this
                .members
                .iter()
                .filter_map(|member| member.picture.as_ref())
            {
                cx.drop_image(picture.0.clone(), None);
            }
        })
        .detach();
        let mut members = Self {
            user: session.read(cx).user().clone(),
            chat,
            members: Vec::new(),
            core_ids: Vec::new(),
            loaded: false,
            failed: false,
            load: None,
            avatars: Arc::default(),
            _changes: changes,
        };
        members.reload(cx);
        members
    }

    pub fn members(&self) -> &[Member] {
        &self.members
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Who `query` names, if the user may remove them (see
    /// [`resolve_removal`]): their name, tag and core id.
    pub fn resolve(&self, query: &str) -> Result<Picked, String> {
        let ix = self.index(query, resolve_removal)?;
        Ok(Picked {
            name: self.members[ix].name.clone(),
            tag: self.members[ix].tag.clone(),
            id: self.core_ids[ix].clone(),
        })
    }

    /// Who `query` names, other than the user (see [`find_member`]).
    pub fn pick(&self, query: &str) -> Result<Picked, String> {
        let ix = self.index(query, find_member)?;
        Ok(Picked {
            name: self.members[ix].name.clone(),
            tag: self.members[ix].tag.clone(),
            id: self.core_ids[ix].clone(),
        })
    }

    /// Sends `picked` a contact request through this group, as upstream's
    /// `add_contact_from_group`. Resolves to the chat it opens (theirs, if
    /// they had already asked the user).
    pub fn add_contact(
        &self,
        picked: Picked,
        cx: &App,
    ) -> impl Future<Output = Result<ChatId, String>> + use<> {
        let (user, chat) = (self.user.clone(), self.chat);
        on_tokio(cx, "send the contact request", async move {
            add_member_contact(&user, chat, picked).await
        })
    }

    /// The member other than the user that `query` names (see
    /// [`find_member`]): their name, tag and core id.
    pub(super) fn find(
        &self,
        query: &str,
    ) -> Result<(SharedString, SharedString, CoreUserId), String> {
        let ix = self.index(query, find_member)?;
        Ok((
            self.members[ix].name.clone(),
            self.members[ix].tag.clone(),
            self.core_ids[ix].clone(),
        ))
    }

    /// The member `query` names by `pick`, once the members are loaded.
    fn index(
        &self,
        query: &str,
        pick: fn(&[Member], &str) -> Result<usize, String>,
    ) -> Result<usize, String> {
        if self.failed {
            return Err("Couldn't load the members; reopen the group".into());
        }
        if !self.loaded {
            return Err("Still loading the members".into());
        }
        pick(&self.members, query)
    }

    pub(super) fn user(&self) -> &CoreUser {
        &self.user
    }

    /// What `:kick` completes (see [`removal_candidates`]).
    pub fn candidates(&self) -> Vec<(String, String)> {
        removal_candidates(&self.members)
    }

    /// Removes the member `resolve` found. Resolves to their name.
    pub fn remove(
        &self,
        removal: Picked,
        cx: &App,
    ) -> impl Future<Output = Result<SharedString, String>> + use<> {
        let (user, chat) = (self.user.clone(), self.chat);
        on_tokio(cx, "remove the member", async move {
            active_group(&user, chat).await?;
            user.remove_users(chat, vec![removal.id]).await?;
            Ok(removal.name)
        })
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let (user, chat, avatars) = (self.user.clone(), self.chat, self.avatars.clone());
        let load = Tokio::spawn(cx, async move {
            // One load at a time: a newer one waits for (and replaces) it.
            let mut avatars = avatars.lock().await;
            load(&user, chat, &mut avatars).await
        });
        self.load = Some(cx.spawn(async move |this, cx| {
            let loaded = load.await;
            _ = this.update(cx, |this, cx| {
                this.failed = loaded.is_err();
                match loaded {
                    Ok(Some(loaded)) => {
                        let old = std::mem::take(&mut this.members);
                        (this.members, this.core_ids) = loaded.into_iter().unzip();
                        avatars::drop_unused(
                            old.iter().filter_map(|member| member.picture.as_ref()),
                            this.members
                                .iter()
                                .filter_map(|member| member.picture.as_ref()),
                            cx,
                        );
                    }
                    // The chat is gone; the workspace closes it.
                    Ok(None) => {}
                    Err(err) => error!("failed to load group members: {err:#}"),
                }
                this.loaded = true;
                cx.notify();
            });
        }));
    }
}

/// The tag in `query`, if all of it is one: `#` and hex digits. Only then
/// is it read as a tag; anything else is matched against names, so a
/// (peer-chosen) name can't smuggle in another member's tag.
pub(super) fn as_tag(query: &str) -> Option<&str> {
    let tag = query.trim().strip_prefix('#')?;
    (!tag.is_empty() && tag.chars().all(|c| c.is_ascii_hexdigit())).then_some(tag)
}

/// Which member `query` names (their index), if the user may remove them:
/// by `#tag` (see [`as_tag`]) or else by name (see [`matches_name`]).
/// Never the user.
fn resolve_removal(members: &[Member], query: &str) -> Result<usize, String> {
    let ix = find_member(members, query)?;
    if !members[ix].can_remove {
        return Err(format!(
            "You can't remove {} from this group",
            members[ix].name
        ));
    }
    Ok(ix)
}

fn find_member(members: &[Member], query: &str) -> Result<usize, String> {
    let others: Vec<_> = (0..members.len()).filter(|&ix| !members[ix].own).collect();
    let ix = if let Some(tag) = as_tag(query) {
        let tag = tag.to_lowercase();
        let mut tagged = others
            .iter()
            .copied()
            .filter(|&ix| members[ix].tag.as_ref() == tag);
        match (tagged.next(), tagged.next()) {
            (Some(ix), None) => ix,
            (None, _) => return Err(format!("No member is tagged #{tag}")),
            (Some(_), Some(_)) => return Err(format!("Several members are tagged #{tag}")),
        }
    } else {
        match matches_name(query, others.iter().map(|&ix| members[ix].name.as_ref())) {
            NameMatch::One(name) => others
                .iter()
                .copied()
                .find(|&ix| members[ix].name.as_ref() == name)
                .ok_or_else(|| "That member is gone".to_string())?,
            NameMatch::None => return Err(format!("No member matches {query:?}")),
            NameMatch::Ambiguous(n) => {
                return Err(format!(
                    "{n} members match {query:?}: use the #tag the details pane shows"
                ));
            }
        }
    };
    Ok(ix)
}

/// What `:kick` completes, with the member's name as help: their name if
/// it finds them again, else their `#tag` (alike names, or a name that
/// reads as a tag). Never the user.
fn removal_candidates(members: &[Member]) -> Vec<(String, String)> {
    let mut alike: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for member in members.iter().filter(|member| !member.own) {
        *alike.entry(normalize(&member.name)).or_default() += 1;
    }
    members
        .iter()
        .filter(|member| !member.own)
        .map(|member| {
            let unique = alike[&normalize(&member.name)] == 1;
            let insert = if unique && as_tag(&member.name).is_none() {
                member.name.to_string()
            } else {
                format!("#{}", member.tag)
            };
            (insert, format!("{} #{}", member.name, member.tag))
        })
        .collect()
}

/// The group's members, the user first, then by name. `None` without a chat.
pub(super) async fn load(
    user: &CoreUser,
    chat: CoreChatId,
    avatars: &mut Avatars<CoreUserId>,
) -> Option<Vec<(Member, CoreUserId)>> {
    let active = user
        .chat(&chat)
        .await
        .is_some_and(|chat| !matches!(chat.status(), ChatStatus::Inactive(_)));
    let participants = user.chat_participants(chat).await?;
    let removing = user.pending_removes(chat).await.unwrap_or_default();
    let room = user
        .load_room_state(&chat)
        .await
        .inspect_err(|err| error!("failed to load a group's room state: {err:#}"))
        .ok();
    let mut members = Vec::with_capacity(participants.len());
    for id in participants {
        let own = &id == user.user_id();
        let profile = user.user_profile(&id).await;
        let picture = profile.profile_picture.map(|picture| picture.into_bytes());
        let member = Member {
            id: UserId(SharedString::default()),
            name: if own {
                "You".into()
            } else {
                crate::model::name_line(profile.display_name.as_ref())
            },
            picture: avatars.get(&id, picture).await,
            own,
            removing: removing.contains(&id),
            tag: SharedString::default(),
            can_remove: !own
                && active
                && room
                    .as_ref()
                    .is_some_and(|(us, state)| can_remove(us, state, &id)),
        };
        members.push((member, id));
    }
    avatars.retain(|id| members.iter().any(|(_, member)| member == id));
    identify(&mut members);
    members.sort_by(|(a, _), (b, _)| (!a.own, &a.name).cmp(&(!b.own, &b.name)));
    Some(members)
}

/// Gives each member their ids from the whole user id: the model id (its
/// hash) and the shortest tag, from [`TAG_LEN`] digits up, that no other
/// member's shares.
fn identify(members: &mut [(Member, CoreUserId)]) {
    let hashes: Vec<String> = members.iter().map(|(_, id)| id_hash(id)).collect();
    let len = tag_len(&hashes);
    for ((member, _), hash) in members.iter_mut().zip(hashes) {
        member.tag = hash[..len].to_string().into();
        member.id = UserId(hash.into());
    }
}

/// The fewest digits, from [`TAG_LEN`] up, that tell `hashes` apart.
fn tag_len(hashes: &[String]) -> usize {
    crate::model::unique_prefix_len(hashes, TAG_LEN)
}

/// Hex SHA-256 of the user id's wire form (uuid and domain).
fn id_hash(id: &CoreUserId) -> String {
    use sha2::Digest as _;
    let bytes = id.tls_serialize_detached().unwrap_or_default();
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether the room policy lets `us` make `target` an outsider: upstream's
/// `UiRoomState::can_kick`.
fn can_remove(us: &CoreUserId, state: &VerifiedRoomState, target: &CoreUserId) -> bool {
    let (Ok(us), Ok(target)) = (us.tls_serialize_detached(), target.tls_serialize_detached())
    else {
        return false;
    };
    state
        .can_apply_regular_proposals(
            &us,
            &[MimiProposal::ChangeRole {
                target,
                role: RoleIndex::Outsider,
            }],
        )
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str, tag: &str, own: bool, can_remove: bool) -> Member {
        Member {
            id: UserId(tag.to_string().into()),
            name: name.to_string().into(),
            own,
            removing: false,
            can_remove,
            tag: tag.to_string().into(),
            picture: None,
        }
    }

    #[test]
    fn a_copied_name_never_shields_a_member_from_removal() {
        let members = [
            member("You", "00000000", true, false),
            member("Grace", "1a2b3c4d", false, true),
            // Took Grace's name, so names alone can't tell them apart.
            member("Grace", "5e6f7a8b", false, true),
            member("Alan", "9c0d1e2f", false, false),
        ];
        let removed =
            |query| resolve_removal(&members, query).map(|ix| members[ix].tag.to_string());
        assert!(removed("grace").is_err(), "never guessed");
        assert_eq!(removed("#5E6F7A8B").as_deref(), Ok("5e6f7a8b"));
        // A tag is only ever the whole query.
        assert!(removed("Grace #1a2b3c4d").is_err());
        // Not the user, nor those the policy protects.
        assert!(removed("you").is_err());
        assert!(removed("#00000000").is_err());
        assert!(removed("alan").is_err());
        assert!(removed("#ffffffff").is_err());
        // Tags are unique when loaded; a duplicate is still never guessed.
        let mut twins = members.clone();
        twins[2].tag = twins[1].tag.clone();
        assert!(resolve_removal(&twins, "#1a2b3c4d").is_err());
    }

    #[test]
    fn a_safety_code_is_never_the_users_own() {
        // Whatever the policy says: the user's own entry is never found.
        let members = [
            member("Grace", "00000000", true, true),
            member("Grace", "1a2b3c4d", false, false),
        ];
        let found = |query| find_member(&members, query).map(|ix| members[ix].tag.to_string());
        // Sharing the user's name, the peer is still the one named.
        assert_eq!(found("grace").as_deref(), Ok("1a2b3c4d"));
        assert!(found("#00000000").is_err());
        // Picked permission plays no part in finding.
        assert_eq!(found("#1a2b3c4d").as_deref(), Ok("1a2b3c4d"));
    }

    #[test]
    fn members_sharing_a_uuid_stay_apart() {
        let uuid = uuid::Uuid::new_v4();
        let grace = CoreUserId::new(uuid, "example.com".parse().unwrap());
        let mallory = CoreUserId::new(uuid, "example.net".parse().unwrap());
        let mut members: Vec<_> = [&grace, &mallory]
            .into_iter()
            .map(|id| (member("Grace", "", false, true), id.clone()))
            .collect();
        identify(&mut members);
        let (rows, ids): (Vec<_>, Vec<_>) = members.into_iter().unzip();
        assert_ne!(rows[0].id, rows[1].id);
        let ix = resolve_removal(&rows, &format!("#{}", rows[1].tag)).unwrap();
        assert_eq!(ids[ix], mallory);
    }

    #[test]
    fn tags_grow_until_unique() {
        let hash = |prefix: &str| format!("{prefix:0<64}");
        assert_eq!(tag_len(&[hash("1a2b3c4d"), hash("5e6f7a8b")]), TAG_LEN);
        // Sharing ten digits, they need eleven.
        assert_eq!(tag_len(&[hash("1a2b3c4d5e1"), hash("1a2b3c4d5e2")]), 11);
    }

    #[test]
    fn every_completion_finds_its_member_whatever_the_names() {
        let grace = member("Grace", "1a2b3c4d", false, true);
        let members = [
            member("You", "00000000", true, false),
            grace.clone(),
            member("Grace", "5e6f7a8b", false, true),
            // Names chosen to point completion at Grace.
            member("Mallory #1a2b3c4d", "9c0d1e2f", false, true),
            member("#1a2b3c4d", "3f4e5d6c", false, true),
            member("Alan", "7b8a9f0e", false, true),
        ];
        let candidates = removal_candidates(&members);
        assert_eq!(candidates.len(), members.len() - 1, "never the user");
        for (insert, _) in &candidates {
            let ix = resolve_removal(&members, insert).unwrap();
            let expected = candidates.iter().position(|(i, _)| i == insert).unwrap() + 1;
            assert_eq!(ix, expected, "{insert:?} found someone else");
        }
        assert!(candidates.contains(&("Alan".into(), "Alan #7b8a9f0e".into())));
    }

    #[test]
    fn group_names_are_trimmed_and_bounded() {
        assert!(parse_title(" \t ").is_err());
        assert_eq!(parse_title(" Engines ").ok().as_deref(), Some("Engines"));
        let longest = "é".repeat(MAX_TITLE);
        assert!(parse_title(&longest).is_ok(), "characters, not bytes");
        assert!(parse_title(&format!("{longest}x")).is_err());
    }
}
