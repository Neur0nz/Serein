//! Explicit account actions; one pending write, never an automatic retry.
use crate::optimistic::ActionKind;
use crate::{
	Command, State,
	auth::{AuthState, Failure},
};
use model::Id;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const MAX_RELATIONSHIPS: usize = 4000;
pub const MAX_RELATIONSHIP_BYTES: usize = 128 * 1024;
pub const MAX_FRIEND_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_RESTRICTED_BYTES: usize = 2 * 1024 * 1024;
const MAX_IGNORED_BYTES: usize = 512 * 1024;
const IGNORED_ENTRY_BYTES: usize = size_of::<(Id, bool)>() + 64;
/// Session-only personal drafts, each already bounded by `valid_personal_text`.
const MAX_FAILED_CONTACT_EDITS: usize = 8;
/// Group DMs hold ten people: the signed-in account and nine others it invites.
pub const MAX_GROUP_RECIPIENTS: usize = 9;

fn replace_id_set(
	dest: &mut BTreeSet<Id>,
	entries: Option<Vec<Id>>,
	exclusive: Option<&mut BTreeSet<Id>>,
	capacity: &'static str,
	invalid: &'static str,
) -> Result<(), &'static str> {
	let Some(entries) = entries else {
		dest.clear();
		return Ok(());
	};
	if entries.len() > MAX_RELATIONSHIPS
		|| entries.capacity() * size_of::<Id>() > MAX_RELATIONSHIP_BYTES
	{
		return Err(capacity);
	}
	let mut next = BTreeSet::new();
	for id in entries {
		if id.0 == 0 || !next.insert(id) {
			return Err(invalid);
		}
	}
	if let Some(exclusive) = exclusive {
		for id in &next {
			exclusive.remove(id);
		}
	}
	*dest = next;
	Ok(())
}

#[derive(Clone, PartialEq, Eq)]
pub enum Action {
	LoadNote(Id),
	Note {
		user: Id,
		text: String,
	},
	Nickname {
		user: Id,
		text: String,
	},
	AddFriend {
		username: String,
	},
	ResolveFriend {
		user: Id,
		accept: bool,
	},
	ProfileFriend {
		user: Id,
		friend: bool,
	},
	OpenDm(Id),
	/// Two to nine other people; the service adds the signed-in account as the owner.
	CreateGroup {
		recipients: Vec<Id>,
	},
	CloseDm(Id),
	Block {
		user: Id,
		blocked: bool,
	},
	Ignore {
		user: Id,
		ignored: bool,
	},
	Mute {
		channel: Id,
		muted: bool,
	},
	MessageRequest {
		channel: Id,
		accept: bool,
	},
}
impl std::fmt::Debug for Action {
	/// Redacted debug output; never prints note, nickname or username text.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("UserAction")
			.field("kind", &std::mem::discriminant(self))
			.finish_non_exhaustive()
	}
}
impl Action {
	pub fn completion_label(&self) -> &'static str {
		match self {
			Self::LoadNote(_) => "Note loaded",
			Self::OpenDm(_) => "Direct message was not confirmed; try opening it again",
			Self::CreateGroup { .. } => "Group message was not confirmed; try creating it again",
			Self::Note { .. } => "Note saved",
			Self::Nickname { .. } => "Nickname saved",
			Self::AddFriend { .. } => "Friend request sent · waiting for service update",
			Self::ProfileFriend { friend: true, .. } => "Friend request sent",
			Self::ProfileFriend { friend: false, .. } => "Friend removed",
			Self::ResolveFriend { accept: true, .. } => "Friend request accepted",
			Self::ResolveFriend { accept: false, .. } => "Friend request removed",
			Self::CloseDm(_) => "DM closed · messages and drafts were not deleted",
			Self::Block { blocked: true, .. } => "User blocked",
			Self::Block { blocked: false, .. } => "User unblocked",
			Self::Ignore { ignored: true, .. } => "User ignored",
			Self::Ignore { ignored: false, .. } => "User unignored",
			Self::Mute { muted: true, .. } => {
				"Conversation notifications muted until you turn them back on"
			}
			Self::Mute { muted: false, .. } => "Conversation notifications unmuted",
			Self::MessageRequest { accept: true, .. } => "Message request accepted",
			Self::MessageRequest { accept: false, .. } => "Message request ignored",
		}
	}
}

/// Friendship-establishing writes can require a user-solved captcha; removals do not.
pub fn establishes_friendship(action: &Action) -> bool {
	matches!(action, Action::AddFriend { .. })
		|| matches!(action, Action::ResolveFriend { accept: true, .. })
		|| matches!(action, Action::ProfileFriend { friend: true, .. })
}
/// The challenge identity for a write that can require a user-solved captcha, if any.
pub fn challenge_target(action: &Action) -> Option<crate::captcha::Target> {
	match action {
		// Opening a conversation with someone new is commonly gated by the service.
		Action::OpenDm(user) => Some(crate::captcha::Target::Direct { user: *user }),
		Action::AddFriend { username } => Some(crate::captcha::Target::Username {
			username: username.clone(),
		}),
		Action::ResolveFriend { user, accept: true }
		| Action::ProfileFriend { user, friend: true } => {
			Some(crate::captcha::Target::Friend { user: *user })
		}
		_ => None,
	}
}

/// Whether one explicit retry of this write may carry a solved captcha.
pub fn challengeable(action: &Action) -> bool {
	challenge_target(action).is_some()
}

pub enum Event {
	DmOpened {
		user: Id,
		request: u64,
		result: Result<Box<model::Channel>, Failure>,
	},
	GroupCreated {
		request: u64,
		recipients: Vec<Id>,
		result: Result<Box<model::Channel>, Failure>,
	},
	NoteChanged {
		user: Id,
		text: String,
	},
	NoteLoaded {
		user: Id,
		request: u64,
		result: Result<String, Failure>,
	},
	Nicknames(Vec<(Id, String)>),
	/// Friendship start times in Unix seconds; `replace` drops every earlier entry first.
	FriendsSince {
		entries: Vec<(Id, i64)>,
		replace: bool,
	},
	Nickname {
		user: Id,
		text: String,
	},
	Requests(Option<Vec<(model::User, String, bool)>>),
	Request {
		user: Id,
		incoming: Option<bool>,
		profile: Option<(model::User, String)>,
	},
	FriendProfile((model::User, String)),
	Friends(Option<Vec<(model::User, String)>>),
	Friend {
		user: Id,
		friend: bool,
		profile: Option<(model::User, String)>,
	},
	Restrictions(Option<Vec<(model::User, String, bool)>>),
	Restriction {
		user: Id,
		ignored: Option<bool>,
		profile: Option<(model::User, String)>,
	},
	Relationships(Option<Vec<(Id, bool)>>),
	Relationship {
		user: Id,
		blocked: bool,
	},
	MessageRequests(Option<Vec<Id>>),
	MessageRequest {
		channel: Id,
		pending: bool,
	},
	MessageSpams(Option<Vec<Id>>),
	MessageSpam {
		channel: Id,
		spam: bool,
	},
	RequestSpams(Option<Vec<Id>>),
	RequestSpam {
		user: Id,
		spam: bool,
	},
	Written {
		action: Action,
		request: u64,
		result: Result<(), Failure>,
	},
	Challenge {
		action: Action,
		request: u64,
		challenge: Box<crate::captcha::Challenge>,
	},
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CoalescedKey {
	Block(Id),
	Ignore(Id),
	Mute(Id),
	Note(Id),
	Nickname(Id),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CoalescedValue {
	Bool(bool),
	Text(String),
}

impl CoalescedValue {
	fn as_bool(&self) -> bool {
		match self {
			Self::Bool(value) => *value,
			Self::Text(_) => false,
		}
	}
	fn as_text(&self) -> &str {
		match self {
			Self::Text(text) => text.as_str(),
			Self::Bool(_) => "",
		}
	}
	fn into_text(self) -> String {
		match self {
			Self::Text(text) => text,
			Self::Bool(_) => String::new(),
		}
	}
}

fn coalesced_action(key: CoalescedKey, value: CoalescedValue) -> Action {
	match key {
		CoalescedKey::Block(user) => Action::Block {
			user,
			blocked: value.as_bool(),
		},
		CoalescedKey::Ignore(user) => Action::Ignore {
			user,
			ignored: value.as_bool(),
		},
		CoalescedKey::Mute(channel) => Action::Mute {
			channel,
			muted: value.as_bool(),
		},
		CoalescedKey::Note(user) => Action::Note {
			user,
			text: value.into_text(),
		},
		CoalescedKey::Nickname(user) => Action::Nickname {
			user,
			text: value.into_text(),
		},
	}
}

fn action_coalesced_key(action: &Action) -> Option<CoalescedKey> {
	match action {
		Action::Block { user, .. } => Some(CoalescedKey::Block(*user)),
		Action::Ignore { user, .. } => Some(CoalescedKey::Ignore(*user)),
		Action::Mute { channel, .. } => Some(CoalescedKey::Mute(*channel)),
		Action::Note { user, .. } => Some(CoalescedKey::Note(*user)),
		Action::Nickname { user, .. } => Some(CoalescedKey::Nickname(*user)),
		_ => None,
	}
}

fn contact_edit_key(user: Id, nickname: bool) -> CoalescedKey {
	if nickname {
		CoalescedKey::Nickname(user)
	} else {
		CoalescedKey::Note(user)
	}
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum OneShotKey {
	AddFriend(String),
	ResolveFriend(Id),
	ProfileFriend(Id),
	CloseDm(Id),
	MessageRequest(Id),
	OpenDm(Id),
	CreateGroup(Vec<Id>),
}

fn action_one_shot_key(action: &Action) -> Option<OneShotKey> {
	match action {
		Action::AddFriend { username } => Some(OneShotKey::AddFriend(username.clone())),
		Action::ResolveFriend { user, .. } => Some(OneShotKey::ResolveFriend(*user)),
		Action::ProfileFriend { user, .. } => Some(OneShotKey::ProfileFriend(*user)),
		Action::CloseDm(channel) => Some(OneShotKey::CloseDm(*channel)),
		Action::MessageRequest { channel, .. } => Some(OneShotKey::MessageRequest(*channel)),
		Action::OpenDm(user) => Some(OneShotKey::OpenDm(*user)),
		Action::CreateGroup { recipients } => Some(OneShotKey::CreateGroup(recipients.clone())),
		_ => None,
	}
}

struct OneShotEntry {
	action: Action,
	label: String,
	observed: bool,
	/// Prior membership when the displayed friend map was updated optimistically.
	pending_friendship: Option<(Id, bool)>,
	rollback: OneShotRollback,
}

enum OneShotRollback {
	AddFriend {
		username: String,
	},
	ResolveFriend {
		user: Id,
		entry: (model::User, String, bool),
		accept: bool,
	},
	ProfileFriend {
		user: Id,
		friend: bool,
		inserted_request: bool,
		removed_friend: Option<(model::User, String, Option<String>, Option<i64>)>,
	},
	CloseDm {
		channel: Option<Box<model::Channel>>,
	},
	MessageRequest {
		channel: Id,
		accept: bool,
		removed_channel: Option<Box<model::Channel>>,
	},
	OpenDm {
		origin: (Option<Id>, u64, bool),
	},
	CreateGroup {
		origin: (Option<Id>, u64),
	},
}

#[derive(Default)]
pub struct Actions {
	note: Option<(Id, String)>,
	note_request: Option<(Id, u64, bool)>,
	failed_contact_edits: Vec<(CoalescedKey, Box<str>)>,
	nicknames: BTreeMap<Id, String>,
	friends_since: BTreeMap<Id, i64>,
	requests: BTreeMap<Id, (model::User, String, bool)>,
	requests_known: bool,
	last_requested: Option<String>,
	failed_add_friends: VecDeque<String>,
	friends: BTreeMap<Id, (model::User, String)>,
	friends_known: bool,
	restricted: BTreeMap<Id, (model::User, String, bool)>,
	restricted_known: bool,
	/// Committed status survives missing display profiles; bounded independently.
	ignored: BTreeMap<Id, bool>,
	relationships: BTreeMap<Id, bool>,
	message_requests: BTreeSet<Id>,
	spam_directs: BTreeSet<Id>,
	spam_requests: BTreeSet<Id>,
	known: bool,
	view: u64,
	sequence: u64,
	/// Bounded latest-wins coalescing for Block, Ignore, DM Mute, Note, and Nickname.
	coalesced: crate::optimistic::CoalescedSlot<CoalescedKey, CoalescedValue>,
	coalesced_labels: BTreeMap<CoalescedKey, String>,
	coalesced_observed: BTreeSet<CoalescedKey>,
	/// Bounded concurrent one-shot writes (AddFriend, ResolveFriend, ProfileFriend, CloseDm,
	/// MessageRequest, OpenDm, CreateGroup).
	one_shots: crate::optimistic::InFlightSet<OneShotKey, OneShotEntry>,
	challenge: Option<(
		OneShotKey,
		Action,
		u64,
		std::time::Instant,
		crate::captcha::Challenge,
	)>,
	// Friendship is required only for the existing friend-only navigation callers.
	dm_origin: Option<(Option<Id>, u64, bool)>,
	opened_dm: Option<(Id, Id)>,
	/// Navigation snapshot and the admitted channel of one group creation.
	group_origin: Option<(Option<Id>, u64)>,
	created_group: Option<Id>,
	status: Option<&'static str>,
}
impl Actions {
	pub(crate) fn reset(&mut self) {
		let mut coalesced = std::mem::take(&mut self.coalesced);
		let mut one_shots = std::mem::take(&mut self.one_shots);
		coalesced.cancel_all();
		one_shots.cancel_all();
		*self = Self {
			sequence: self.sequence,
			view: self.view.wrapping_add(1),
			failed_contact_edits: std::mem::take(&mut self.failed_contact_edits),
			failed_add_friends: std::mem::take(&mut self.failed_add_friends),
			coalesced,
			one_shots,
			..Self::default()
		};
	}
	fn bump_view(&mut self) {
		self.view = self.view.wrapping_add(1);
	}
	fn keep_failed_contact_edit(&mut self, key: CoalescedKey, draft: String) {
		self.failed_contact_edits
			.retain(|(target, _)| *target != key);
		if self.failed_contact_edits.len() >= MAX_FAILED_CONTACT_EDITS {
			self.failed_contact_edits.remove(0);
		}
		self.failed_contact_edits
			.push((key, draft.into_boxed_str()));
	}
}
impl State {
	/// Changes whenever friend filtering or ordering may change, including optimistic blocks.
	pub fn relationship_view(&self) -> u64 {
		self.user_actions.view
	}
	pub fn user_note(&self, user: Id) -> Option<&str> {
		if let Some(value) = self
			.user_actions
			.coalesced
			.desired(&CoalescedKey::Note(user))
		{
			return Some(value.as_text());
		}
		self.user_actions
			.note
			.as_ref()
			.filter(|(id, _)| *id == user)
			.map(|(_, text)| text.as_str())
	}
	pub fn user_note_loading(&self, user: Id) -> bool {
		self.user_actions
			.note_request
			.is_some_and(|(id, _, _)| id == user)
	}
	pub fn friend_nickname(&self, user: Id) -> Option<&str> {
		if let Some(value) = self
			.user_actions
			.coalesced
			.desired(&CoalescedKey::Nickname(user))
		{
			let text = value.as_text();
			return (!text.is_empty()).then_some(text);
		}
		self.user_actions.nicknames.get(&user).map(String::as_str)
	}
	/// Failed personal text can be reopened for retry within this account session.
	pub fn failed_contact_edit(&self, user: Id, nickname: bool) -> Option<&str> {
		let key = contact_edit_key(user, nickname);
		self.user_actions
			.failed_contact_edits
			.iter()
			.find(|(target, _)| *target == key)
			.map(|(_, text)| text.as_ref())
	}
	pub fn discard_failed_contact_edit(&mut self, user: Id, nickname: bool) {
		let key = contact_edit_key(user, nickname);
		self.user_actions
			.failed_contact_edits
			.retain(|(target, _)| *target != key);
	}
	/// When the friendship began, if the service reported it for a current friend.
	pub fn friend_since(&self, user: Id) -> Option<i64> {
		self.friend(user)?;
		self.user_actions.friends_since.get(&user).copied()
	}
	pub fn user_display_name<'a>(&'a self, user: &'a model::User) -> &'a str {
		self.friend_nickname(user.id).unwrap_or(&user.name)
	}
	pub fn message_author_name<'a>(&'a self, message: &'a model::Message) -> &'a str {
		if message.author.webhook {
			return &message.author.name;
		}
		if let Some(guild) = self
			.channel(message.channel)
			.and_then(|channel| channel.guild)
		{
			let member = self
				.members
				.as_ref()
				.filter(|list| list.guild == Some(guild))
				.and_then(|list| {
					list.slots
						.iter()
						.flatten()
						.filter_map(|slot| match slot {
							model::MemberSlot::Person(m) => Some(m),
							_ => None,
						})
						.find(|m| m.user.id == message.author.id)
				});
			let nick = member
				.and_then(|m| m.nick.as_deref().filter(|nick| !nick.is_empty()))
				.or(message.author_nick.as_deref());
			if let Some(nick) = nick.filter(|nick| !nick.is_empty()) {
				return nick;
			}
		}
		self.user_display_name(&message.author)
	}
	pub fn conversation_name<'a>(&'a self, channel: &'a model::Channel) -> &'a str {
		if channel.kind == 1
			&& let Some(user) = channel.recipients.first()
		{
			self.friend_nickname(user.id).unwrap_or(&channel.name)
		} else {
			&channel.name
		}
	}
	pub fn load_user_note(&mut self, user: Id) -> Option<Command> {
		if user.0 == 0
			|| self
				.user_actions
				.note_request
				.is_some_and(|(id, _, _)| id == user)
			|| (!self.demo && (self.auth != AuthState::Authenticated || !self.gateway_connected))
		{
			return None;
		}
		if self.user_note(user).is_none() {
			self.user_actions.note = None;
		}
		self.user_actions.sequence = self.user_actions.sequence.wrapping_add(1);
		let request = self.user_actions.sequence;
		self.user_actions.note_request = Some((user, request, false));
		Some(Command::UserAction {
			action: Action::LoadNote(user),
			request,
			captcha: None,
		})
	}
	pub(crate) fn cancel_user_note_read(&mut self) {
		self.user_actions.note_request = None;
	}
	pub fn set_user_note(&mut self, user: Id, text: String) -> Option<Command> {
		if self.user_note(user).is_none() || !valid_personal_text(&text, false) {
			return None;
		}
		self.request_user_action(Action::Note {
			user,
			text: text.as_str().to_owned(),
		})
	}
	pub fn set_friend_nickname(&mut self, user: Id, text: String) -> Option<Command> {
		if !self.friends().any(|friend| friend.id == user) || !valid_personal_text(&text, true) {
			return None;
		}
		self.request_user_action(Action::Nickname {
			user,
			text: text.as_str().to_owned(),
		})
	}
	pub fn pending_friends(&self) -> impl Iterator<Item = &(model::User, String, bool)> {
		self.user_actions
			.requests
			.values()
			.filter(|(u, _, _)| self.user_blocked(u.id) == Some(false))
	}
	fn stranger_message_request(&self, channel: &model::Channel) -> bool {
		let mut other = false;
		for user in &channel.recipients {
			if self.user_actions.friends.contains_key(&user.id)
				|| self.user_blocked(user.id) == Some(true)
			{
				return false;
			}
			other = true;
		}
		other
	}
	pub fn home_request_parts(&self) -> (u32, u32) {
		let friends = self
			.pending_friends()
			.filter(|(user, _, incoming)| {
				*incoming && !self.user_actions.spam_requests.contains(&user.id)
			})
			.count();
		let messages = self
			.user_actions
			.message_requests
			.iter()
			.filter(|channel| {
				self.channel(**channel)
					.is_some_and(|channel| self.stranger_message_request(channel))
			})
			.count();
		(
			u32::try_from(friends).unwrap_or(u32::MAX),
			u32::try_from(messages).unwrap_or(u32::MAX),
		)
	}
	pub fn home_request_count(&self) -> u32 {
		let (friends, messages) = self.home_request_parts();
		friends.saturating_add(messages)
	}
	pub(crate) fn forget_direct_inbox(&mut self, channel: Id) {
		self.user_actions.message_requests.remove(&channel);
		self.user_actions.spam_directs.remove(&channel);
	}
	pub(crate) fn message_request_pending(&self, channel: Id) -> bool {
		self.user_actions.message_requests.contains(&channel)
	}
	/// A pending request from someone who is neither a friend nor blocked; the same rule as the badge.
	pub fn message_request(&self, channel: Id) -> bool {
		self.message_request_pending(channel)
			&& self
				.channel(channel)
				.is_some_and(|channel| self.stranger_message_request(channel))
	}
	/// Accepts or ignores a pending message request. Ignoring removes the conversation.
	pub fn resolve_message_request(&mut self, channel: Id, accept: bool) -> Option<Command> {
		if !self.message_request(channel) {
			return None;
		}
		self.request_user_action(Action::MessageRequest { channel, accept })
	}
	pub fn spam_direct(&self, channel: Id) -> bool {
		self.user_actions.spam_directs.contains(&channel)
	}
	fn set_message_request(&mut self, channel: Id, pending: bool) -> Result<(), &'static str> {
		if channel.0 == 0 {
			return Err("Invalid message request");
		}
		if pending {
			if !self.user_actions.message_requests.contains(&channel)
				&& self.user_actions.message_requests.len() >= MAX_RELATIONSHIPS
			{
				return Err("Message requests exceed safe capacity");
			}
			self.user_actions.spam_directs.remove(&channel);
			self.user_actions.message_requests.insert(channel);
		} else {
			self.user_actions.message_requests.remove(&channel);
		}
		self.user_actions.bump_view();
		Ok(())
	}
	fn set_spam_request(&mut self, user: Id, spam: bool) -> Result<(), &'static str> {
		if user.0 == 0 {
			return Err("Invalid friend request");
		}
		if spam {
			if !self.user_actions.spam_requests.contains(&user)
				&& self.user_actions.spam_requests.len() >= MAX_RELATIONSHIPS
			{
				return Err("Friend requests exceed safe capacity");
			}
			self.user_actions.spam_requests.insert(user);
		} else {
			self.user_actions.spam_requests.remove(&user);
		}
		Ok(())
	}
	fn set_spam_direct(&mut self, channel: Id, spam: bool) -> Result<(), &'static str> {
		if channel.0 == 0 {
			return Err("Invalid message request");
		}
		if spam {
			if !self.user_actions.spam_directs.contains(&channel)
				&& self.user_actions.spam_directs.len() >= MAX_RELATIONSHIPS
			{
				return Err("Message requests exceed safe capacity");
			}
			self.user_actions.message_requests.remove(&channel);
			self.user_actions.spam_directs.insert(channel);
		} else {
			self.user_actions.spam_directs.remove(&channel);
		}
		Ok(())
	}
	pub fn friend_requests_known(&self) -> bool {
		self.user_actions.requests_known
	}
	pub fn add_friend(&mut self, username: &str) -> Option<Command> {
		if username.len() > 132 {
			self.user_actions.status = Some("Enter a Discord username of at most 32 characters.");
			return None;
		}
		let username = username.trim().trim_start_matches('@').to_ascii_lowercase();
		if !valid_username(&username) {
			self.user_actions.status =
				Some("Enter a Discord username: 2–32 letters, numbers, underscores or periods.");
			return None;
		}
		if self.user_actions.last_requested.as_deref() == Some(&username)
			|| self
				.user_actions
				.friends
				.values()
				.any(|(_, n)| n == &username)
			|| self
				.user_actions
				.requests
				.values()
				.any(|(_, n, _)| n == &username)
			|| self
				.user_actions
				.one_shots
				.contains(&OneShotKey::AddFriend(username.clone()))
		{
			self.user_actions.status =
				Some("Already friends or a request is pending. Check the Pending tab.");
			return None;
		}
		self.request_user_action(Action::AddFriend { username })
	}
	/// In-flight outgoing friend requests not yet confirmed by a Gateway `RELATIONSHIP_ADD`.
	pub fn optimistic_outgoing_requests(&self) -> Vec<String> {
		self.user_actions
			.one_shots
			.iter()
			.filter_map(|(key, _)| match key {
				OneShotKey::AddFriend(username)
					if !self
						.user_actions
						.requests
						.values()
						.any(|(_, name, incoming)| !*incoming && name == username) =>
				{
					Some(username.clone())
				}
				_ => None,
			})
			.collect()
	}
	/// Consumes a failed `AddFriend` username so the UI can restore an empty input box.
	pub fn take_failed_add_friend(&mut self) -> Option<String> {
		self.user_actions.failed_add_friends.pop_front()
	}
	pub fn add_profile_friend(&mut self, user: Id) -> Option<Command> {
		if !self.profile_friend_action_allowed(user)
			|| self.user_actions.friends.contains_key(&user)
			|| self.user_actions.requests.contains_key(&user)
		{
			return None;
		}
		self.request_user_action(Action::ProfileFriend { user, friend: true })
	}
	pub fn remove_friend(&mut self, user: Id) -> Option<Command> {
		if !self.profile_friend_action_allowed(user)
			|| !self.user_actions.friends.contains_key(&user)
			|| self
				.user_actions
				.one_shots
				.contains(&OneShotKey::ResolveFriend(user))
		{
			return None;
		}
		self.request_user_action(Action::ProfileFriend {
			user,
			friend: false,
		})
	}
	fn profile_friend_action_allowed(&self, user: Id) -> bool {
		user.0 != 0
			&& self.user.as_ref().is_some_and(|owner| owner.id != user)
			&& self.friends_known()
			&& self.friend_requests_known()
			&& self.user_blocked(user) == Some(false)
	}
	/// Accepts or declines one incoming friend request.
	pub fn resolve_friend_request(&mut self, user: Id, accept: bool) -> Option<Command> {
		let (_, _, incoming) = self.user_actions.requests.get(&user)?;
		if (accept && !incoming)
			|| self.user_blocked(user) != Some(false)
			|| self
				.user_actions
				.one_shots
				.contains(&OneShotKey::ProfileFriend(user))
		{
			return None;
		}
		self.request_user_action(Action::ResolveFriend { user, accept })
	}
	/// The one pending account write that is waiting on a user-solved challenge.
	pub fn account_challenge(&self) -> Option<(u64, &crate::captcha::Challenge)> {
		let (_, action, request, at, challenge) = self.user_actions.challenge.as_ref()?;
		if !challengeable(action) {
			return None;
		}
		(at.elapsed() < crate::captcha::LIFETIME).then_some((*request, challenge))
	}
	/// The user-visible flow for the pending account challenge.
	pub(crate) fn account_verification(
		&self,
	) -> Option<(crate::captcha::Verification, &crate::captcha::Challenge)> {
		let (_, action, request, at, challenge) = self.user_actions.challenge.as_ref()?;
		if !challengeable(action) || at.elapsed() >= crate::captcha::LIFETIME {
			return None;
		}
		let direct = matches!(action, Action::OpenDm(_));
		Some((
			if direct {
				crate::captcha::Verification::Direct { request: *request }
			} else {
				crate::captcha::Verification::Friend { request: *request }
			},
			challenge,
		))
	}
	/// Releases a challenged write that will not be resumed, rolling back that target cleanly.
	fn release_challenge(&mut self, cancelled: bool) {
		let Some((key, action, seq, _, _)) = self.user_actions.challenge.take() else {
			return;
		};
		let direct = matches!(action, Action::OpenDm(_));
		if let Some(entry) = self.user_actions.one_shots.finish(&key, seq) {
			self.rollback_one_shot(entry, None);
		}
		if direct {
			self.user_actions.dm_origin = None;
			self.user_actions.opened_dm = None;
		}
		self.user_actions.status = Some(match (direct, cancelled) {
			(true, true) => "Verification cancelled; the conversation was not opened.",
			(true, false) => "Verification expired; open the conversation again.",
			(false, true) => "Verification cancelled; the friend request was not sent.",
			(false, false) => "Verification expired; send the friend request again.",
		});
		self.status = self.user_actions.status.unwrap();
	}
	/// Builds the one explicit retry that resumes a solved account challenge.
	pub fn resume_account_challenge(
		&mut self,
		request: u64,
		solution: crate::captcha::Solution,
	) -> Option<Command> {
		if self.demo || !self.gateway_connected || self.auth != AuthState::Authenticated {
			return None;
		}
		if self.account_challenge()?.0 != request {
			return None;
		}
		let (key, action, old_seq, at, challenge) = self.user_actions.challenge.take()?;
		let entry = self.user_actions.one_shots.finish(&key, old_seq)?;
		let target = challenge_target(&action)?;
		// The new identity makes a duplicated response to the first attempt stale.
		let Ok(request) = self.user_actions.one_shots.insert(key, entry) else {
			return None;
		};
		Some(Command::UserAction {
			action,
			request,
			captcha: Some(Box::new(crate::captcha::Retry {
				target,
				request,
				challenge,
				solution,
				expires: at + crate::captcha::LIFETIME,
			})),
		})
	}
	/// Cancels the pending account challenge and releases its write.
	pub fn cancel_account_challenge(&mut self, request: u64) {
		if self
			.account_challenge()
			.is_some_and(|(pending, _)| pending == request)
		{
			self.release_challenge(true);
		}
	}
	/// Releases an account challenge that outlived its five-minute lifetime.
	pub(crate) fn expire_account_challenge(&mut self) {
		if self
			.user_actions
			.challenge
			.as_ref()
			.is_some_and(|(_, _, _, at, _)| at.elapsed() >= crate::captcha::LIFETIME)
		{
			self.release_challenge(false);
		}
	}
	/// Visible friends, filtered by the current relationship snapshot.
	pub fn friends(&self) -> impl Iterator<Item = &model::User> {
		self.user_actions
			.friends
			.values()
			.map(|(user, _)| user)
			.filter(|u| self.user_blocked(u.id) == Some(false))
	}
	pub fn friend(&self, user: Id) -> Option<&model::User> {
		self.user_actions
			.friends
			.get(&user)
			.map(|(user, _)| user)
			.filter(|user| self.user_blocked(user.id) == Some(false))
	}
	/// Rebuilds the friend map from a snapshot; `None` leaves friends unknown.
	fn replace_friends(
		&mut self,
		entries: Option<Vec<(model::User, String)>>,
	) -> Result<(), &'static str> {
		self.user_actions.friends_known = false;
		if let Some(entries) = entries {
			if entries.len() > MAX_RELATIONSHIPS
				|| entries.capacity() * size_of::<(model::User, String)>()
					+ entries
						.iter()
						.map(|(u, n)| u.heap_bytes() + n.capacity() + 64)
						.sum::<usize>()
					> MAX_FRIEND_BYTES
			{
				return Err("Friends exceed safe capacity");
			}
			for (user, name) in entries {
				if !valid_friend(&user, &name)
					|| self
						.user_actions
						.friends
						.insert(user.id, (user, name))
						.is_some()
				{
					self.user_actions.friends.clear();
					return Err("Friends contain invalid or duplicate users");
				}
			}
			self.user_actions.friends_known = true;
		}
		Ok(())
	}
	pub(crate) fn friend_count(&self) -> usize {
		self.user_actions.friends.len()
	}
	pub fn friends_known(&self) -> bool {
		self.user_actions.friends_known
	}
	pub fn restricted_users(&self) -> impl Iterator<Item = &(model::User, String, bool)> {
		self.user_actions.restricted.values()
	}
	pub fn restricted_user(&self, user: Id) -> Option<&(model::User, String, bool)> {
		self.user_actions.restricted.get(&user)
	}
	pub fn restricted_users_known(&self) -> bool {
		self.user_actions.restricted_known
	}
	pub fn friend_username(&self, user: Id) -> Option<&str> {
		self.user_actions
			.friends
			.get(&user)
			.map(|(_, name)| name.as_str())
	}
	/// Committed relationship snapshots only; optimistic toggles cannot prove server eligibility.
	pub(crate) fn service_author_excluded(&self, user: Id) -> Option<bool> {
		let blocked = self.confirmed_user_blocked(user);
		let ignored = self.confirmed_user_ignored(user);
		match (blocked, ignored) {
			(Some(true), _) | (_, Some(true)) => Some(true),
			(Some(false), Some(false)) => {
				let changing = self
					.user_actions
					.coalesced
					.entry(&CoalescedKey::Block(user))
					.is_some() || self
					.user_actions
					.coalesced
					.entry(&CoalescedKey::Ignore(user))
					.is_some();
				(!changing).then_some(false)
			}
			_ => None,
		}
	}
	fn confirmed_user_blocked(&self, user: Id) -> Option<bool> {
		self.user_actions
			.relationships
			.get(&user)
			.copied()
			.or_else(|| (self.demo || self.user_actions.known).then_some(false))
	}
	fn confirmed_user_ignored(&self, user: Id) -> Option<bool> {
		if let Some(ignored) = self.user_actions.ignored.get(&user) {
			return Some(*ignored);
		}
		if let Some((_, _, ignored)) = self.user_actions.restricted.get(&user) {
			return Some(*ignored);
		}
		(self.demo || self.user_actions.restricted_known).then_some(false)
	}
	pub fn user_blocked(&self, user: Id) -> Option<bool> {
		if let Some(value) = self
			.user_actions
			.coalesced
			.desired(&CoalescedKey::Block(user))
		{
			return Some(value.as_bool());
		}
		self.confirmed_user_blocked(user)
	}
	/// Whether the user is ignored, including an optimistic pending toggle; `None` while unknown.
	pub fn user_ignored(&self, user: Id) -> Option<bool> {
		if let Some(value) = self
			.user_actions
			.coalesced
			.desired(&CoalescedKey::Ignore(user))
		{
			return Some(value.as_bool());
		}
		self.confirmed_user_ignored(user)
	}
	/// Record a committed ignore/unignore without requiring a display profile.
	fn store_ignored(&mut self, user: Id, ignored: bool) -> Result<(), &'static str> {
		if user.0 == 0 {
			return Err("Invalid restricted user update");
		}
		let actions = &mut self.user_actions;
		if !actions.ignored.contains_key(&user) {
			while actions.ignored.len() >= MAX_RELATIONSHIPS
				|| (actions.ignored.len() + 1) * IGNORED_ENTRY_BYTES > MAX_IGNORED_BYTES
			{
				actions.ignored.pop_first();
				// An evicted profile-less status must become unknown, never implicitly unignored.
				actions.restricted_known = false;
			}
		}
		actions.ignored.insert(user, ignored);
		Ok(())
	}
	pub(crate) fn pending_dm_muted(&self, channel: Id) -> Option<bool> {
		self.user_actions
			.coalesced
			.desired(&CoalescedKey::Mute(channel))
			.map(CoalescedValue::as_bool)
	}
	pub fn user_action_pending(&self) -> bool {
		!self.user_actions.coalesced.is_empty()
			|| !self.user_actions.one_shots.is_empty()
			|| self.user_actions.challenge.is_some()
	}
	pub fn dm_open_pending(&self) -> bool {
		self.user_actions
			.one_shots
			.iter()
			.any(|(key, _)| matches!(key, OneShotKey::OpenDm(_)))
	}
	pub fn group_create_pending(&self) -> bool {
		self.user_actions
			.one_shots
			.iter()
			.any(|(key, _)| matches!(key, OneShotKey::CreateGroup(_)))
	}
	pub fn user_action_status(&self) -> Option<&'static str> {
		self.user_actions.status
	}
	pub fn take_user_action_status(&mut self) -> Option<&'static str> {
		self.user_actions.status.take()
	}
	pub fn open_friend_dm(&mut self, user: Id) -> Option<Command> {
		let friend = self.friend(user)?;
		if friend.webhook || user.0 == 0 || self.user.as_ref().is_none_or(|owner| owner.id == user)
		{
			return None;
		}
		// A loaded friend DM stays navigable while the session reconnects; only creation needs it.
		if let Some(channel) = self.existing_dm(user) {
			return self.select(channel);
		}
		if !self.dm_target_allowed(user, true) {
			return None;
		}
		self.open_dm(user, true)
	}
	/// Whether an explicit message action can open a DM with this loaded user.
	pub fn can_open_user_dm(&self, user: &model::User) -> bool {
		!user.webhook
			&& !self
				.user_actions
				.one_shots
				.contains(&OneShotKey::OpenDm(user.id))
			&& self.dm_target_allowed(user.id, false)
	}
	/// Opens a DM with a loaded user, including people who are not friends.
	pub fn open_user_dm(&mut self, user: &model::User) -> Option<Command> {
		if !self.can_open_user_dm(user) {
			return None;
		}
		self.open_dm(user.id, false)
	}
	fn dm_target_allowed(&self, user: Id, friend_only: bool) -> bool {
		user.0 != 0
			&& self.user.as_ref().is_some_and(|owner| owner.id != user)
			&& self.user_blocked(user) == Some(false)
			&& (!friend_only || self.friend(user).is_some())
			&& (self.demo || (self.auth == AuthState::Authenticated && self.gateway_connected))
	}
	fn existing_dm(&self, user: Id) -> Option<Id> {
		self.channels
			.iter()
			.find(|channel| {
				channel.guild.is_none()
					&& channel.kind == 1
					&& channel.recipients.len() == 1
					&& channel.recipients[0].id == user
					&& !channel.recipients[0].webhook
			})
			.map(|channel| channel.id)
	}
	/// Opens a direct message with two to nine other friends, the most a group DM holds.
	pub fn create_group_dm(&mut self, recipients: &[Id]) -> Option<Command> {
		let mut people: Vec<Id> = Vec::with_capacity(recipients.len());
		for recipient in recipients {
			if people.contains(recipient) {
				continue;
			}
			people.push(*recipient);
		}
		if !(2..=MAX_GROUP_RECIPIENTS).contains(&people.len())
			|| people
				.iter()
				.any(|user| self.friend(*user).is_none() || !self.dm_target_allowed(*user, true))
		{
			self.user_actions.status =
				Some("Choose between two and nine friends for a group message");
			self.status = self.user_actions.status.unwrap();
			return None;
		}
		people.sort_unstable_by_key(|user| user.0);
		if let Some(channel) = self.existing_group(&people) {
			return self.select(channel);
		}
		let origin = (self.selected, self.request);
		let command = self.request_one_shot(
			Action::CreateGroup { recipients: people },
			OneShotRollback::CreateGroup { origin },
		)?;
		self.user_actions.group_origin = Some(origin);
		self.user_actions.created_group = None;
		self.status = "Creating group message…";
		Some(command)
	}
	fn existing_group(&self, people: &[Id]) -> Option<Id> {
		self.channels
			.iter()
			.filter(|channel| channel.guild.is_none() && channel.kind == 3)
			.filter(|channel| channel.recipients.len() == people.len())
			.find(|channel| {
				people
					.iter()
					.all(|user| channel.recipients.iter().any(|entry| entry.id == *user))
			})
			.map(|channel| channel.id)
	}
	/// Consume one admitted group without overriding navigation made during the request.
	pub fn select_created_group(&mut self) -> Option<Command> {
		let channel = self.user_actions.created_group.take()?;
		let (selected, request) = self.user_actions.group_origin.take()?;
		if (selected, request) != (self.selected, self.request)
			|| self
				.channel(channel)
				.is_none_or(|known| known.guild.is_some() || known.kind != 3)
		{
			return None;
		}
		self.select(channel)
	}
	fn open_dm(&mut self, user: Id, friend_only: bool) -> Option<Command> {
		if let Some(channel) = self.existing_dm(user) {
			return self.select(channel);
		}
		let origin = (self.selected, self.request, friend_only);
		let command =
			self.request_one_shot(Action::OpenDm(user), OneShotRollback::OpenDm { origin })?;
		self.user_actions.dm_origin = Some(origin);
		self.user_actions.opened_dm = None;
		self.status = "Opening direct message…";
		Some(command)
	}
	/// Consume one confirmed DM target without overriding navigation made during the request.
	pub fn select_opened_dm(&mut self) -> Option<Command> {
		let (channel, user) = self.user_actions.opened_dm.take()?;
		let (selected, request, friend_only) = self.user_actions.dm_origin.take()?;
		if (selected, request) != (self.selected, self.request)
			|| !self.dm_target_allowed(user, friend_only)
			|| self.channel(channel).is_none_or(|known| {
				known.guild.is_some()
					|| known.kind != 1
					|| known.recipients.len() != 1
					|| known.recipients[0].id != user
					|| known.recipients[0].webhook
			}) {
			return None;
		}
		self.select(channel)
	}
	pub fn close_dm(&mut self, channel: Id) -> Option<Command> {
		if !self.is_one_to_one_dm(channel) {
			return None;
		}
		if self.server_invite_pending()
			|| self.pending.iter().any(|p| p.channel == channel)
			|| self
				.voice
				.active
				.as_ref()
				.is_some_and(|call| call.channel == channel)
		{
			self.user_actions.status =
				Some("Finish pending messages and leave the call before closing this DM");
			self.status = self.user_actions.status.unwrap();
			return None;
		}
		self.request_user_action(Action::CloseDm(channel))
	}
	pub fn set_user_blocked(&mut self, user: Id, blocked: bool) -> Option<Command> {
		if user.0 == 0
			|| self.user.as_ref().is_none_or(|owner| owner.id == user)
			|| (!blocked && self.user_blocked(user) != Some(true))
		{
			return None;
		}
		self.request_user_action(Action::Block { user, blocked })
	}
	pub fn set_user_ignored(&mut self, user: Id, ignored: bool) -> Option<Command> {
		if user.0 == 0
			|| self.user.as_ref().is_none_or(|owner| owner.id == user)
			|| self.user_blocked(user) != Some(false)
			|| self.user_ignored(user) != Some(!ignored)
		{
			return None;
		}
		self.request_user_action(Action::Ignore { user, ignored })
	}
	pub fn set_dm_muted(&mut self, channel: Id, muted: bool) -> Option<Command> {
		if !self.is_one_to_one_dm(channel) && !self.is_group_dm(channel) {
			return None;
		}
		self.request_user_action(Action::Mute { channel, muted })
	}
	fn is_one_to_one_dm(&self, channel: Id) -> bool {
		self.channel(channel)
			.is_some_and(|c| c.guild.is_none() && c.kind == 1 && c.recipients.len() == 1)
	}
	fn user_toast_label(&self, user: Id) -> String {
		if let Some(nick) = self.user_actions.nicknames.get(&user)
			&& !nick.is_empty()
		{
			return nick.clone();
		}
		if let Some((record, _)) = self.user_actions.friends.get(&user) {
			return record.name.clone();
		}
		if let Some((record, _, _)) = self.user_actions.requests.get(&user) {
			return record.name.clone();
		}
		if let Some((record, _, _)) = self.user_actions.restricted.get(&user) {
			return record.name.clone();
		}
		if let Some(profile) = self.profile.as_ref().filter(|p| p.user == user)
			&& let Some(data) = profile.data.as_ref()
		{
			return data.user.name.clone();
		}
		if let Some(found) = self
			.channels
			.iter()
			.flat_map(|ch| &ch.recipients)
			.find(|u| u.id == user)
		{
			return found.name.clone();
		}
		format!("user {user}")
	}
	fn lookup_user_record(&self, user: Id) -> (model::User, String) {
		if let Some((record, name)) = self.user_actions.friends.get(&user) {
			return (record.clone(), name.clone());
		}
		if let Some((record, name, _)) = self.user_actions.requests.get(&user) {
			return (record.clone(), name.clone());
		}
		if let Some(profile) = self.profile.as_ref().filter(|p| p.user == user)
			&& let Some(data) = profile.data.as_ref()
		{
			let username = if data.username.is_empty() {
				data.user.name.clone()
			} else {
				data.username.clone()
			};
			return (data.user.clone(), username);
		}
		if let Some(found) = self
			.channels
			.iter()
			.flat_map(|ch| &ch.recipients)
			.find(|u| u.id == user)
		{
			return (found.clone(), found.name.clone());
		}
		(
			model::User {
				id: user,
				name: "Unknown user".into(),
				avatar: None,
				discriminator: 0,
				primary_guild: None,
				cosmetics: None,
				webhook: false,
				kind: Default::default(),
			},
			format!("User ID: {user}"),
		)
	}
	fn user_action_ready(&mut self) -> bool {
		if !self.demo && (self.auth != AuthState::Authenticated || !self.gateway_connected) {
			self.user_actions.status = Some("User actions unavailable while disconnected");
			self.status = self.user_actions.status.unwrap();
			return false;
		}
		true
	}
	fn request_one_shot(&mut self, action: Action, rollback: OneShotRollback) -> Option<Command> {
		if !self.user_action_ready() {
			return None;
		}
		let key = action_one_shot_key(&action)?;
		let label = match &action {
			Action::AddFriend { username } => format!("@{username}"),
			Action::ResolveFriend { user, .. }
			| Action::ProfileFriend { user, .. }
			| Action::OpenDm(user) => self.user_toast_label(*user),
			Action::CloseDm(channel) | Action::MessageRequest { channel, .. } => {
				self.channel_toast_label(*channel)
			}
			Action::CreateGroup { .. } => "group message".into(),
			_ => String::new(),
		};
		let entry = OneShotEntry {
			action: action.clone(),
			label,
			observed: false,
			pending_friendship: match action {
				Action::ResolveFriend { user, accept: true }
				| Action::ProfileFriend {
					user,
					friend: false,
				} => Some((user, self.user_actions.friends.contains_key(&user))),
				_ => None,
			},
			rollback,
		};
		let seq = match self.user_actions.one_shots.insert(key, entry) {
			Ok(seq) => seq,
			Err(crate::optimistic::InsertError::Duplicate(_)) => return None,
			Err(crate::optimistic::InsertError::Full(_)) => {
				self.user_actions.status =
					Some("Too many user actions in flight; try again shortly");
				return None;
			}
		};
		self.user_actions.status = None;
		Some(Command::UserAction {
			action,
			request: seq,
			captcha: None,
		})
	}
	/// Queues one optimistic user action on either `CoalescedSlot` (idempotent toggles/edits)
	/// or `InFlightSet` (concurrent one-shot transitions).
	fn request_user_action(&mut self, action: Action) -> Option<Command> {
		if !self.user_action_ready() {
			return None;
		}
		if let Some(key) = action_coalesced_key(&action) {
			let (baseline, desired, label) = match &action {
				Action::Block { user, blocked } => (
					CoalescedValue::Bool(self.confirmed_user_blocked(*user).unwrap_or(!*blocked)),
					CoalescedValue::Bool(*blocked),
					self.user_toast_label(*user),
				),
				Action::Ignore { user, ignored } => (
					CoalescedValue::Bool(self.confirmed_user_ignored(*user).unwrap_or(!*ignored)),
					CoalescedValue::Bool(*ignored),
					self.user_toast_label(*user),
				),
				Action::Mute { channel, muted } => (
					CoalescedValue::Bool(self.confirmed_dm_muted(*channel).unwrap_or(!*muted)),
					CoalescedValue::Bool(*muted),
					self.channel_toast_label(*channel),
				),
				Action::Note { user, text } => (
					CoalescedValue::Text(
						self.user_actions
							.note
							.as_ref()
							.filter(|(id, _)| id == user)
							.map(|(_, t)| t.clone())
							.unwrap_or_default(),
					),
					CoalescedValue::Text(text.clone()),
					self.user_toast_label(*user),
				),
				Action::Nickname { user, text } => (
					CoalescedValue::Text(
						self.user_actions
							.nicknames
							.get(user)
							.cloned()
							.unwrap_or_default(),
					),
					CoalescedValue::Text(text.clone()),
					self.user_toast_label(*user),
				),
				_ => unreachable!(),
			};
			let outcome = self.user_actions.coalesced.set(key, baseline, desired);
			if outcome != crate::optimistic::SetOutcome::Full
				&& matches!(key, CoalescedKey::Note(_) | CoalescedKey::Nickname(_))
			{
				self.user_actions
					.failed_contact_edits
					.retain(|(target, _)| *target != key);
			}
			match outcome {
				crate::optimistic::SetOutcome::Full => {
					self.user_actions.status =
						Some("Too many pending settings updates; wait a moment");
					return None;
				}
				crate::optimistic::SetOutcome::Queued { cancel_in_flight } => {
					if matches!(key, CoalescedKey::Mute(_)) {
						self.revision = self.revision.wrapping_add(1);
					} else {
						self.user_actions.bump_view();
					}
					self.user_actions.status = None;
					return cancel_in_flight.map(|request| Command::CancelPendingWrite {
						kind: crate::optimistic::WriteKind::UserAction,
						request,
					});
				}
				crate::optimistic::SetOutcome::Dispatch(seq) => {
					self.user_actions.coalesced_labels.insert(key, label);
					self.user_actions.coalesced_observed.remove(&key);
					if matches!(key, CoalescedKey::Mute(_)) {
						self.revision = self.revision.wrapping_add(1);
					} else {
						self.user_actions.bump_view();
					}
					self.user_actions.status = None;
					return Some(Command::UserAction {
						action,
						request: seq,
						captcha: None,
					});
				}
			}
		}
		// One-shot optimistic transitions.
		match action {
			Action::AddFriend { username } => {
				let rollback = OneShotRollback::AddFriend {
					username: username.clone(),
				};
				let cmd = self.request_one_shot(
					Action::AddFriend {
						username: username.clone(),
					},
					rollback,
				)?;
				self.user_actions
					.failed_add_friends
					.retain(|failed| *failed != username);
				self.user_actions.bump_view();
				Some(cmd)
			}
			Action::ResolveFriend { user, accept } => {
				let entry = self.user_actions.requests.get(&user)?.clone();
				let rollback = OneShotRollback::ResolveFriend {
					user,
					entry: entry.clone(),
					accept,
				};
				let cmd =
					self.request_one_shot(Action::ResolveFriend { user, accept }, rollback)?;
				self.user_actions.requests.remove(&user);
				if accept {
					self.user_actions.friends.insert(user, (entry.0, entry.1));
				}
				self.user_actions.bump_view();
				self.revision = self.revision.wrapping_add(1);
				Some(cmd)
			}
			Action::ProfileFriend { user, friend } => {
				if friend {
					let (record, name) = self.lookup_user_record(user);
					let rollback = OneShotRollback::ProfileFriend {
						user,
						friend: true,
						inserted_request: true,
						removed_friend: None,
					};
					let cmd = self
						.request_one_shot(Action::ProfileFriend { user, friend: true }, rollback)?;
					self.user_actions
						.requests
						.insert(user, (record, name, false));
					self.user_actions.bump_view();
					self.revision = self.revision.wrapping_add(1);
					Some(cmd)
				} else {
					let removed = self.user_actions.friends.get(&user).cloned();
					let nick = self.user_actions.nicknames.get(&user).cloned();
					let since = self.user_actions.friends_since.get(&user).copied();
					let rollback = OneShotRollback::ProfileFriend {
						user,
						friend: false,
						inserted_request: false,
						removed_friend: removed.map(|(u, n)| (u, n, nick, since)),
					};
					let cmd = self.request_one_shot(
						Action::ProfileFriend {
							user,
							friend: false,
						},
						rollback,
					)?;
					self.user_actions.friends.remove(&user);
					self.user_actions.nicknames.remove(&user);
					self.user_actions.friends_since.remove(&user);
					self.user_actions.bump_view();
					self.revision = self.revision.wrapping_add(1);
					Some(cmd)
				}
			}
			Action::CloseDm(channel) => {
				let snapshot = self.channel(channel).cloned().map(Box::new);
				let rollback = OneShotRollback::CloseDm { channel: snapshot };
				let cmd = self.request_one_shot(Action::CloseDm(channel), rollback)?;
				self.channels.retain(|ch| ch.id != channel);
				self.invalidate_navigation();
				if self.selected == Some(channel) {
					self.arrived_home();
				}
				self.revision = self.revision.wrapping_add(1);
				Some(cmd)
			}
			Action::MessageRequest { channel, accept } => {
				let removed_channel = (!accept)
					.then(|| self.channel(channel).cloned().map(Box::new))
					.flatten();
				let rollback = OneShotRollback::MessageRequest {
					channel,
					accept,
					removed_channel,
				};
				let cmd =
					self.request_one_shot(Action::MessageRequest { channel, accept }, rollback)?;
				self.user_actions.message_requests.remove(&channel);
				if !accept {
					self.channels.retain(|ch| ch.id != channel);
					self.invalidate_navigation();
					if self.selected == Some(channel) {
						self.arrived_home();
					}
				}
				self.user_actions.bump_view();
				self.revision = self.revision.wrapping_add(1);
				Some(cmd)
			}
			Action::OpenDm(user) => {
				let origin = (self.selected, self.request, false);
				self.request_one_shot(Action::OpenDm(user), OneShotRollback::OpenDm { origin })
			}
			Action::CreateGroup { recipients } => {
				let origin = (self.selected, self.request);
				self.request_one_shot(
					Action::CreateGroup { recipients },
					OneShotRollback::CreateGroup { origin },
				)
			}
			Action::LoadNote(_)
			| Action::Block { .. }
			| Action::Ignore { .. }
			| Action::Mute { .. }
			| Action::Note { .. }
			| Action::Nickname { .. } => None,
		}
	}
	fn commit_coalesced_user_action(
		&mut self,
		key: CoalescedKey,
		confirmed: CoalescedValue,
	) -> Result<(), &'static str> {
		match key {
			CoalescedKey::Note(user) => {
				if let Some((target, _, obs)) = &mut self.user_actions.note_request
					&& *target == user
				{
					*obs = true;
				}
				if self
					.user_actions
					.note
					.as_ref()
					.is_none_or(|(id, _)| *id == user)
				{
					self.user_actions.note = Some((user, confirmed.into_text()));
				}
			}
			CoalescedKey::Nickname(user) => {
				self.apply_user_action(Event::Nickname {
					user,
					text: confirmed.into_text(),
				})?;
			}
			CoalescedKey::Block(user) => {
				self.store_relationship(user, confirmed.as_bool())?;
			}
			CoalescedKey::Ignore(user) => {
				let ignored = confirmed.as_bool();
				let profile = self.user_actions.friends.get(&user).cloned().or_else(|| {
					self.user_actions
						.requests
						.get(&user)
						.map(|(u, n, _)| (u.clone(), n.clone()))
				});
				self.apply_user_action(Event::Restriction {
					user,
					ignored: ignored.then_some(true),
					profile,
				})?;
			}
			CoalescedKey::Mute(channel) => {
				self.confirm_dm_muted(channel, confirmed.as_bool())?;
			}
		}
		Ok(())
	}
	fn rollback_one_shot(&mut self, entry: OneShotEntry, failure: Option<Failure>) {
		let label = entry.label;
		let toast_failure = failure.filter(|f| !f.ends_session() || *f == Failure::Capacity);
		match entry.rollback {
			OneShotRollback::AddFriend { username } => {
				if self
					.user_actions
					.requests
					.values()
					.any(|(_, name, incoming)| !*incoming && *name == username)
				{
					return;
				}
				if let Some(failure) = toast_failure {
					self.push_write_error(ActionKind::AddFriend, format!("@{username}"), failure);
				}
				let drafts = &mut self.user_actions.failed_add_friends;
				if drafts.len() >= crate::optimistic::MAX_IN_FLIGHT {
					drafts.pop_front();
				}
				drafts.push_back(username);
			}
			OneShotRollback::ResolveFriend {
				user,
				entry: prior,
				accept,
			} => {
				if !entry.observed {
					if accept {
						self.user_actions.friends.remove(&user);
					}
					self.user_actions.requests.entry(user).or_insert(prior);
				}
				if let Some(failure) = toast_failure {
					let action = if accept {
						ActionKind::AcceptFriendRequest
					} else {
						ActionKind::DeclineFriendRequest
					};
					self.push_write_error(action, label, failure);
				}
			}
			OneShotRollback::ProfileFriend {
				user,
				friend,
				inserted_request,
				removed_friend,
			} => {
				if !entry.observed {
					if inserted_request {
						self.user_actions.requests.remove(&user);
					}
					if let Some((record, name, nick, since)) = removed_friend {
						self.user_actions
							.friends
							.entry(user)
							.or_insert((record, name));
						if let Some(nick) = nick {
							self.user_actions.nicknames.entry(user).or_insert(nick);
						}
						if let Some(since) = since {
							self.user_actions.friends_since.entry(user).or_insert(since);
						}
					}
				}
				if let Some(failure) = toast_failure {
					let action = if friend {
						ActionKind::AddFriend
					} else {
						ActionKind::RemoveFriend
					};
					self.push_write_error(action, label, failure);
				}
			}
			OneShotRollback::CloseDm { channel } => {
				if !entry.observed
					&& let Some(ch) = channel
					&& self.channel(ch.id).is_none()
					&& self.channels.len() < crate::MAX_NAV
				{
					self.channels.push(*ch);
					self.invalidate_navigation();
				}
				if let Some(failure) = toast_failure {
					self.push_write_error(ActionKind::CloseDm, label, failure);
				}
			}
			OneShotRollback::MessageRequest {
				channel,
				accept,
				removed_channel,
			} => {
				if !entry.observed {
					self.user_actions.message_requests.insert(channel);
					if let Some(ch) = removed_channel
						&& self.channel(ch.id).is_none()
						&& self.channels.len() < crate::MAX_NAV
					{
						self.channels.push(*ch);
						self.invalidate_navigation();
					}
				}
				if let Some(failure) = toast_failure {
					let action = if accept {
						ActionKind::AcceptMessageRequest
					} else {
						ActionKind::IgnoreMessageRequest
					};
					self.push_write_error(action, label, failure);
				}
			}
			OneShotRollback::OpenDm { .. } => {
				self.user_actions.dm_origin = None;
				self.user_actions.opened_dm = None;
				if let Some(failure) = toast_failure {
					self.push_write_error(ActionKind::OpenDm, label, failure);
				}
			}
			OneShotRollback::CreateGroup { .. } => {
				self.user_actions.group_origin = None;
				self.user_actions.created_group = None;
				if let Some(failure) = toast_failure {
					self.push_write_error(ActionKind::CreateGroup, "", failure);
				}
			}
		}
		self.user_actions.bump_view();
		self.revision = self.revision.wrapping_add(1);
	}
	/// Handles a confirmed pre-wire cancellation of a superseded coalesced user action.
	pub(crate) fn cancel_user_action_pre_wire(&mut self, request: u64) {
		let Some((key, outcome)) = self.user_actions.coalesced.cancelled_by_seq(request) else {
			return;
		};
		match outcome {
			crate::optimistic::Cancelled::Stale => {}
			crate::optimistic::Cancelled::Settled => {
				self.user_actions.coalesced_labels.remove(&key);
				self.user_actions.coalesced_observed.remove(&key);
				if matches!(key, CoalescedKey::Block(_) | CoalescedKey::Nickname(_)) {
					self.user_actions.bump_view();
				}
				self.revision = self.revision.wrapping_add(1);
			}
			crate::optimistic::Cancelled::DispatchFollowUp { seq, value } => {
				self.user_actions.coalesced_observed.remove(&key);
				self.push_follow_up(Command::UserAction {
					action: coalesced_action(key, value),
					request: seq,
					captcha: None,
				});
			}
		}
	}
	/// Aborts all pending account writes on disconnect and rolls back optimistic overlays.
	pub(crate) fn cancel_user_action(&mut self) {
		self.cancel_user_note_read();
		self.user_actions.dm_origin = None;
		self.user_actions.opened_dm = None;
		self.user_actions.group_origin = None;
		self.user_actions.created_group = None;
		self.user_actions.challenge = None;
		let coalesced = self.user_actions.coalesced.cancel_all();
		let had_coalesced = !coalesced.is_empty();
		for (key, entry) in coalesced {
			if matches!(key, CoalescedKey::Note(_) | CoalescedKey::Nickname(_))
				&& entry.desired != entry.confirmed
			{
				self.user_actions
					.keep_failed_contact_edit(key, entry.desired.into_text());
			}
		}
		self.user_actions.coalesced_labels.clear();
		self.user_actions.coalesced_observed.clear();
		let one_shots = self.user_actions.one_shots.cancel_all();
		let had_any = had_coalesced || !one_shots.is_empty();
		for (_, entry) in one_shots {
			self.rollback_one_shot(entry, None);
		}
		if had_any {
			self.user_actions.bump_view();
			self.user_actions.status =
				Some("Outcome unknown · check the official client before retrying");
			self.revision = self.revision.wrapping_add(1);
		}
	}
	pub(crate) fn observe_dm_reopened(&mut self, channel: Id) {
		if let Some(entry) = self
			.user_actions
			.one_shots
			.get_mut(&OneShotKey::CloseDm(channel))
		{
			entry.observed = true;
		}
	}
	pub(crate) fn observe_dm_settings(&mut self, event: &crate::notifications::Event) {
		let affects_dm = match event {
			crate::notifications::Event::Invalidate => true,
			crate::notifications::Event::Settings { entries, replace } => {
				*replace || entries.iter().any(|s| s.guild.is_none())
			}
			_ => false,
		};
		if affects_dm {
			let keys: Vec<CoalescedKey> = self
				.user_actions
				.coalesced
				.iter()
				.filter_map(|(k, _)| matches!(k, CoalescedKey::Mute(_)).then_some(*k))
				.collect();
			for key in keys {
				self.user_actions.coalesced_observed.insert(key);
			}
		}
	}
	/// Applies one account-action event to relationship and challenge state.
	pub(crate) fn apply_user_action(&mut self, event: Event) -> Result<(), &'static str> {
		// Bump before applying: invalid full snapshots can clear previously visible entries.
		if matches!(
			&event,
			Event::Nicknames(_)
				| Event::Nickname { .. }
				| Event::Friends(_)
				| Event::Friend { .. }
				| Event::Restrictions(_)
				| Event::Restriction { .. }
				| Event::Relationships(_)
		) {
			self.user_actions.bump_view();
		}
		match event {
			Event::DmOpened {
				user,
				request,
				result,
			} => {
				let key = OneShotKey::OpenDm(user);
				let Some(entry) = self.user_actions.one_shots.finish(&key, request) else {
					return Ok(());
				};
				if self
					.user_actions
					.challenge
					.as_ref()
					.is_some_and(|(ck, _, seq, _, _)| *ck == key && *seq == request)
				{
					self.user_actions.challenge = None;
				}
				let origin = match entry.rollback {
					OneShotRollback::OpenDm { origin } => origin,
					_ => (self.selected, self.request, false),
				};
				let result = result.and_then(|channel| {
					if channel.id.0 == 0
						|| channel.guild.is_some()
						|| channel.kind != 1
						|| channel.recipients.len() != 1
						|| channel.recipients[0].id != user
						|| channel.recipients[0].webhook
						|| channel.bytes() > 64 * 1024
						|| self.channel(channel.id).is_some_and(|known| {
							known.guild.is_some()
								|| known.kind != 1 || known.recipients.len() != 1
								|| known.recipients[0].id != user
								|| known.recipients[0].webhook
						}) {
						Err(Failure::Ambiguous)
					} else if !self.dm_target_allowed(user, origin.2) {
						Err(Failure::Forbidden)
					} else {
						Ok(channel)
					}
				});
				let channel = match result {
					Ok(channel) => channel,
					Err(failure) => {
						self.rollback_one_shot(entry, Some(failure));
						if failure.ends_session() && failure != Failure::Capacity {
							self.status = failure.label();
							self.fail(failure);
						}
						return Ok(());
					}
				};
				let id = channel.id;
				// Preserve newer Gateway metadata; otherwise use the normal navigation bounds.
				if self.channel(id).is_none() {
					self.apply(crate::Envelope {
						generation: self.generation,
						event: crate::Event::ChannelCreated(*channel),
					});
				}
				if self.channel(id).is_some() {
					self.user_actions.dm_origin = Some(origin);
					self.user_actions.opened_dm = Some((id, user));
					self.user_actions.status = None;
					self.status = "Direct message opened";
				}
			}
			Event::GroupCreated {
				request,
				recipients,
				result,
			} => {
				let key = OneShotKey::CreateGroup(recipients.clone());
				let Some(entry) = self.user_actions.one_shots.finish(&key, request) else {
					return Ok(());
				};
				let origin = match entry.rollback {
					OneShotRollback::CreateGroup { origin } => origin,
					_ => (self.selected, self.request),
				};
				let result = result.and_then(|channel| {
					let holds_everyone = recipients
						.iter()
						.all(|user| channel.recipients.iter().any(|entry| entry.id == *user));
					if channel.id.0 == 0
						|| channel.guild.is_some()
						|| channel.kind != 3
						|| channel.recipients.is_empty()
						|| channel.recipients.len() > MAX_GROUP_RECIPIENTS
						|| !holds_everyone || channel.recipients.iter().any(|entry| entry.webhook)
						|| channel.bytes() > 64 * 1024
						|| self
							.channel(channel.id)
							.is_some_and(|known| known.guild.is_some() || known.kind != 3)
					{
						Err(Failure::Ambiguous)
					} else {
						Ok(channel)
					}
				});
				let channel = match result {
					Ok(channel) => channel,
					Err(failure) => {
						self.rollback_one_shot(entry, Some(failure));
						if failure.ends_session() && failure != Failure::Capacity {
							self.status = failure.label();
							self.fail(failure);
						}
						return Ok(());
					}
				};
				let id = channel.id;
				if self.channel(id).is_none() {
					self.apply(crate::Envelope {
						generation: self.generation,
						event: crate::Event::ChannelCreated(*channel),
					});
				}
				if self.channel(id).is_some() {
					self.user_actions.group_origin = Some(origin);
					self.user_actions.created_group = Some(id);
					self.user_actions.status = None;
					self.status = "Group message opened";
				}
			}
			Event::NoteChanged { user, text } => {
				if user.0 == 0 || !valid_personal_text(&text, false) {
					return Err("Invalid note");
				}
				let mut active = self.user_note(user).is_some();
				if let Some((target, _, observed)) = &mut self.user_actions.note_request
					&& *target == user
				{
					*observed = true;
					active = true;
				}
				let note_key = CoalescedKey::Note(user);
				if self.user_actions.coalesced.entry(&note_key).is_some() {
					self.user_actions
						.coalesced
						.observe_authoritative(&note_key, CoalescedValue::Text(text.clone()));
					self.user_actions.coalesced_observed.insert(note_key);
					active = true;
				}
				if active {
					self.user_actions.note = Some((user, text.as_str().to_owned()));
				}
			}
			Event::NoteLoaded {
				user,
				request,
				result,
			} => {
				let Some((target, sequence, observed)) = self.user_actions.note_request else {
					return Ok(());
				};
				if target != user || sequence != request {
					return Ok(());
				}
				self.user_actions.note_request = None;
				match result {
					Ok(text) if valid_personal_text(&text, false) && !observed => {
						self.user_actions.note = Some((user, text.as_str().to_owned()));
					}
					Err(failure) if failure.ends_session() => self.fail(failure),
					_ => {}
				}
			}
			Event::FriendsSince { entries, replace } => {
				if entries.len() > MAX_RELATIONSHIPS || entries.iter().any(|(id, _)| id.0 == 0) {
					return Err("Invalid friendship dates");
				}
				if replace {
					self.user_actions.friends_since.clear();
				}
				for (user, since) in entries {
					let known = &mut self.user_actions.friends_since;
					if known.contains_key(&user) || known.len() < MAX_RELATIONSHIPS {
						known.insert(user, since);
					}
				}
			}
			Event::Nicknames(entries) => {
				if entries.len() > MAX_RELATIONSHIPS
					|| entries
						.iter()
						.any(|(id, text)| id.0 == 0 || !valid_personal_text(text, true))
				{
					return Err("Invalid friend nicknames");
				}
				self.user_actions.nicknames.clear();
				for (user, text) in entries {
					self.apply_user_action(Event::Nickname { user, text })?;
				}
			}
			Event::Nickname { user, text } => {
				if user.0 == 0 || !valid_personal_text(&text, true) {
					return Err("Invalid friend nickname");
				}
				let nick_key = CoalescedKey::Nickname(user);
				if self.user_actions.coalesced.entry(&nick_key).is_some() {
					self.user_actions
						.coalesced
						.observe_authoritative(&nick_key, CoalescedValue::Text(text.clone()));
					self.user_actions.coalesced_observed.insert(nick_key);
				}
				if text.is_empty() || !self.user_actions.friends.contains_key(&user) {
					self.user_actions.nicknames.remove(&user);
				} else {
					if !self.user_actions.nicknames.contains_key(&user)
						&& self.user_actions.nicknames.len() >= MAX_RELATIONSHIPS
					{
						return Err("Nickname capacity exceeded");
					}
					// At most 4000 tightly allocated 128-byte strings plus map overhead.
					self.user_actions
						.nicknames
						.insert(user, text.as_str().to_owned());
				}
			}
			Event::Requests(entries) => {
				let profile_keys: Vec<OneShotKey> = self
					.user_actions
					.one_shots
					.iter()
					.filter(|&(k, _)| matches!(k, OneShotKey::ProfileFriend(_)))
					.map(|(k, _)| k.clone())
					.collect();
				for key in profile_keys {
					if let Some(entry) = self.user_actions.one_shots.get_mut(&key) {
						entry.observed = true;
					}
				}
				self.user_actions.requests.clear();
				self.user_actions.requests_known = false;
				if let Some(entries) = entries {
					if entries.len() > MAX_RELATIONSHIPS
						|| entries.capacity() * size_of::<(model::User, String, bool)>()
							+ entries
								.iter()
								.map(|(u, n, _)| u.heap_bytes() + n.capacity() + 64)
								.sum::<usize>() > MAX_FRIEND_BYTES
					{
						return Err("Friend requests exceed safe capacity");
					}
					for (user, name, incoming) in entries {
						if self.user_actions.requests.contains_key(&user.id) {
							return Err("Duplicate friend request");
						}
						self.apply_user_action(Event::Request {
							user: user.id,
							incoming: Some(incoming),
							profile: Some((user, name)),
						})?;
					}
					self.user_actions.requests_known = true;
				}
			}
			Event::Request {
				user,
				incoming,
				profile,
			} => {
				if user.0 == 0 {
					return Err("Invalid friend request");
				}
				let resolve_already_observed = self
					.user_actions
					.one_shots
					.get(&OneShotKey::ResolveFriend(user))
					.is_some_and(|e| e.observed);
				for key in [
					OneShotKey::ResolveFriend(user),
					OneShotKey::ProfileFriend(user),
				] {
					if let Some(entry) = self.user_actions.one_shots.get_mut(&key) {
						entry.observed = true;
					}
				}
				if let Some(incoming) = incoming {
					let profile = profile.or_else(|| {
						(!self.user_actions.requests.contains_key(&user)).then(|| {
							(
								model::User {
									id: user,
									name: "Unknown user".into(),
									avatar: None,
									discriminator: 0,
									primary_guild: None,
									cosmetics: None,
									webhook: false,
									kind: Default::default(),
								},
								format!("User ID: {user}"),
							)
						})
					});
					if let Some((record, name)) = profile {
						if user != record.id || !valid_friend(&record, &name) {
							return Err("Invalid friend request profile");
						}
						let entries = &mut self.user_actions.requests;
						let bytes = entries
							.iter()
							.filter(|(id, _)| **id != user)
							.map(|(_, (u, n, _))| {
								u.heap_bytes()
									+ n.capacity() + size_of::<(Id, model::User, String, bool)>()
									+ 64
							})
							.sum::<usize>();
						if (!entries.contains_key(&user) && entries.len() >= MAX_RELATIONSHIPS)
							|| bytes
								+ record.heap_bytes() + name.capacity()
								+ size_of::<(Id, model::User, String, bool)>()
								+ 64 > MAX_FRIEND_BYTES
						{
							return Err("Friend requests exceed safe capacity");
						}
						entries.insert(user, (record, name, incoming));
					} else if let Some(entry) = self.user_actions.requests.get_mut(&user) {
						entry.2 = incoming;
					}
				} else {
					if let Some(entry) = self
						.user_actions
						.one_shots
						.get(&OneShotKey::ResolveFriend(user))
						&& !resolve_already_observed
						&& matches!(entry.action, Action::ResolveFriend { accept: true, .. })
					{
						self.user_actions.friends.remove(&user);
						self.user_actions.bump_view();
					}
					self.user_actions.spam_requests.remove(&user);
					if let Some((_, name, _)) = self.user_actions.requests.remove(&user)
						&& self.user_actions.last_requested.as_deref() == Some(&name)
					{
						self.user_actions.last_requested = None;
					}
				}
			}
			Event::FriendProfile(profile) => {
				if let Some((_, _, ignored)) = self.user_actions.restricted.get(&profile.0.id) {
					let ignored = *ignored;
					let user = profile.0.id;
					self.apply_user_action(Event::Restriction {
						user,
						ignored: Some(ignored),
						profile: Some(profile.clone()),
					})?;
				}
				let user = profile.0.id;
				if self.user_actions.friends.contains_key(&user)
					&& self
						.user_actions
						.one_shots
						.get(&OneShotKey::ResolveFriend(user))
						.is_none_or(|entry| entry.pending_friendship.is_none())
				{
					self.apply_user_action(Event::Friend {
						user,
						friend: true,
						profile: Some(profile),
					})?;
					return Ok(());
				}
			}
			Event::Friends(entries) => {
				let profile_keys: Vec<OneShotKey> = self
					.user_actions
					.one_shots
					.iter()
					.filter(|&(k, entry)| {
						matches!(k, OneShotKey::ProfileFriend(_))
							|| entry.pending_friendship.is_some()
					})
					.map(|(k, _)| k.clone())
					.collect();
				let mut pending_friendships = BTreeMap::new();
				for key in profile_keys {
					if let Some(entry) = self.user_actions.one_shots.get_mut(&key) {
						entry.observed = true;
						if let Some((user, previous_membership)) = entry.pending_friendship.take() {
							pending_friendships.insert(user, previous_membership);
						}
					}
				}
				// Profile Privacy depends on friendship: drop cached profiles of anyone the
				// snapshot adds or removes, including when it is rejected or unknown.
				let previous = std::mem::take(&mut self.user_actions.friends);
				let result = self.replace_friends(entries);
				let friends = &self.user_actions.friends;
				let changed = |user: &Id| {
					pending_friendships
						.get(user)
						.copied()
						.unwrap_or_else(|| previous.contains_key(user))
						!= friends.contains_key(user)
				};
				if previous
					.keys()
					.chain(friends.keys())
					.chain(pending_friendships.keys())
					.any(changed)
				{
					// A profile preload in flight may carry the old friendship's view.
					self.preload.profiles.touch(None);
				}
				let open = self
					.profile
					.as_ref()
					.map(|view| view.user)
					.filter(|user| changed(user));
				self.profile_cache.retain_users(|user| !changed(&user));
				if let Some(user) = open {
					self.refresh_open_profile(user);
				}
				return result;
			}
			Event::Friend {
				user,
				friend,
				profile,
			} => {
				let mut previous_membership = None;
				for key in [
					OneShotKey::ProfileFriend(user),
					OneShotKey::ResolveFriend(user),
				] {
					if let Some(entry) = self.user_actions.one_shots.get_mut(&key) {
						entry.observed = true;
						if let Some((_, was_friend)) = entry.pending_friendship.take() {
							previous_membership = Some(was_friend);
						}
					}
				}
				if previous_membership
					.unwrap_or_else(|| self.user_actions.friends.contains_key(&user))
					!= friend
				{
					self.invalidate_profile_for_friendship_change(user);
				}
				let profile = profile.or_else(|| {
					self.user_actions
						.requests
						.get(&user)
						.map(|(u, n, _)| (u.clone(), n.clone()))
				});
				if !friend {
					let nick_key = CoalescedKey::Nickname(user);
					if self.user_actions.coalesced.entry(&nick_key).is_some() {
						self.user_actions.coalesced_observed.insert(nick_key);
					}
					self.user_actions.friends.remove(&user);
					self.user_actions.nicknames.remove(&user);
					self.user_actions.friends_since.remove(&user);
				} else if let Some((record, name)) = profile {
					let entries = &mut self.user_actions.friends;
					if user != record.id || !valid_friend(&record, &name) {
						return Err("Invalid friend update");
					}
					let old = entries.get(&user).map_or(0, |(u, n)| {
						u.heap_bytes() + n.capacity() + size_of::<(Id, model::User, String)>() + 64
					});
					let bytes: usize = entries
						.values()
						.map(|(u, n)| {
							u.heap_bytes()
								+ n.capacity() + size_of::<(Id, model::User, String)>()
								+ 64
						})
						.sum();
					if (!entries.contains_key(&user) && entries.len() >= MAX_RELATIONSHIPS)
						|| bytes - old
							+ record.heap_bytes() + name.capacity()
							+ size_of::<(Id, model::User, String)>()
							+ 64 > MAX_FRIEND_BYTES
					{
						return Err("Friends exceed safe capacity");
					}
					entries.insert(user, (record, name));
					self.user_actions.requests.remove(&user);
				}
			}
			Event::Restrictions(entries) => {
				self.user_actions.restricted.clear();
				self.user_actions.ignored.clear();
				self.user_actions.restricted_known = false;
				if let Some(entries) = entries {
					if entries.len() > MAX_RELATIONSHIPS
						|| entries.capacity() * size_of::<(model::User, String, bool)>()
							+ entries
								.iter()
								.map(|(u, n, _)| u.heap_bytes() + n.capacity() + 64)
								.sum::<usize>() > MAX_RESTRICTED_BYTES
					{
						return Err("Restricted users exceed safe capacity");
					}
					for (user, name, ignored) in entries {
						if !valid_friend(&user, &name)
							|| self
								.user_actions
								.restricted
								.insert(user.id, (user, name, ignored))
								.is_some()
						{
							self.user_actions.restricted.clear();
							self.user_actions.ignored.clear();
							return Err("Restricted users contain invalid or duplicate profiles");
						}
					}
					self.user_actions.ignored = self
						.user_actions
						.restricted
						.iter()
						.map(|(id, entry)| (*id, entry.2))
						.collect();
					self.user_actions.restricted_known = true;
				}
			}
			Event::Restriction {
				user,
				ignored,
				profile,
			} => {
				let ign_key = CoalescedKey::Ignore(user);
				if self.user_actions.coalesced.entry(&ign_key).is_some() {
					self.user_actions.coalesced.observe_authoritative(
						&ign_key,
						CoalescedValue::Bool(ignored.unwrap_or(false)),
					);
					self.user_actions.coalesced_observed.insert(ign_key);
				}
				let Some(ignored) = ignored else {
					self.store_ignored(user, false)?;
					self.user_actions.restricted.remove(&user);
					return Ok(());
				};
				if let Some((record, name)) = profile {
					if user != record.id || !valid_friend(&record, &name) {
						return Err("Invalid restricted user update");
					}
					let entries = &mut self.user_actions.restricted;
					let old = entries.get(&user).map_or(0, |(u, n, _)| {
						u.heap_bytes()
							+ n.capacity() + size_of::<(Id, model::User, String, bool)>()
							+ 64
					});
					let bytes: usize = entries
						.values()
						.map(|(u, n, _)| {
							u.heap_bytes()
								+ n.capacity() + size_of::<(Id, model::User, String, bool)>()
								+ 64
						})
						.sum();
					if (!entries.contains_key(&user) && entries.len() >= MAX_RELATIONSHIPS)
						|| bytes - old
							+ record.heap_bytes() + name.capacity()
							+ size_of::<(Id, model::User, String, bool)>()
							+ 64 > MAX_RESTRICTED_BYTES
					{
						return Err("Restricted users exceed safe capacity");
					}
					entries.insert(user, (record, name, ignored));
				} else if let Some(entry) = self.user_actions.restricted.get_mut(&user) {
					entry.2 = ignored;
				}
				self.store_ignored(user, ignored)?;
			}
			Event::Relationships(entries) => {
				let block_keys: Vec<CoalescedKey> = self
					.user_actions
					.coalesced
					.iter()
					.filter_map(|(k, _)| matches!(k, CoalescedKey::Block(_)).then_some(*k))
					.collect();
				for key in block_keys {
					self.user_actions.coalesced_observed.insert(key);
				}
				self.user_actions.relationships.clear();
				self.user_actions.known = false;
				if let Some(entries) = entries {
					if entries.len() > MAX_RELATIONSHIPS
						|| entries.capacity() * size_of::<(Id, bool)>() > MAX_RELATIONSHIP_BYTES
					{
						return Err("Relationships exceed safe capacity");
					}
					for (user, blocked) in entries {
						if user.0 == 0
							|| self
								.user_actions
								.relationships
								.insert(user, blocked)
								.is_some()
						{
							self.user_actions.relationships.clear();
							return Err("Relationships contain invalid or duplicate users");
						}
					}
					self.user_actions.known = true;
				}
			}
			Event::MessageRequests(entries) => {
				replace_id_set(
					&mut self.user_actions.message_requests,
					entries,
					Some(&mut self.user_actions.spam_directs),
					"Message requests exceed safe capacity",
					"Message requests contain invalid or duplicate channels",
				)?;
				self.user_actions.bump_view();
			}
			Event::MessageRequest { channel, pending } => {
				if let Some(entry) = self
					.user_actions
					.one_shots
					.get_mut(&OneShotKey::MessageRequest(channel))
				{
					entry.observed = true;
				}
				self.set_message_request(channel, pending)?;
			}
			Event::MessageSpams(entries) => {
				replace_id_set(
					&mut self.user_actions.spam_directs,
					entries,
					Some(&mut self.user_actions.message_requests),
					"Message requests exceed safe capacity",
					"Message requests contain invalid or duplicate channels",
				)?;
			}
			Event::MessageSpam { channel, spam } => {
				self.set_spam_direct(channel, spam)?;
			}
			Event::RequestSpams(entries) => {
				replace_id_set(
					&mut self.user_actions.spam_requests,
					entries,
					None,
					"Friend requests exceed safe capacity",
					"Friend requests contain invalid or duplicate users",
				)?;
			}
			Event::RequestSpam { user, spam } => {
				self.set_spam_request(user, spam)?;
			}
			Event::Relationship { user, blocked } => {
				self.store_relationship(user, blocked)?;
				let blk_key = CoalescedKey::Block(user);
				if self.user_actions.coalesced.entry(&blk_key).is_some() {
					self.user_actions
						.coalesced
						.observe_authoritative(&blk_key, CoalescedValue::Bool(blocked));
					self.user_actions.coalesced_observed.insert(blk_key);
				}
				if let Some(entry) = self
					.user_actions
					.one_shots
					.get_mut(&OneShotKey::ProfileFriend(user))
				{
					entry.observed = true;
				}
			}
			Event::Challenge {
				action,
				request,
				challenge,
			} => {
				let Some(key) = action_one_shot_key(&action) else {
					return Ok(());
				};
				if !challengeable(&action) {
					return Ok(());
				}
				// Verify this exact one-shot attempt is active with matching sequence.
				let matches = self.user_actions.one_shots.seq(&key) == Some(request)
					&& self
						.user_actions
						.one_shots
						.get(&key)
						.is_some_and(|entry| entry.action == action);
				if !matches {
					return Ok(());
				}
				if self.user_actions.challenge.as_ref().is_some_and(
					|(active_key, _, active_seq, _, _)| {
						*active_key != key || *active_seq != request
					},
				) {
					if let Some(entry) = self.user_actions.one_shots.finish(&key, request) {
						self.rollback_one_shot(
							entry,
							Some(Failure::ProtocolAt(
								"Finish the active verification check first",
							)),
						);
					}
					return Ok(());
				}
				self.user_actions.status = None;
				self.user_actions.challenge =
					Some((key, action, request, std::time::Instant::now(), *challenge));
			}
			Event::Written {
				action,
				request,
				result,
			} => {
				if let Action::LoadNote(user) = action {
					return self.apply_user_action(Event::NoteLoaded {
						user,
						request,
						result: Err(result.err().unwrap_or(Failure::Protocol)),
					});
				}
				if let Some(key) = action_coalesced_key(&action) {
					let contact_draft = if result.is_err()
						&& matches!(key, CoalescedKey::Note(_) | CoalescedKey::Nickname(_))
					{
						self.user_actions.coalesced.entry(&key).and_then(|entry| {
							(entry.desired != entry.confirmed)
								.then(|| entry.desired.as_text().to_owned())
						})
					} else {
						None
					};
					let finish = self.user_actions.coalesced.finish(&key, request, result);
					match finish {
						crate::optimistic::Finish::Stale => return Ok(()),
						crate::optimistic::Finish::Settled { confirmed } => {
							let observed = self.user_actions.coalesced_observed.remove(&key);
							self.user_actions.coalesced_labels.remove(&key);
							if !observed {
								self.commit_coalesced_user_action(key, confirmed)?;
							}
							if matches!(key, CoalescedKey::Mute(_)) {
								self.revision = self.revision.wrapping_add(1);
							} else {
								self.user_actions.bump_view();
							}
							self.user_actions.status = None;
						}
						crate::optimistic::Finish::DispatchFollowUp { seq, value } => {
							let observed = self.user_actions.coalesced_observed.remove(&key);
							if !observed
								&& let Some(confirmed) = self
									.user_actions
									.coalesced
									.entry(&key)
									.map(|e| e.confirmed.clone())
							{
								self.commit_coalesced_user_action(key, confirmed)?;
							}
							self.user_actions.coalesced_observed.remove(&key);
							self.push_follow_up(Command::UserAction {
								action: coalesced_action(key, value),
								request: seq,
								captcha: None,
							});
						}
						crate::optimistic::Finish::RolledBack { failure, .. } => {
							if let Some(draft) = contact_draft {
								self.user_actions.keep_failed_contact_edit(key, draft);
							}
							self.user_actions.coalesced_observed.remove(&key);
							let label = self
								.user_actions
								.coalesced_labels
								.remove(&key)
								.unwrap_or_else(|| "this user".into());
							if matches!(key, CoalescedKey::Mute(_)) {
								self.revision = self.revision.wrapping_add(1);
							} else {
								self.user_actions.bump_view();
							}
							if failure.ends_session() && failure != Failure::Capacity {
								self.status = failure.label();
								self.fail(failure);
							} else {
								let action = match key {
									CoalescedKey::Block(_) => ActionKind::Block,
									CoalescedKey::Ignore(_) => ActionKind::Ignore,
									CoalescedKey::Mute(_) => ActionKind::Mute,
									CoalescedKey::Note(_) => ActionKind::SaveNote,
									CoalescedKey::Nickname(_) => ActionKind::SaveNickname,
								};
								self.push_write_error(action, label, failure);
							}
						}
					}
					return Ok(());
				}
				let Some(key) = action_one_shot_key(&action) else {
					return Ok(());
				};
				let Some(entry) = self.user_actions.one_shots.finish(&key, request) else {
					return Ok(());
				};
				if self
					.user_actions
					.challenge
					.as_ref()
					.is_some_and(|(ck, _, seq, _, _)| *ck == key && *seq == request)
				{
					self.user_actions.challenge = None;
				}
				if let Err(failure) = result {
					self.rollback_one_shot(entry, Some(failure));
					if failure.ends_session() && failure != Failure::Capacity {
						self.status = failure.label();
						self.fail(failure);
					}
					return Ok(());
				}
				if let Some((user, was_friend)) = entry.pending_friendship
					&& self.user_actions.friends.contains_key(&user) != was_friend
				{
					self.invalidate_profile_for_friendship_change(user);
				}
				self.user_actions.status = None;
				match action {
					Action::AddFriend { username } => {
						self.user_actions.last_requested = Some(username);
					}
					Action::ResolveFriend { .. } => {
						self.user_actions.last_requested = None;
					}
					Action::CloseDm(channel) => {
						if !entry.observed {
							self.remove_channels(&std::collections::BTreeSet::from([channel]));
						}
					}
					Action::MessageRequest { channel, accept } => {
						if !accept && !entry.observed {
							self.remove_channels(&std::collections::BTreeSet::from([channel]));
						}
					}
					Action::OpenDm(_) => {
						// OpenDm returning Written(Ok) means Discord didn't return a channel object;
						// treat as ambiguous and release origin.
						self.rollback_one_shot(entry, Some(Failure::Ambiguous));
					}
					Action::CreateGroup { .. } => {
						self.rollback_one_shot(entry, Some(Failure::Ambiguous));
					}
					_ => {}
				}
				self.user_actions.bump_view();
				self.revision = self.revision.wrapping_add(1);
			}
		}
		Ok(())
	}
	fn invalidate_profile_for_friendship_change(&mut self, user: Id) {
		self.profile_cache.forget(user);
		self.preload.profiles.touch(None);
		self.refresh_open_profile(user);
	}
	fn store_relationship(&mut self, user: Id, blocked: bool) -> Result<(), &'static str> {
		self.user_actions.bump_view();
		if blocked {
			self.user_actions.friends.remove(&user);
			self.user_actions.nicknames.remove(&user);
			self.user_actions.requests.remove(&user);
		}
		let entries = &mut self.user_actions.relationships;
		// Fixed-size IDs and booleans: <= 4000 entries and a conservative 32-byte entry estimate.
		if user.0 == 0
			|| (!entries.contains_key(&user)
				&& (entries.len() >= MAX_RELATIONSHIPS
					|| (entries.len() + 1) * 32 > MAX_RELATIONSHIP_BYTES))
		{
			return Err("Relationships exceed safe capacity or contain an invalid user");
		}
		entries.insert(user, blocked);
		self.read_state.activity.clear_notifications();
		Ok(())
	}
}

pub fn valid_personal_text(text: &str, nickname: bool) -> bool {
	let limit = if nickname { 32 } else { 256 };
	text.len() <= limit * 4
		&& text.chars().count() <= limit
		&& !text
			.chars()
			.any(|c| c.is_control() && (nickname || !matches!(c, '\n' | '\t')))
}
pub fn valid_username(name: &str) -> bool {
	(2..=32).contains(&name.len())
		&& !name.contains("..")
		&& name
			.bytes()
			.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.'))
}
fn valid_friend(user: &model::User, username: &str) -> bool {
	user.id.0 != 0
		&& !user.name.is_empty()
		&& user.name.len() <= 512
		&& !user.name.chars().any(char::is_control)
		&& !username.is_empty()
		&& username.len() <= 128
		&& !username.chars().any(char::is_control)
		&& user
			.avatar
			.as_ref()
			.is_none_or(|hash| model::valid_avatar_hash(hash))
}

#[cfg(debug_assertions)]
impl State {
	/// Offline state check; preserves the caller's action state and sends no commands.
	pub fn debug_profile_note_read_check(&mut self, user: &model::User) {
		let saved = std::mem::take(&mut self.user_actions);
		let allowed = self.can_open_user_dm(user);
		let Command::UserAction { request, .. } = self.load_user_note(user.id).unwrap() else {
			panic!("note read");
		};
		assert!(
			!self.user_action_pending(),
			"note read never reserves the account-write slot"
		);
		assert_eq!(self.can_open_user_dm(user), allowed);
		let write = self
			.request_user_action(Action::Note {
				user: user.id,
				text: "New note".into(),
			})
			.expect("write can run alongside note read");
		self.apply_user_action(Event::NoteChanged {
			user: user.id,
			text: "New note".into(),
		})
		.unwrap();
		self.apply_user_action(Event::NoteLoaded {
			user: user.id,
			request,
			result: Ok("Old note".into()),
		})
		.unwrap();
		assert_eq!(self.user_note(user.id), Some("New note"));
		assert!(
			self.user_action_pending(),
			"read completion cannot clear the pending write"
		);
		let Command::UserAction {
			action, request, ..
		} = write
		else {
			panic!("note write");
		};
		self.apply_user_action(Event::Written {
			action,
			request,
			result: Ok(()),
		})
		.unwrap();
		assert!(!self.user_action_pending());
		let Command::UserAction { request, .. } = self.load_user_note(user.id).unwrap() else {
			panic!("second read");
		};
		self.cancel_user_note_read();
		self.apply_user_action(Event::NoteLoaded {
			user: user.id,
			request,
			result: Ok("Cancelled read".into()),
		})
		.unwrap();
		assert_eq!(self.user_note(user.id), Some("New note"));
		self.user_actions = saved;
		println!("Profile note read isolation, newer-note reconciliation and cancellation passed.");
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Envelope, Event as CoreEvent};

	fn stranger_dm() -> (State, model::User, model::Channel) {
		let mut state = state();
		state
			.apply_user_action(Event::Relationships(Some(vec![])))
			.unwrap();
		let mut channel = state.channels[0].clone();
		channel.id = Id(30);
		channel.recipients[0].id = Id(3);
		let user = channel.recipients[0].clone();
		(state, user, channel)
	}

	fn open_request(command: Command, user: Id) -> u64 {
		let Command::UserAction {
			action: Action::OpenDm(target),
			request,
			..
		} = command
		else {
			panic!("expected DM open request")
		};
		assert_eq!(target, user);
		request
	}

	fn opened(state: &mut State, user: Id, request: u64, result: Result<model::Channel, Failure>) {
		state.apply(Envelope {
			generation: state.generation,
			event: CoreEvent::UserAction(Event::DmOpened {
				user,
				request,
				result: result.map(Box::new),
			}),
		});
	}

	#[test]
	fn a_group_request_names_every_friend_once_and_admits_the_created_channel() {
		let mut state = state();
		state
			.apply_user_action(Event::Relationships(Some(vec![])))
			.unwrap();
		let people: Vec<model::User> = (0..3)
			.map(|index| {
				let mut user = state.channels[0].recipients[0].clone();
				user.id = Id(10 + index);
				user.name = format!("Friend {index}");
				user
			})
			.collect();
		state
			.apply_user_action(Event::Friends(Some(
				people
					.iter()
					.map(|user| (user.clone(), "synthetic.handle".into()))
					.collect(),
			)))
			.unwrap();
		// A group needs two people, and everyone has to be a known friend.
		assert!(state.create_group_dm(&[]).is_none());
		assert!(state.create_group_dm(&[people[0].id]).is_none());
		assert!(state.create_group_dm(&[people[0].id, Id(999)]).is_none());
		// One request carries each chosen friend once, whatever order the picker listed them in.
		let Some(Command::UserAction {
			action: Action::CreateGroup { recipients },
			request,
			..
		}) = state.create_group_dm(&[people[2].id, people[0].id, people[2].id])
		else {
			panic!("expected one group request");
		};
		assert_eq!(recipients, vec![people[0].id, people[2].id]);
		let mut channel = state.channels[0].clone();
		channel.id = Id(40);
		channel.kind = 3;
		channel.name = "Synthetic group".into();
		channel.recipients = vec![people[0].clone(), people[2].clone()];
		state.apply(Envelope {
			generation: state.generation,
			event: CoreEvent::UserAction(Event::GroupCreated {
				request,
				result: Ok(Box::new(channel.clone())),
				recipients: recipients.clone(),
			}),
		});
		assert_eq!(state.channel(Id(40)).map(|known| known.kind), Some(3));
		assert!(state.user_action_pending().eq(&false));
		// The admitted group is opened once, and only while the reader stayed where they were.
		assert!(state.select_created_group().is_some());
		assert_eq!(state.selected, Some(Id(40)));
		assert!(state.select_created_group().is_none());
	}

	#[test]
	fn nonfriend_dm_opens_only_after_confirmation_and_preserves_drafts() {
		let (mut state, user, channel) = stranger_dm();
		state
			.drafts
			.insert(Id(10), "Existing conversation draft".into());
		state.drafts.insert(channel.id, "Destination draft".into());
		let drafts = state.drafts.clone();
		assert!(state.open_friend_dm(user.id).is_none());
		assert!(state.can_open_user_dm(&user));
		let request = open_request(state.open_user_dm(&user).unwrap(), user.id);
		assert_eq!(state.selected, Some(Id(10)));
		assert!(!state.can_open_user_dm(&user));
		assert!(state.open_user_dm(&user).is_none());
		opened(&mut state, user.id, request + 1, Ok(channel.clone()));
		assert!(state.user_action_pending());
		assert!(state.channel(channel.id).is_none());
		opened(&mut state, Id(999), request, Ok(channel.clone()));
		assert!(state.user_action_pending());
		opened(&mut state, user.id, request, Ok(channel.clone()));
		assert_eq!(state.selected, Some(Id(10)));
		assert!(!state.user_action_pending());
		assert!(
			matches!(state.select_opened_dm(), Some(Command::History { channel: id, .. }) if id == channel.id)
		);
		assert_eq!(state.selected, Some(channel.id));
		assert_eq!(state.drafts, drafts);
		assert!(state.select_opened_dm().is_none());
		state.select(Id(10));
		assert!(
			matches!(state.open_user_dm(&user), Some(Command::History { channel: id, .. }) if id == channel.id)
		);
		assert_eq!(
			state.channels.iter().filter(|c| c.id == channel.id).count(),
			1
		);
		assert_eq!(state.drafts, drafts);
	}

	#[test]
	fn nonfriend_dm_rejects_invalid_targets_and_unavailable_actions() {
		let (mut state, user, _) = stranger_dm();
		for invalid in [
			model::User {
				id: Id(0),
				..user.clone()
			},
			state.user.clone().unwrap(),
			model::User {
				webhook: true,
				..user.clone()
			},
		] {
			assert!(!state.can_open_user_dm(&invalid));
			assert!(state.open_user_dm(&invalid).is_none());
		}
		state.apply_user_action(Event::Relationships(None)).unwrap();
		assert!(state.open_user_dm(&user).is_none());
		state
			.apply_user_action(Event::Relationships(Some(vec![(user.id, true)])))
			.unwrap();
		assert!(state.open_user_dm(&user).is_none());
		state
			.apply_user_action(Event::Relationships(Some(vec![])))
			.unwrap();
		state.gateway_connected = false;
		assert!(state.open_user_dm(&user).is_none());
		state.gateway_connected = true;
		state.auth = AuthState::Unauthenticated;
		assert!(state.open_user_dm(&user).is_none());
	}

	#[test]
	fn nonfriend_dm_rechecks_returned_identity_relationship_and_session() {
		for outcome in 0..8 {
			let (mut state, user, mut channel) = stranger_dm();
			state.drafts.insert(Id(10), "Keep my draft".into());
			let drafts = state.drafts.clone();
			let request = open_request(state.open_user_dm(&user).unwrap(), user.id);
			match outcome {
				0 => channel.recipients[0].id = Id(999),
				1 => channel.recipients[0].webhook = true,
				2 => state
					.apply_user_action(Event::Relationship {
						user: user.id,
						blocked: true,
					})
					.unwrap(),
				3 => state.apply_user_action(Event::Relationships(None)).unwrap(),
				4 => state.gateway_connected = false,
				5 => state.auth = AuthState::Unauthenticated,
				6 => channel.id = Id(10), // Existing channel belongs to a different recipient.
				_ => {}
			}
			let id = channel.id;
			opened(
				&mut state,
				user.id,
				request,
				if outcome == 7 {
					Err(Failure::Forbidden)
				} else {
					Ok(channel)
				},
			);
			assert!(state.select_opened_dm().is_none());
			assert_eq!(state.selected, Some(Id(10)));
			assert!(!state.user_action_pending());
			if id != Id(10) {
				assert!(state.channel(id).is_none());
			}
			assert_eq!(state.drafts, drafts);
		}
	}

	#[test]
	fn nonfriend_dm_does_not_override_navigation_or_accept_old_generations() {
		let (mut state, user, channel) = stranger_dm();
		let request = open_request(state.open_user_dm(&user).unwrap(), user.id);
		state.apply(Envelope {
			generation: state.generation.wrapping_sub(1),
			event: CoreEvent::UserAction(Event::DmOpened {
				user: user.id,
				request,
				result: Ok(Box::new(channel.clone())),
			}),
		});
		assert!(state.user_action_pending());
		assert!(state.channel(channel.id).is_none());
		// Even returning to the same conversation must not revive an earlier navigation intent.
		state.request = state.request.wrapping_add(1);
		opened(&mut state, user.id, request, Ok(channel.clone()));
		assert!(state.channel(channel.id).is_some());
		assert!(state.select_opened_dm().is_none());
		assert_eq!(state.selected, Some(Id(10)));
		let (mut state, user, channel) = stranger_dm();
		let request = open_request(state.open_user_dm(&user).unwrap(), user.id);
		opened(&mut state, user.id, request, Ok(channel));
		state
			.apply_user_action(Event::Relationship {
				user: user.id,
				blocked: true,
			})
			.unwrap();
		assert!(state.select_opened_dm().is_none());
		assert_eq!(state.selected, Some(Id(10)));
	}

	#[test]
	fn existing_friend_dm_stays_navigable_while_reconnecting() {
		let (mut state, user, channel) = stranger_dm();
		state.channels.push(channel.clone());
		state
			.apply_user_action(Event::Friends(Some(vec![(
				user.clone(),
				"synthetic".into(),
			)])))
			.unwrap();
		state.gateway_connected = false;
		assert!(
			matches!(state.open_friend_dm(user.id), Some(Command::History { channel: id, .. }) if id == channel.id)
		);
		assert_eq!(state.selected, Some(channel.id));
		state.channels.retain(|known| known.id != channel.id);
		state.select(Id(10));
		assert!(state.open_friend_dm(user.id).is_none());
		assert!(!state.user_action_pending());
	}

	#[test]
	fn friend_only_dm_still_requires_friendship_until_selection() {
		for removed_after_response in [false, true] {
			let (mut state, user, channel) = stranger_dm();
			state
				.apply_user_action(Event::Friends(Some(vec![(
					user.clone(),
					"synthetic".into(),
				)])))
				.unwrap();
			let request = open_request(state.open_friend_dm(user.id).unwrap(), user.id);
			if removed_after_response {
				opened(&mut state, user.id, request, Ok(channel.clone()));
			}
			state
				.apply_user_action(Event::Friend {
					user: user.id,
					friend: false,
					profile: None,
				})
				.unwrap();
			if !removed_after_response {
				opened(&mut state, user.id, request, Ok(channel.clone()));
				assert!(state.channel(channel.id).is_none());
			}
			assert!(state.select_opened_dm().is_none());
			assert_eq!(state.selected, Some(Id(10)));
			assert!(!state.user_action_pending());
		}
	}
	#[test]
	fn relationship_view_tracks_all_friend_inputs_and_failed_mutations() {
		{
			let mut state = state();
			let mut user = state.channels[0].recipients[0].clone();
			let mut view = state.relationship_view();
			for event in [
				Event::Relationships(Some(vec![])),
				Event::Friends(Some(vec![(user.clone(), "friend".into())])),
				Event::Nickname {
					user: user.id,
					text: "Nickname".into(),
				},
				Event::Nicknames(vec![]),
				{
					user.name = "Changed display name".into();
					Event::FriendProfile((user.clone(), "changed_username".into()))
				},
			] {
				state.apply_user_action(event).unwrap();
				assert!(state.relationship_view() > view);
				view = state.relationship_view();
			}
			assert!(state.friend(user.id) == state.friends().next());
			assert_eq!(state.friend(user.id).unwrap().name, "Changed display name");
			assert!(state.friend(Id(999)).is_none());
			for cancelled in [false, true] {
				let command = state.set_user_blocked(user.id, true).unwrap();
				assert!(state.relationship_view() > view);
				assert!(state.friend(user.id).is_none());
				view = state.relationship_view();
				if cancelled {
					state.cancel_user_action();
				} else {
					finish(&mut state, command, Err(Failure::Forbidden));
				}
				assert!(state.relationship_view() > view);
				assert!(state.friend(user.id) == state.friends().next());
				assert!(state.friend(user.id).is_some());
				view = state.relationship_view();
			}
			// A rejected full snapshot clears existing values before reporting the error.
			assert!(
				state
					.apply_user_action(Event::Friends(Some(vec![
						(user.clone(), "friend".into()),
						(user.clone(), "duplicate".into()),
					])))
					.is_err()
			);
			assert!(state.relationship_view() > view);
			assert!(state.friend(user.id).is_none());
			state
				.apply_user_action(Event::Friends(Some(vec![(user.clone(), "friend".into())])))
				.unwrap();
			view = state.relationship_view();
			assert!(
				state
					.apply_user_action(Event::Relationships(Some(vec![(Id(0), false)])))
					.is_err()
			);
			assert!(state.relationship_view() > view);
			assert!(state.friend(user.id).is_none());
			state
				.apply_user_action(Event::Relationships(Some(vec![])))
				.unwrap();
			view = state.relationship_view();
			state
				.apply_user_action(Event::Relationship {
					user: user.id,
					blocked: true,
				})
				.unwrap();
			assert!(state.relationship_view() > view);
			assert!(state.friend(user.id).is_none());
			// A rejected block can remove a friend before the relationship capacity check.
			state.user_actions.relationships = (10..10 + MAX_RELATIONSHIPS as u64)
				.map(|id| (Id(id), false))
				.collect();
			state
				.apply_user_action(Event::Friends(Some(vec![(user.clone(), "friend".into())])))
				.unwrap();
			view = state.relationship_view();
			assert!(state.store_relationship(user.id, true).is_err());
			assert!(state.relationship_view() > view);
			assert!(state.friend(user.id).is_none());
		}
		{
			let mut state = state();
			let user = state.channels[0].recipients[0].clone();
			state
				.apply_user_action(Event::Relationships(Some(vec![])))
				.unwrap();
			state
				.apply_user_action(Event::Friends(Some(vec![(user.clone(), "friend".into())])))
				.unwrap();
			let view = state.relationship_view();
			state.apply(Envelope {
				generation: state.generation,
				event: CoreEvent::Ready {
					permissions: Default::default(),
					user: state.user.clone().unwrap(),
					guilds: vec![],
					channels: state.channels.clone(),
				},
			});
			assert!(state.relationship_view() > view);
			assert!(state.friend(user.id).is_none());
			let generation = state.generation;
			state.logout();
			let view = state.relationship_view();
			state.apply(Envelope {
				generation,
				event: CoreEvent::UserAction(Event::Friends(Some(vec![(user, "stale".into())]))),
			});
			assert_eq!(state.relationship_view(), view);
			assert_eq!(state.friends().count(), 0);
		}
		{
			let mut state = state();
			let user = state.channels[0].recipients[0].clone();
			state
				.apply_user_action(Event::Relationships(Some(vec![(user.id, false)])))
				.unwrap();
			state
				.apply_user_action(Event::Friends(Some(vec![(
					user.clone(),
					"synthetic_friend".into(),
				)])))
				.unwrap();
			assert!(state.friends_known());
			assert_eq!(state.friends().count(), 1);
			assert_eq!(state.friend_username(user.id), Some("synthetic_friend"));
			let mut changed = user.clone();
			changed.name = "New display".into();
			state
				.apply_user_action(Event::FriendProfile((changed, "new_username".into())))
				.unwrap();
			assert_eq!(state.friends().next().unwrap().name, "New display");
			state
				.apply_user_action(Event::Friend {
					user: user.id,
					friend: false,
					profile: None,
				})
				.unwrap();
			assert_eq!(state.friends().count(), 0);
			state
				.apply_user_action(Event::Friend {
					user: user.id,
					friend: true,
					profile: Some((user.clone(), "user".into())),
				})
				.unwrap();
			let command = state.set_user_blocked(user.id, true).unwrap();
			assert_eq!(state.friends().count(), 0);
			finish(&mut state, command, Err(Failure::Forbidden));
			assert_eq!(state.friends().count(), 1);
			state
				.apply_user_action(Event::Relationship {
					user: user.id,
					blocked: true,
				})
				.unwrap();
			assert_eq!(state.friends().count(), 0);
			assert!(
				state
					.apply_user_action(Event::Friends(Some(vec![(
						user,
						"x".repeat(MAX_FRIEND_BYTES)
					)])))
					.is_err()
			);
			assert!(!state.friends_known());
			state.logout();
			assert_eq!(state.friends().count(), 0);
		}
	}

	#[test]
	fn disconnect_retains_the_latest_contact_draft_only_for_this_account() {
		let mut state = state();
		let user = Id(2);
		state.user_actions.note = Some((user, "Original".into()));
		assert!(state.set_user_note(user, "First draft".into()).is_some());
		assert!(state.set_user_note(user, "Latest draft".into()).is_some());

		state.cancel_user_action();
		state.user_actions.reset(); // The same account received a fresh READY.
		assert_eq!(state.failed_contact_edit(user, false), Some("Latest draft"));
		assert_eq!(state.failed_contact_edit(user, true), None);
		state.logout();
		assert_eq!(state.failed_contact_edit(user, false), None);
	}

	#[test]
	fn personal_edits_are_bounded_confirmed_and_reconcile_newer_service_state() {
		{
			let mut state = state();
			let user = state.channels[0].recipients[0].clone();
			assert!(
				state
					.set_user_note(user.id, "Must load first".into())
					.is_none()
			);
			assert!(
				state
					.set_friend_nickname(user.id, "Not a friend".into())
					.is_none()
			);
			let Command::UserAction { request, .. } = state.load_user_note(user.id).unwrap() else {
				panic!()
			};
			state
				.apply_user_action(Event::NoteChanged {
					user: user.id,
					text: "Newest note".into(),
				})
				.unwrap();
			state
				.apply_user_action(Event::NoteLoaded {
					user: user.id,
					request,
					result: Ok("Stale read".into()),
				})
				.unwrap();
			assert_eq!(state.user_note(user.id), Some("Newest note"));
			let write = state
				.set_user_note(user.id, "Private draft".into())
				.unwrap();
			finish(&mut state, write, Err(Failure::Forbidden));
			assert_eq!(state.user_note(user.id), Some("Newest note"));
			assert_eq!(
				state.failed_contact_edit(user.id, false),
				Some("Private draft")
			);
			let write = state.set_user_note(user.id, String::new()).unwrap();
			finish(&mut state, write, Ok(()));
			assert_eq!(state.user_note(user.id), Some(""));
			assert_eq!(state.failed_contact_edit(user.id, false), None);
			assert!(!valid_personal_text(&"🌙".repeat(257), false));
			assert!(!valid_personal_text("bad\nname", true));
			assert!(valid_personal_text(&"🌙".repeat(32), true));
			state
				.apply_user_action(Event::Relationships(Some(vec![])))
				.unwrap();
			state
				.apply_user_action(Event::Friends(Some(vec![(
					user.clone(),
					"synthetic".into(),
				)])))
				.unwrap();
			let write = state
				.set_friend_nickname(user.id, "Private name".into())
				.unwrap();
			finish(&mut state, write, Ok(()));
			assert_eq!(state.user_display_name(&user), "Private name");
			assert_eq!(state.conversation_name(&state.channels[0]), "Private name");
			let write = state
				.set_friend_nickname(user.id, "Late result".into())
				.unwrap();
			state
				.apply_user_action(Event::Nickname {
					user: user.id,
					text: "Newest name".into(),
				})
				.unwrap();
			finish(&mut state, write, Ok(()));
			assert_eq!(state.friend_nickname(user.id), Some("Newest name"));
			let write = state
				.set_friend_nickname(user.id, "Removed friend".into())
				.unwrap();
			state
				.apply_user_action(Event::Friend {
					user: user.id,
					friend: false,
					profile: None,
				})
				.unwrap();
			finish(&mut state, write, Ok(()));
			assert_eq!(state.friend_nickname(user.id), None);
			assert!(
				!format!(
					"{:?}",
					Action::Note {
						user: user.id,
						text: "PRIVATE".into()
					}
				)
				.contains("PRIVATE")
			);
			state.logout();
			assert_eq!(state.user_note(user.id), None);
		}
		{
			let mut state = state();
			assert_eq!(state.user_blocked(Id(2)), None);
			assert!(state.set_user_blocked(Id(1), true).is_none());
			assert!(state.set_user_blocked(Id(2), false).is_none());
			let block = state.set_user_blocked(Id(2), true).unwrap();
			assert_eq!(state.user_blocked(Id(2)), Some(true));
			assert_eq!(state.user_action_status(), None);
			// Duplicate identical block is a no-op; unrelated targets stay unblocked.
			assert!(state.set_user_blocked(Id(2), true).is_none());
			finish(&mut state, block, Err(Failure::Forbidden));
			assert!(!state.user_action_pending());
			assert_eq!(state.user_blocked(Id(2)), None);
			assert_eq!(state.status, "Disconnected");
			assert_eq!(state.user_action_status(), None);
			let block = state.set_user_blocked(Id(2), true).unwrap();
			finish(&mut state, block, Ok(()));
			assert_eq!(state.user_blocked(Id(2)), Some(true));
			let unblock = state.set_user_blocked(Id(2), false).unwrap();
			assert_eq!(state.user_blocked(Id(2)), Some(false));
			finish(&mut state, unblock, Ok(()));
			assert_eq!(state.user_blocked(Id(2)), Some(false));
			state.drafts.insert(Id(10), "Keep my draft".into());
			let close = state.close_dm(Id(10)).unwrap();
			finish(&mut state, close, Err(Failure::Ambiguous));
			assert!(state.channel(Id(10)).is_some());
			let close = state.close_dm(Id(10)).unwrap();
			finish(&mut state, close, Ok(()));
			assert!(state.channel(Id(10)).is_none());
			assert_eq!(state.selected, None);
			assert_eq!(state.drafts[&Id(10)], "Keep my draft");
			let block = state.set_user_blocked(Id(2), true).unwrap();
			state.cancel_user_action();
			finish(&mut state, block, Ok(()));
			assert_eq!(state.user_blocked(Id(2)), Some(false));
			state.gateway_connected = false;
			assert!(state.set_user_blocked(Id(2), true).is_none());
			state.demo = true;
			let command = state.set_user_blocked(Id(2), true).unwrap();
			state.command_rejected(command);
			assert!(!state.user_action_pending());
			assert_eq!(state.user_blocked(Id(2)), Some(false));
		}
		{
			let mut state = state();
			state
				.apply_user_action(Event::Relationships(Some(vec![(Id(2), false)])))
				.unwrap();
			let block = state.set_user_blocked(Id(2), true).unwrap();
			state
				.apply_user_action(Event::Relationship {
					user: Id(2),
					blocked: true,
				})
				.unwrap();
			state
				.apply_user_action(Event::Relationship {
					user: Id(2),
					blocked: false,
				})
				.unwrap();
			finish(&mut state, block, Ok(()));
			assert_eq!(state.user_blocked(Id(2)), Some(false));
			let mute = state.set_dm_muted(Id(10), true).unwrap();
			assert_eq!(state.dm_muted(Id(10)), Some(true));
			finish(&mut state, mute, Err(Failure::Forbidden));
			assert_eq!(state.dm_muted(Id(10)), None);
			let mute = state.set_dm_muted(Id(10), true).unwrap();
			finish(&mut state, mute, Ok(()));
			assert_eq!(state.dm_muted(Id(10)), Some(true));
			let unmute = state.set_dm_muted(Id(10), false).unwrap();
			state
				.apply_notification_preferences(crate::notifications::Event::Settings {
					entries: vec![crate::notifications::Setting {
						channels: vec![(Id(10), Some(true), Some(2))],
						..Default::default()
					}],
					replace: false,
				})
				.unwrap();
			finish(&mut state, unmute, Ok(()));
			assert_eq!(state.dm_muted(Id(10)), Some(true));
			// Completion feedback is transient UI state, never a shared menu status.
			assert_eq!(state.status, "Disconnected");
			assert_eq!(state.user_action_status(), None);
			let unmute = state.set_dm_muted(Id(10), false).unwrap();
			state
				.apply_notification_preferences(crate::notifications::Event::Settings {
					entries: vec![crate::notifications::Setting {
						guild: Some(Id(999)),
						..Default::default()
					}],
					replace: false,
				})
				.unwrap();
			finish(&mut state, unmute, Ok(()));
			assert_eq!(state.dm_muted(Id(10)), Some(false));
			let channel = state.channel(Id(10)).unwrap().clone();
			let close = state.close_dm(Id(10)).unwrap();
			assert!(state.channel(Id(10)).is_none());
			state.apply(Envelope {
				generation: state.generation,
				event: CoreEvent::Unavailable(Id(10)),
			});
			state.apply(Envelope {
				generation: state.generation,
				event: CoreEvent::ChannelCreated(channel),
			});
			finish(&mut state, close, Ok(()));
			assert!(
				state.channel(Id(10)).is_some(),
				"late close must not remove a DM reopened by a later event"
			);
			assert!(
				state
					.apply_user_action(Event::Relationships(Some(vec![
						(Id(2), true);
						MAX_RELATIONSHIPS + 1
					])))
					.is_err()
			);
			assert_eq!(state.user_blocked(Id(2)), None);
			assert!(
				state
					.apply_user_action(Event::Relationships(Some(vec![
						(Id(2), true),
						(Id(2), false)
					])))
					.is_err()
			);
			state.logout();
			assert_eq!(state.dm_muted(Id(10)), None);
			assert_eq!(state.user_blocked(Id(2)), None);
		}
	}
	#[test]
	fn friend_requests_validate_bound_and_reconcile_without_duplicate_writes() {
		{
			let mut state = state();
			let user = state.channels[0].recipients[0].clone();
			state
				.apply_user_action(Event::Relationships(Some(vec![])))
				.unwrap();
			state
				.apply_user_action(Event::Requests(Some(vec![(
					user.clone(),
					"synthetic".into(),
					true,
				)])))
				.unwrap();
			assert_eq!(state.pending_friends().count(), 1);
			assert!(state.add_friend("../invalid").is_none());
			let send = state.add_friend(" @new_friend ").unwrap();
			assert!(state.add_friend("new_friend").is_none());
			assert_eq!(state.optimistic_outgoing_requests(), vec!["new_friend"]);
			finish(&mut state, send, Ok(()));
			assert!(state.optimistic_outgoing_requests().is_empty());
			assert_eq!(state.user_action_status(), None);
			assert!(state.add_friend("new_friend").is_none());
			assert_eq!(
				state.pending_friends().count(),
				1,
				"no invented outgoing identity"
			);
			let rejected = state.add_friend("other_friend").unwrap();
			finish(&mut state, rejected, Err(Failure::Protocol));
			assert_eq!(state.user_action_status(), None);
			let accept = state.resolve_friend_request(user.id, true).unwrap();
			finish(&mut state, accept, Err(Failure::Forbidden));
			assert_eq!(state.pending_friends().count(), 1);
			let accept = state.resolve_friend_request(user.id, true).unwrap();
			state
				.apply_user_action(Event::Request {
					user: user.id,
					incoming: None,
					profile: None,
				})
				.unwrap();
			finish(&mut state, accept, Ok(()));
			assert_eq!(
				state.friends().count(),
				0,
				"newer Gateway removal wins over acceptance"
			);
			state
				.apply_user_action(Event::Request {
					user: user.id,
					incoming: Some(false),
					profile: Some((user.clone(), "synthetic".into())),
				})
				.unwrap();
			assert!(state.resolve_friend_request(user.id, true).is_none());
			let cancel = state.resolve_friend_request(user.id, false).unwrap();
			finish(&mut state, cancel, Ok(()));
			assert_eq!(state.user_action_status(), None);
			assert_eq!(state.pending_friends().count(), 0);
			state
				.apply_user_action(Event::Request {
					user: user.id,
					incoming: Some(true),
					profile: Some((user.clone(), "synthetic".into())),
				})
				.unwrap();
			let accept = state.resolve_friend_request(user.id, true).unwrap();
			state
				.apply_user_action(Event::Friend {
					user: user.id,
					friend: true,
					profile: None,
				})
				.unwrap();
			state
				.apply_user_action(Event::Request {
					user: user.id,
					incoming: None,
					profile: None,
				})
				.unwrap();
			assert_eq!(
				state.friends().count(),
				1,
				"acceptance without repeated metadata retains the request profile"
			);
			finish(&mut state, accept, Ok(()));
			assert_eq!(state.friends().count(), 1);
			assert_eq!(state.pending_friends().count(), 0);
			assert!(
				state
					.apply_user_action(Event::Requests(Some(vec![(
						user,
						"huge".repeat(MAX_FRIEND_BYTES),
						true
					)])))
					.is_err()
			);
			state.gateway_connected = false;
			assert!(state.add_friend("another").is_none());
			state.logout();
			assert!(!state.friend_requests_known());
			assert_eq!(state.pending_friends().count(), 0);
		}
		{
			let mut state = state();
			let user = state.channels[0].recipients[0].clone();
			assert!(state.add_profile_friend(user.id).is_none());
			state
				.apply_user_action(Event::Friends(Some(vec![])))
				.unwrap();
			state
				.apply_user_action(Event::Requests(Some(vec![])))
				.unwrap();
			state
				.apply_user_action(Event::Relationships(Some(vec![])))
				.unwrap();
			assert!(state.add_profile_friend(Id(0)).is_none());
			assert!(state.add_profile_friend(Id(1)).is_none());
			let add = state.add_profile_friend(user.id).unwrap();
			assert!(state.add_profile_friend(user.id).is_none());
			assert_eq!(state.pending_friends().count(), 1);
			finish(&mut state, add, Err(Failure::Forbidden));
			assert_eq!(state.pending_friends().count(), 0);
			let add = state.add_profile_friend(user.id).unwrap();
			finish(&mut state, add, Ok(()));
			assert_eq!(
				state
					.pending_friends()
					.next()
					.map(|(u, _, incoming)| (u.id, *incoming)),
				Some((user.id, false))
			);
			assert!(state.add_profile_friend(user.id).is_none());
			assert!(state.remove_friend(user.id).is_none());
			state
				.apply_user_action(Event::Friend {
					user: user.id,
					friend: true,
					profile: Some((user.clone(), "synthetic".into())),
				})
				.unwrap();
			state
				.apply_user_action(Event::Request {
					user: user.id,
					incoming: None,
					profile: None,
				})
				.unwrap();
			let remove = state.remove_friend(user.id).unwrap();
			assert_eq!(state.friends().count(), 0);
			finish(&mut state, remove, Err(Failure::Forbidden));
			assert_eq!(state.friends().count(), 1);
			let remove = state.remove_friend(user.id).unwrap();
			finish(&mut state, remove, Ok(()));
			assert_eq!(state.friends().count(), 0);
			let add = state.add_profile_friend(user.id).unwrap();
			state
				.apply_user_action(Event::Friend {
					user: user.id,
					friend: true,
					profile: Some((user.clone(), "synthetic".into())),
				})
				.unwrap();
			finish(&mut state, add, Ok(()));
			assert_eq!(state.friends().count(), 1);
			assert_eq!(state.pending_friends().count(), 0);
			let remove = state.remove_friend(user.id).unwrap();
			state
				.apply_user_action(Event::Friend {
					user: user.id,
					friend: false,
					profile: None,
				})
				.unwrap();
			state
				.apply_user_action(Event::Friend {
					user: user.id,
					friend: true,
					profile: Some((user.clone(), "synthetic".into())),
				})
				.unwrap();
			finish(&mut state, remove, Ok(()));
			assert_eq!(state.friends().count(), 1, "newer gateway friendship wins");
			state
				.apply_user_action(Event::Relationship {
					user: user.id,
					blocked: true,
				})
				.unwrap();
			assert!(state.add_profile_friend(user.id).is_none());
			assert!(state.remove_friend(user.id).is_none());
			state.gateway_connected = false;
			assert!(state.add_profile_friend(Id(3)).is_none());
		}
	}
	/// Fills the committed status index to exercise conservative eviction, then verifies logout removes retained exclusions.
	#[test]
	fn committed_ignore_capacity_eviction_becomes_unknown_and_logout_forgets_it() {
		let mut state = state();
		state
			.apply_user_action(Event::Relationships(Some(vec![])))
			.unwrap();
		state
			.apply_user_action(Event::Restrictions(Some(vec![])))
			.unwrap();
		for id in 100..=100 + MAX_RELATIONSHIPS as u64 {
			state
				.apply_user_action(Event::Restriction {
					user: Id(id),
					ignored: Some(true),
					profile: None,
				})
				.unwrap();
		}
		let newest = Id(100 + MAX_RELATIONSHIPS as u64);
		assert_eq!(state.user_ignored(newest), Some(true));
		assert_eq!(state.service_author_excluded(Id(100)), None);
		assert!(state.user_actions.ignored.len() <= MAX_RELATIONSHIPS);
		assert!(state.user_actions.ignored.len() * IGNORED_ENTRY_BYTES <= MAX_IGNORED_BYTES);
		state.logout();
		assert_eq!(state.user_ignored(newest), None);
	}
	/// Synthetic authenticated state for user-action tests.
	fn state() -> State {
		let user = |id| model::User {
			primary_guild: None,
			cosmetics: None,
			id: Id(id),
			name: "Synthetic".into(),
			avatar: None,
			webhook: false,
			kind: Default::default(),
			discriminator: 0,
		};
		State {
			user: Some(user(1)),
			auth: AuthState::Authenticated,
			gateway_connected: true,
			selected: Some(Id(10)),
			channels: vec![model::Channel {
				user_limit: None,
				slowmode: 0,
				id: Id(10),
				guild: None,
				kind: 1,
				recipients: vec![user(2)],
				name: "Synthetic DM".into(),
				parent_id: None,
				position: 0,
				last_message: None,
				icon: None,
				member_list_id: None,
				tags: None,
				message_count: None,
			}],
			..State::default()
		}
	}
	/// Completes a pending write with the given result.
	fn finish(state: &mut State, command: Command, result: Result<(), Failure>) {
		let Command::UserAction {
			action, request, ..
		} = command
		else {
			panic!("wrong command")
		};
		state.apply(Envelope {
			generation: state.generation,
			event: CoreEvent::UserAction(Event::Written {
				action,
				request,
				result,
			}),
		});
	}
	#[test]
	fn restricted_profiles_replace_update_and_clear_with_the_session() {
		let mut state = state();
		let user = state.channels[0].recipients[0].clone();
		state
			.apply_user_action(Event::Restrictions(Some(vec![(
				user.clone(),
				"synthetic_user".into(),
				false,
			)])))
			.unwrap();
		assert!(state.restricted_users_known());
		assert_eq!(state.restricted_users().count(), 1);
		assert!(!state.restricted_user(user.id).unwrap().2);
		state
			.apply_user_action(Event::Restriction {
				user: user.id,
				ignored: Some(true),
				profile: None,
			})
			.unwrap();
		assert!(state.restricted_user(user.id).unwrap().2);
		let mut changed = user.clone();
		changed.name = "Updated display".into();
		state
			.apply_user_action(Event::FriendProfile((changed, "updated_username".into())))
			.unwrap();
		assert_eq!(
			state.restricted_user(user.id).unwrap().0.name,
			"Updated display"
		);
		state
			.apply_user_action(Event::Restriction {
				user: user.id,
				ignored: None,
				profile: None,
			})
			.unwrap();
		assert_eq!(state.restricted_users().count(), 0);
		state.logout();
		assert!(!state.restricted_users_known());
	}
	#[test]
	fn fresh_ready_does_not_reuse_an_outstanding_write_request() {
		let mut state = state();
		let old = state.set_user_blocked(Id(2), true).unwrap();
		let user = state.user.clone().unwrap();
		let channels = state.channels.clone();
		state.apply(Envelope {
			generation: state.generation,
			event: CoreEvent::Ready {
				permissions: Default::default(),
				user,
				guilds: vec![],
				channels,
			},
		});
		let new = state.set_user_blocked(Id(2), true).unwrap();
		finish(&mut state, old, Ok(()));
		assert!(state.user_action_pending());
		assert_eq!(state.user_blocked(Id(2)), Some(true));
		finish(&mut state, new, Ok(()));
		assert_eq!(state.user_blocked(Id(2)), Some(true));
	}
	/// Regression: a friendship challenge is scoped, single-use and resumes its write.
	#[test]
	fn friend_captcha_is_scoped_single_use_and_resumes_the_same_write() {
		{
			use crate::captcha::{Challenge, Solution};
			let mut state = state();
			let Some(Command::UserAction {
				action,
				request,
				captcha: None,
			}) = state.add_friend("synthetic_friend")
			else {
				panic!("friend request")
			};
			assert!(matches!(action, Action::AddFriend { .. }));
			state.apply(Envelope {
				generation: state.generation,
				event: CoreEvent::UserAction(Event::Challenge {
					action: action.clone(),
					request,
					challenge: Box::new(
						Challenge::new("synthetic-key".into(), None, None, None, false).unwrap(),
					),
				}),
			});
			assert_eq!(
				state.account_challenge().map(|(request, _)| request),
				Some(request)
			);
			// A solution cannot resume a different request.
			assert!(
				state
					.resume_account_challenge(
						request.wrapping_add(1),
						Solution::new("synthetic-solution".into()).unwrap()
					)
					.is_none()
			);
			let Some(Command::UserAction {
				action: resumed,
				request: resumed_request,
				captcha: Some(retry),
			}) = state.resume_account_challenge(
				request,
				Solution::new("synthetic-solution".into()).unwrap(),
			)
			else {
				panic!("resume")
			};
			assert!(matches!(resumed, Action::AddFriend { .. }));
			assert_ne!(resumed_request, request);
			assert!(retry.matches_target(&challenge_target(&resumed).unwrap()));
			// The challenge is consumed once.
			assert!(state.account_challenge().is_none());
			assert!(
				state
					.resume_account_challenge(
						resumed_request,
						Solution::new("synthetic-solution".into()).unwrap()
					)
					.is_none()
			);
			finish(
				&mut state,
				Command::UserAction {
					action: resumed,
					request: resumed_request,
					captcha: None,
				},
				Ok(()),
			);
			assert!(!state.user_action_pending());
		}
		{
			use crate::captcha::Challenge;
			let challenge =
				|| Challenge::new("synthetic-key".into(), None, None, None, false).unwrap();
			let mut state = state();
			let Some(Command::UserAction {
				action, request, ..
			}) = state.add_friend("synthetic_friend")
			else {
				panic!("friend request")
			};
			state.apply(Envelope {
				generation: state.generation,
				event: CoreEvent::UserAction(Event::Challenge {
					action: action.clone(),
					request,
					challenge: Box::new(challenge()),
				}),
			});
			state.cancel_account_challenge(request);
			assert!(state.account_challenge().is_none() && !state.user_action_pending());
			assert_eq!(
				state.take_failed_add_friend().as_deref(),
				Some("synthetic_friend")
			);
			// Expiry releases the pending write so the user can send it again.
			let Some(Command::UserAction {
				action, request, ..
			}) = state.add_friend("synthetic_friend")
			else {
				panic!("friend request")
			};
			state.apply(Envelope {
				generation: state.generation,
				event: CoreEvent::UserAction(Event::Challenge {
					action: action.clone(),
					request,
					challenge: Box::new(challenge()),
				}),
			});
			state.user_actions.challenge.as_mut().unwrap().3 =
				std::time::Instant::now() - crate::captcha::LIFETIME;
			state.expire_account_challenge();
			assert!(state.account_challenge().is_none() && !state.user_action_pending());
			assert_eq!(
				state.take_failed_add_friend().as_deref(),
				Some("synthetic_friend")
			);
		}
	}

	#[test]
	fn optimistic_user_actions_coalesce_run_concurrently_and_rollback_with_toasts() {
		let mut state = state();
		let dm_id = Id(10);
		let u2 = Id(2);
		let u3 = Id(3);
		let mut u2_user = state.channels[0].recipients[0].clone();
		u2_user.id = u2;
		u2_user.name = "Two".into();
		let mut u3_user = u2_user.clone();
		u3_user.id = u3;
		u3_user.name = "Three".into();
		state
			.apply_user_action(Event::Relationships(Some(vec![])))
			.unwrap();
		state
			.apply_user_action(Event::Requests(Some(vec![
				(u2_user, "two".into(), true),
				(u3_user, "three".into(), true),
			])))
			.unwrap();

		// Two friend resolutions can run concurrently without blocking each other.
		let accept_u2 = state.resolve_friend_request(u2, true).unwrap();
		let decline_u3 = state.resolve_friend_request(u3, false).unwrap();
		assert_eq!(state.pending_friends().count(), 0);
		assert!(state.friend(u2).is_some());

		// Failure on u2 rolls back u2 without disturbing u3 and emits an action error toast.
		finish(&mut state, accept_u2, Err(Failure::Forbidden));
		assert!(state.friend(u2).is_none());
		assert_eq!(state.pending_friends().count(), 1);
		let errors = state.take_action_errors();
		assert_eq!(errors.len(), 1);
		assert!(errors[0].target.contains("Two"));
		finish(&mut state, decline_u3, Ok(()));
		assert_eq!(state.pending_friends().count(), 1);

		// AddFriend failure clears optimistic outgoing row, restores username for UI, and toasts.
		let add = state.add_friend("alice_99").unwrap();
		assert_eq!(state.optimistic_outgoing_requests(), vec!["alice_99"]);
		finish(&mut state, add, Err(Failure::RateLimited));
		assert!(state.optimistic_outgoing_requests().is_empty());
		assert_eq!(state.take_failed_add_friend().as_deref(), Some("alice_99"));
		assert_eq!(state.take_action_errors().len(), 1);

		// Rapid Block -> Unblock emits a pre-wire cancel and settles cleanly when cancelled.
		let block = state.set_user_blocked(u2, true).unwrap();
		let Command::UserAction {
			request: block_seq, ..
		} = block
		else {
			panic!("expected user action command");
		};
		assert_eq!(state.user_blocked(u2), Some(true));
		let cancel = state.set_user_blocked(u2, false).unwrap();
		assert!(matches!(
			cancel,
			Command::CancelPendingWrite {
				kind: crate::optimistic::WriteKind::UserAction,
				request,
			} if request == block_seq
		));
		assert_eq!(state.user_blocked(u2), Some(false));
		state.apply(Envelope {
			generation: state.generation,
			event: CoreEvent::WriteCancelled {
				kind: crate::optimistic::WriteKind::UserAction,
				request: block_seq,
			},
		});
		assert_eq!(state.user_blocked(u2), Some(false));
		assert!(state.take_follow_up_commands().is_empty());

		// CloseDm failure restores the channel and emits an error toast.
		let close = state.close_dm(dm_id).unwrap();
		assert!(state.channel(dm_id).is_none());
		finish(&mut state, close, Err(Failure::Network));
		assert!(state.channel(dm_id).is_some());
		assert_eq!(state.take_action_errors().len(), 1);

		// Coalesced Nickname write #1 -> write #2 commits write #2 cleanly when settled.
		let accept_u2_ok = state.resolve_friend_request(u2, true).unwrap();
		finish(&mut state, accept_u2_ok, Ok(()));
		let nick1 = state.set_friend_nickname(u2, "First".into()).unwrap();
		assert_eq!(state.friend_nickname(u2), Some("First"));
		assert!(matches!(
			state.set_friend_nickname(u2, "Second".into()),
			Some(Command::CancelPendingWrite {
				kind: crate::optimistic::WriteKind::UserAction,
				..
			})
		));
		assert_eq!(state.friend_nickname(u2), Some("Second"));
		finish(&mut state, nick1, Ok(()));
		let follow_ups = state.take_follow_up_commands();
		assert_eq!(follow_ups.len(), 1);
		finish(&mut state, follow_ups.into_iter().next().unwrap(), Ok(()));
		assert_eq!(state.friend_nickname(u2), Some("Second"));
	}
}
