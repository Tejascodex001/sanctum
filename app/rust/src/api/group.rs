//! Group chats. See `sanctum_core::group`.

use super::session::{self, poke};

pub struct GroupMemberView {
    pub key: Vec<u8>,
    pub name: String,
    /// The leader or an admin.
    pub admin: bool,
    /// The group's leader.
    pub leader: bool,
    pub me: bool,
    /// Invited, not joined yet.
    pub pending: bool,
}

pub struct GroupDetails {
    pub title: String,
    pub admin_is_me: bool,
    /// We are the leader or an admin.
    pub i_am_admin: bool,
    /// False once we've left or been removed.
    pub active: bool,
    pub members: Vec<GroupMemberView>,
}

fn reach(peers: Vec<[u8; 32]>) {
    for peer in peers {
        poke(Some(peer));
    }
}

/// Starts a group with these friends (their chat ids); we're its admin.
/// Most people in a group, counting its admin.
#[flutter_rust_bridge::frb(sync)]
pub fn group_max_members() -> u32 {
    sanctum_core::MAX_GROUP as u32
}

pub fn group_create(title: String, friends: Vec<i64>) -> anyhow::Result<i64> {
    let (chat, invitees) = session::with(|s| Ok(s.create_group(&title, &friends)?))?;
    reach(invitees);
    Ok(chat)
}

pub fn group_details(chat_id: i64) -> anyhow::Result<GroupDetails> {
    let view = session::with(|s| Ok(s.group_view(chat_id)?))?;
    Ok(GroupDetails {
        title: view.title,
        admin_is_me: view.admin_is_me,
        i_am_admin: view.i_am_admin,
        active: view.active,
        members: view
            .members
            .into_iter()
            .map(|m| GroupMemberView {
                key: m.identity.to_vec(),
                name: m.name,
                admin: m.admin,
                leader: m.leader,
                me: m.me,
                pending: m.pending,
            })
            .collect(),
    })
}

/// Adds friends; with `share_history`, the newcomers also get the last week
/// of the group's text messages.
pub fn group_add(chat_id: i64, friends: Vec<i64>, share_history: bool) -> anyhow::Result<()> {
    reach(session::with(|s| Ok(s.group_add(chat_id, &friends, share_history)?))?);
    Ok(())
}

fn key32(member: Vec<u8>) -> anyhow::Result<[u8; 32]> {
    member.try_into().map_err(|_| anyhow::anyhow!("keys are 32 bytes"))
}

/// Makes a member an admin (leader only).
pub fn group_make_admin(chat_id: i64, member: Vec<u8>) -> anyhow::Result<()> {
    let member = key32(member)?;
    reach(session::with(|s| Ok(s.group_make_admin(chat_id, &member)?))?);
    Ok(())
}

/// Takes admin rights away.
pub fn group_remove_admin(chat_id: i64, member: Vec<u8>) -> anyhow::Result<()> {
    let member = key32(member)?;
    reach(session::with(|s| Ok(s.group_remove_admin(chat_id, &member)?))?);
    Ok(())
}

/// The leader makes an admin the new leader and becomes an admin.
pub fn group_hand_over(chat_id: i64, member: Vec<u8>) -> anyhow::Result<()> {
    let member = key32(member)?;
    reach(session::with(|s| Ok(s.group_hand_over(chat_id, &member)?))?);
    Ok(())
}

pub fn group_remove(chat_id: i64, member: Vec<u8>) -> anyhow::Result<()> {
    let member = key32(member)?;
    reach(session::with(|s| Ok(s.group_remove(chat_id, &member)?))?);
    Ok(())
}

pub fn group_rename(chat_id: i64, title: String) -> anyhow::Result<()> {
    reach(session::with(|s| Ok(s.group_rename(chat_id, &title)?))?);
    Ok(())
}

/// Leaves (everyone is told) and deletes the group from this phone.
pub fn group_leave(chat_id: i64) -> anyhow::Result<()> {
    reach(session::with(|s| Ok(s.group_leave(chat_id)?))?);
    Ok(())
}
