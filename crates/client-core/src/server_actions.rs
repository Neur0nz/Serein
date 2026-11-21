//! Bounded server writes: coalesced notification prefs, concurrent invites, and confirmed leave/delete.
use crate::optimistic::ActionKind;
use crate::{
	Command, State,
	auth::{AuthState, Failure},
	optimistic::{Cancelled, CoalescedSlot, Finish, InFlightSet, SetOutcome, WriteKind},
};
use model::{Id, permissions::VIEW_CHANNEL};
use std::{
	collections::BTreeMap,
	time::{Duration, Instant},
};

const CREATE_INSTANT_INVITE: u128 = 1;
const MAX_NOTIFICATION_WRITES: usize = 32;
const MAX_CONCURRENT_INVITES: usize = 8;
const MAX_CONCURRENT_LEAVES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InviteOptions {
	pub max_age: u32,
	pub max_uses: u16,
	pub temporary: bool,
}
impl Default for InviteOptions {
	fn default() -> Self {
		Self {
			max_age: 30 * 86400,
			max_uses: 0,
			temporary: false,
		}
	}
}
impl InviteOptions {
	pub fn valid(self) -> bool {
		// Normal-user 30-day invites exceed the public developer API's documented 7 days.
		self.max_age <= 30 * 86400 && self.max_uses <= 100
	}
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InviteStatus {
	Sending,
	Sent,
	Failed(Failure),
}
/// Sparse account-scoped preferences; no server-management permission is required.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NotificationOptions {
	pub level: Option<u8>,
	pub muted: Option<bool>,
	pub suppress_everyone: Option<bool>,
	pub suppress_roles: Option<bool>,
}
impl NotificationOptions {
	pub fn valid(self) -> bool {
		self.level.is_none_or(|level| level <= 3)
			&& (self.level.is_some()
				|| self.muted.is_some()
				|| self.suppress_everyone.is_some()
				|| self.suppress_roles.is_some())
	}
	pub(crate) fn merge(self, overlay: Self) -> Self {
		Self {
			level: overlay.level.or(self.level),
			muted: overlay.muted.or(self.muted),
			suppress_everyone: overlay.suppress_everyone.or(self.suppress_everyone),
			suppress_roles: overlay.suppress_roles.or(self.suppress_roles),
		}
	}
	pub(crate) fn sparse_diff(self, base: Self) -> Self {
		Self {
			level: (self.level != base.level).then_some(self.level).flatten(),
			muted: (self.muted != base.muted).then_some(self.muted).flatten(),
			suppress_everyone: (self.suppress_everyone != base.suppress_everyone)
				.then_some(self.suppress_everyone)
				.flatten(),
			suppress_roles: (self.suppress_roles != base.suppress_roles)
				.then_some(self.suppress_roles)
				.flatten(),
		}
	}
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
	Notifications {
		guild: Id,
		options: NotificationOptions,
	},
	CreateInvite {
		guild: Id,
		channel: Id,
		options: InviteOptions,
	},
	Leave(Id),
	Delete(Id),
}
impl Action {
	pub fn guild(self) -> Id {
		match self {
			Self::CreateInvite { guild, .. }
			| Self::Notifications { guild, .. }
			| Self::Leave(guild)
			| Self::Delete(guild) => guild,
		}
	}
}
pub enum Event {
	InviteSent {
		guild: Id,
		user: Id,
		request: u64,
		result: Result<Box<(model::Channel, model::Message)>, Failure>,
	},
	Written {
		action: Action,
		request: u64,
		result: Result<Option<String>, Failure>,
	},
}
struct NotificationMeta {
	observed: bool,
	guild_label: String,
}
struct InviteInFlight {
	nonce: String,
	friend_label: String,
}
struct LeftServerSnapshot {
	guild: model::Guild,
	channels: Vec<model::Channel>,
	perm_guild: Option<model::permissions::Guild>,
	perm_channels: Vec<(Id, model::permissions::Channel)>,
	observed: bool,
	guild_label: String,
}
#[derive(Default)]
pub struct Actions {
	sequence: u64,
	/// Synchronous/destructive server writes (`CreateInvite`, `Delete`).
	pending: Option<(Action, u64, bool)>,
	notifications: CoalescedSlot<Id, NotificationOptions, MAX_NOTIFICATION_WRITES>,
	notification_meta: BTreeMap<Id, NotificationMeta>,
	leaves: InFlightSet<Id, LeftServerSnapshot, MAX_CONCURRENT_LEAVES>,
	status: Option<(Id, &'static str)>,
	invite: Option<(Id, Id, String, Instant, InviteOptions)>,
	sending: InFlightSet<(Id, Id), InviteInFlight, MAX_CONCURRENT_INVITES>,
	sent: BTreeMap<Id, InviteStatus>,
}
impl Actions {
	pub(crate) fn reset(&mut self) {
		let mut notifications = std::mem::take(&mut self.notifications);
		let _ = notifications.cancel_all();
		let mut leaves = std::mem::take(&mut self.leaves);
		let _ = leaves.cancel_all();
		let mut sending = std::mem::take(&mut self.sending);
		let _ = sending.cancel_all();
		*self = Self {
			sequence: self.sequence,
			notifications,
			leaves,
			sending,
			..Self::default()
		};
	}
	pub(crate) fn optimistic_notifications(&self, guild: Id) -> Option<NotificationOptions> {
		self.notifications.entry(&guild).map(|e| e.desired)
	}
	pub(crate) fn has_fast_pending(&self) -> bool {
		self.pending.is_some()
			|| !self.notifications.is_empty()
			|| !self.leaves.is_empty()
			|| !self.sending.is_empty()
	}
}
impl State {
	pub fn can_update_server_notifications(&self, guild: Id) -> bool {
		guild.0 != 0
			&& self.guild(guild).is_some()
			&& (self.demo || (self.auth == AuthState::Authenticated && self.gateway_connected))
	}
	pub fn update_server_notifications(
		&mut self,
		guild: Id,
		options: NotificationOptions,
	) -> Option<Command> {
		if !options.valid() || !self.can_update_server_notifications(guild) {
			return None;
		}
		let confirmed = self.confirmed_server_notification_settings(guild);
		let desired = self.server_notification_settings(guild).merge(options);
		let label = self.guild_toast_label(guild);
		let outcome = self
			.server_actions
			.notifications
			.set(guild, confirmed, desired);
		self.server_actions.status = None;
		match outcome {
			SetOutcome::Full => {
				self.server_actions.status = Some((guild, "Too many pending server updates"));
				None
			}
			SetOutcome::Dispatch(seq) => {
				self.server_actions.notification_meta.insert(
					guild,
					NotificationMeta {
						observed: false,
						guild_label: label,
					},
				);
				self.revision = self.revision.wrapping_add(1);
				Some(Command::ServerAction {
					action: Action::Notifications {
						guild,
						options: desired.sparse_diff(confirmed),
					},
					request: seq,
				})
			}
			SetOutcome::Queued {
				cancel_in_flight: Some(cancel_seq),
			} => {
				self.revision = self.revision.wrapping_add(1);
				Some(Command::CancelPendingWrite {
					kind: WriteKind::ServerAction,
					request: cancel_seq,
				})
			}
			SetOutcome::Queued {
				cancel_in_flight: None,
			} => {
				self.revision = self.revision.wrapping_add(1);
				None
			}
		}
	}
	pub(crate) fn cancel_server_action_pre_wire(&mut self, request: u64) {
		let Some((guild, outcome)) = self.server_actions.notifications.cancelled_by_seq(request)
		else {
			return;
		};
		match outcome {
			Cancelled::Stale => {}
			Cancelled::Settled => {
				self.server_actions.notification_meta.remove(&guild);
				self.revision = self.revision.wrapping_add(1);
			}
			Cancelled::DispatchFollowUp { seq, value } => {
				if let Some(meta) = self.server_actions.notification_meta.get_mut(&guild) {
					meta.observed = false;
				}
				let confirmed = self
					.server_actions
					.notifications
					.entry(&guild)
					.map(|e| e.confirmed)
					.unwrap_or_default();
				let sparse = value.sparse_diff(confirmed);
				let options = if sparse.valid() { sparse } else { value };
				self.push_follow_up(Command::ServerAction {
					action: Action::Notifications { guild, options },
					request: seq,
				});
			}
		}
	}
	pub(crate) fn observe_server_notification_settings(
		&mut self,
		entries: &[crate::notifications::Setting],
	) {
		for setting in entries {
			let Some(guild) = setting.guild else {
				continue;
			};
			if let Some(meta) = self.server_actions.notification_meta.get_mut(&guild) {
				meta.observed = true;
			}
			if self.server_actions.notifications.entry(&guild).is_some() {
				let observed = NotificationOptions {
					level: setting.level.filter(|level| *level <= 3),
					muted: setting.muted(),
					suppress_everyone: setting.suppress_everyone,
					suppress_roles: setting.suppress_roles,
				};
				self.server_actions
					.notifications
					.observe_authoritative(&guild, observed);
			}
		}
	}
	pub fn server_invite_pending(&self) -> bool {
		!self.server_actions.sending.is_empty()
	}
	pub fn server_invite_status(&self, guild: Id, user: Id) -> Option<InviteStatus> {
		if self.server_actions.sending.contains(&(guild, user)) {
			return Some(InviteStatus::Sent);
		}
		self.server_actions
			.invite
			.as_ref()
			.filter(|i| i.0 == guild)?;
		self.server_actions.sent.get(&user).copied()
	}
	pub fn send_server_invite(&mut self, guild: Id, user: Id) -> Option<Command> {
		if self.server_actions.sending.len() >= MAX_CONCURRENT_INVITES
			|| self.server_actions.sending.contains(&(guild, user))
			|| self.server_actions.pending.is_some()
			|| (!self.demo && (self.auth != AuthState::Authenticated || !self.gateway_connected))
			|| !self.friends().any(|friend| friend.id == user)
			|| self.user_blocked(user) != Some(false)
			|| matches!(
				self.server_invite_status(guild, user),
				Some(InviteStatus::Sent | InviteStatus::Failed(Failure::Ambiguous))
			) || self.server_actions.sent.len() >= crate::user_actions::MAX_RELATIONSHIPS
		{
			return None;
		}
		let url = self.created_invite(guild)?.to_owned();
		let code = url.strip_prefix("https://discord.gg/")?.to_owned();
		self.send_sequence = self.send_sequence.wrapping_add(1);
		let epoch = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap_or_default()
			.as_millis();
		let nonce = crate::fingerprint::nonce(epoch, self.send_sequence);
		let friend_label = self
			.friends()
			.find(|f| f.id == user)
			.map(|f| f.name.clone())
			.unwrap_or_else(|| "friend".into());
		let request = self
			.server_actions
			.sending
			.insert(
				(guild, user),
				InviteInFlight {
					nonce: nonce.clone(),
					friend_label,
				},
			)
			.ok()?;
		self.server_actions.status = None;
		self.revision = self.revision.wrapping_add(1);
		Some(Command::SendServerInvite {
			guild,
			user,
			code,
			nonce,
			request,
		})
	}
	fn apply_server_invite_sent(
		&mut self,
		guild: Id,
		user: Id,
		request: u64,
		result: Result<(model::Channel, model::Message), Failure>,
	) -> Result<(), &'static str> {
		let Some(in_flight) = self.server_actions.sending.finish(&(guild, user), request) else {
			return Ok(());
		};
		let result = result.and_then(|(channel, message)| {
			if channel.guild.is_some()
				|| channel.kind != 1
				|| channel.recipients.len() != 1
				|| channel.recipients[0].id != user
				|| channel.id.0 == 0
				|| message.id.0 == 0
				|| message.channel != channel.id
				|| message.nonce.as_deref() != Some(in_flight.nonce.as_str())
				|| self
					.user
					.as_ref()
					.is_none_or(|owner| owner.id != message.author.id)
				|| self
					.server_actions
					.invite
					.as_ref()
					.is_none_or(|i| i.0 != guild || i.2 != message.content)
			{
				Err(Failure::Ambiguous)
			} else {
				Ok((channel, message))
			}
		});
		match result {
			Ok((channel, message)) => {
				self.server_actions.sent.insert(user, InviteStatus::Sent);
				self.server_actions.status = Some((guild, "Invite sent"));
				// Feed acknowledgements through normal bounded channel/message reconciliation.
				if self.channel(channel.id).is_none() {
					self.apply(crate::Envelope {
						generation: self.generation,
						event: crate::Event::ChannelCreated(channel),
					});
				}
				self.apply(crate::Envelope {
					generation: self.generation,
					event: crate::Event::Message(message),
				});
			}
			Err(failure) => {
				self.server_actions
					.sent
					.insert(user, InviteStatus::Failed(failure));
				self.server_actions.status = Some((guild, failure.label()));
				if failure.ends_session() && failure != Failure::Capacity {
					self.fail(failure);
				} else {
					self.push_write_error(ActionKind::SendInvite, in_flight.friend_label, failure);
				}
			}
		}
		self.revision = self.revision.wrapping_add(1);
		Ok(())
	}
	pub fn server_action_pending(&self) -> bool {
		self.server_actions.pending.is_some()
	}
	pub fn server_action_status(&self, guild: Id) -> Option<&'static str> {
		self.server_actions
			.status
			.filter(|(id, _)| *id == guild)
			.map(|(_, text)| text)
	}
	pub fn created_invite(&self, guild: Id) -> Option<&str> {
		self.server_actions
			.invite
			.as_ref()
			.filter(|(id, channel, _, at, options)| {
				*id == guild
					&& self.can_create_server_invite(guild, *channel)
					&& (options.max_age == 0
						|| at.elapsed() < Duration::from_secs(u64::from(options.max_age)))
			})
			.map(|(_, _, url, _, _)| url.as_str())
	}
	pub fn created_invite_options(&self, guild: Id) -> Option<InviteOptions> {
		self.created_invite(guild)?;
		self.server_actions.invite.as_ref().map(|i| i.4)
	}
	pub fn clear_server_action_result(&mut self, guild: Id) {
		if self
			.server_actions
			.sending
			.iter()
			.any(|((g, _), _)| *g == guild)
		{
			return;
		}
		if self
			.server_actions
			.status
			.is_some_and(|(id, _)| id == guild)
		{
			self.server_actions.status = None;
		}
		if self
			.server_actions
			.invite
			.as_ref()
			.is_some_and(|(id, _, _, _, _)| *id == guild)
		{
			self.server_actions.invite = None;
			self.server_actions.sent.clear();
		}
	}
	pub fn can_create_server_invite(&self, guild: Id, channel: Id) -> bool {
		self.channel(channel)
			.is_some_and(|c| c.guild == Some(guild) && matches!(c.kind, 0 | 2 | 5 | 13 | 15 | 16))
			&& self.permission(channel, VIEW_CHANNEL | CREATE_INSTANT_INVITE) == Some(true)
	}
	pub fn invite_channel(&self, guild: Id) -> Option<Id> {
		self.selected
			.filter(|id| self.can_create_server_invite(guild, *id))
			.or_else(|| {
				self.channels
					.iter()
					.find(|c| self.can_create_server_invite(guild, c.id))
					.map(|c| c.id)
			})
	}
	pub fn leave_server_reason(&self, guild: Id) -> Option<&'static str> {
		if self.guild(guild).is_none() {
			return Some("Server is no longer available");
		}
		let Some(owner) = self.permissions.guilds.get(&guild).and_then(|g| g.owner) else {
			return Some("Server ownership is not available yet");
		};
		let Some(user) = self.user.as_ref() else {
			return Some("Sign in before leaving a server");
		};
		if owner == user.id {
			return Some("Transfer ownership in Discord before leaving this server");
		}
		if self.pending.iter().any(|p| {
			self.channel(p.channel)
				.is_some_and(|c| c.guild == Some(guild))
		}) || self
			.voice
			.active
			.as_ref()
			.is_some_and(|call| call.guild == Some(guild))
		{
			return Some("Finish pending messages and leave the call before leaving this server");
		}
		None
	}
	pub fn create_server_invite(&mut self, guild: Id, channel: Id) -> Option<Command> {
		self.create_server_invite_with_options(guild, channel, InviteOptions::default())
	}
	pub fn create_server_invite_with_options(
		&mut self,
		guild: Id,
		channel: Id,
		options: InviteOptions,
	) -> Option<Command> {
		if !options.valid() || self.server_invite_pending() {
			return None;
		}
		if !self.can_create_server_invite(guild, channel) {
			self.server_actions.status = Some((
				guild,
				"You need Create Invite permission in a visible channel",
			));
			return None;
		}
		self.request_server_action(Action::CreateInvite {
			guild,
			channel,
			options,
		})
	}
	pub fn leave_server(&mut self, guild: Id) -> Option<Command> {
		if let Some(reason) = self.leave_server_reason(guild) {
			self.server_actions.status = Some((guild, reason));
			return None;
		}
		if self.server_actions.leaves.contains(&guild)
			|| self
				.server_actions
				.pending
				.as_ref()
				.is_some_and(|(action, ..)| action.guild() == guild)
		{
			return None;
		}
		if !self.demo && (self.auth != AuthState::Authenticated || !self.gateway_connected) {
			self.server_actions.status =
				Some((guild, "Server actions unavailable while disconnected"));
			return None;
		}
		let guild_record = self.guild(guild)?.clone();
		let channels: Vec<model::Channel> = self
			.channels
			.iter()
			.filter(|c| c.guild == Some(guild))
			.cloned()
			.collect();
		let perm_guild = self.permissions.guilds.get(&guild).cloned();
		let perm_channels: Vec<(Id, model::permissions::Channel)> = channels
			.iter()
			.filter_map(|c| {
				self.permissions
					.channels
					.get(&c.id)
					.map(|p| (c.id, p.clone()))
			})
			.collect();
		let guild_label = self.guild_toast_label(guild);
		let request = self
			.server_actions
			.leaves
			.insert(
				guild,
				LeftServerSnapshot {
					guild: guild_record,
					channels,
					perm_guild,
					perm_channels,
					observed: false,
					guild_label,
				},
			)
			.ok()?;
		self.remove_server(guild);
		self.revision = self.revision.wrapping_add(1);
		Some(Command::ServerAction {
			action: Action::Leave(guild),
			request,
		})
	}
	pub fn can_delete_server(&self, guild: Id) -> bool {
		self.guild(guild).is_some()
			&& self.user.as_ref().is_some_and(|user| {
				self.permissions
					.guilds
					.get(&guild)
					.and_then(|permissions| permissions.owner)
					== Some(user.id)
			})
	}
	pub fn delete_server_reason(&self, guild: Id) -> Option<&'static str> {
		if !self.can_delete_server(guild) {
			return Some("Only the server owner can delete this server");
		}
		if self.server_settings.pending
			|| self.server_admin.pending
			|| self.pending.iter().any(|pending| {
				self.channel(pending.channel)
					.is_some_and(|channel| channel.guild == Some(guild))
			}) || self
			.voice
			.active
			.as_ref()
			.is_some_and(|call| call.guild == Some(guild))
		{
			return Some("Finish pending changes, messages and calls before deleting this server");
		}
		None
	}
	pub fn delete_server(&mut self, guild: Id) -> Option<Command> {
		if let Some(reason) = self.delete_server_reason(guild) {
			self.server_actions.status = Some((guild, reason));
			return None;
		}
		self.request_server_action(Action::Delete(guild))
	}
	fn request_server_action(&mut self, action: Action) -> Option<Command> {
		if self.server_action_pending() || self.server_invite_pending() {
			return None;
		}
		if !self.demo && (self.auth != AuthState::Authenticated || !self.gateway_connected) {
			self.server_actions.status = Some((
				action.guild(),
				"Server actions unavailable while disconnected",
			));
			return None;
		}
		self.server_actions.sequence = self.server_actions.sequence.wrapping_add(1);
		let request = self.server_actions.sequence;
		self.server_actions.pending = Some((action, request, false));
		self.clear_server_action_result(action.guild());
		Some(Command::ServerAction { action, request })
	}
	pub(crate) fn cancel_server_action(&mut self) {
		for ((guild, user), _) in self.server_actions.sending.cancel_all() {
			self.server_actions
				.sent
				.insert(user, InviteStatus::Failed(Failure::Ambiguous));
			self.server_actions.status = Some((guild, Failure::Ambiguous.label()));
		}
		let _ = self.server_actions.notifications.cancel_all();
		self.server_actions.notification_meta.clear();
		for (guild, snapshot) in self.server_actions.leaves.cancel_all() {
			self.restore_left_server(snapshot);
			self.server_actions.status =
				Some((guild, "Outcome unknown; check Discord before retrying"));
		}
		if let Some((action, _, _)) = self.server_actions.pending.take() {
			self.server_actions.status = Some((
				action.guild(),
				"Outcome unknown; check Discord before retrying",
			));
		}
		self.revision = self.revision.wrapping_add(1);
	}
	pub(crate) fn observe_server_joined(&mut self, guild: Id) {
		if let Some(snapshot) = self.server_actions.leaves.get_mut(&guild) {
			snapshot.observed = true;
		}
		if let Some((Action::Leave(id), _, observed)) = &mut self.server_actions.pending
			&& *id == guild
		{
			*observed = true;
		}
	}
	fn restore_left_server(&mut self, snapshot: LeftServerSnapshot) {
		if snapshot.observed || self.guild(snapshot.guild.id).is_some() {
			return;
		}
		if self.guilds.len() < crate::MAX_NAV {
			self.guilds.push(snapshot.guild);
		}
		for ch in snapshot.channels {
			if self.channel(ch.id).is_none() && self.channels.len() < crate::MAX_NAV {
				self.channels.push(ch);
			}
		}
		if let Some(pg) = snapshot.perm_guild {
			self.permissions.guilds.insert(pg.id, pg);
		}
		for (cid, pc) in snapshot.perm_channels {
			self.permissions.channels.insert(cid, pc);
		}
		self.permissions.clear_cache();
		self.invalidate_navigation();
	}
	pub(crate) fn apply_server_action(&mut self, event: Event) -> Result<(), &'static str> {
		if let Event::InviteSent {
			guild,
			user,
			request,
			result,
		} = event
		{
			return self.apply_server_invite_sent(guild, user, request, result.map(|sent| *sent));
		}
		let Event::Written {
			action,
			request,
			result,
		} = event
		else {
			unreachable!()
		};
		if let Action::Notifications { guild, options } = action {
			let valid_res = match result {
				Ok(None) => Ok(()),
				Ok(Some(_)) => Err(Failure::Ambiguous),
				Err(failure) => Err(failure),
			};
			match self
				.server_actions
				.notifications
				.finish(&guild, request, valid_res)
			{
				Finish::Stale => return Ok(()),
				Finish::Settled { .. } => {
					let meta = self.server_actions.notification_meta.remove(&guild);
					let observed = meta.as_ref().is_some_and(|m| m.observed);
					if !observed && self.guild(guild).is_some() {
						self.confirm_server_notifications(guild, options)?;
					}
					self.server_actions.status = None;
					self.revision = self.revision.wrapping_add(1);
				}
				Finish::DispatchFollowUp { seq, value } => {
					let observed = self
						.server_actions
						.notification_meta
						.get(&guild)
						.is_some_and(|m| m.observed);
					if let Some(meta) = self.server_actions.notification_meta.get_mut(&guild) {
						meta.observed = false;
					}
					let confirmed = self
						.server_actions
						.notifications
						.entry(&guild)
						.map(|e| e.confirmed)
						.unwrap_or_default();
					if !observed && self.guild(guild).is_some() {
						self.confirm_server_notifications(guild, options)?;
					}
					let sparse = value.sparse_diff(confirmed);
					let options = if sparse.valid() { sparse } else { value };
					self.push_follow_up(Command::ServerAction {
						action: Action::Notifications { guild, options },
						request: seq,
					});
				}
				Finish::RolledBack { failure, .. } => {
					let meta = self.server_actions.notification_meta.remove(&guild);
					let label = meta
						.map(|m| m.guild_label)
						.unwrap_or_else(|| self.guild_toast_label(guild));
					self.server_actions.status = Some((guild, failure.label()));
					self.status = failure.label();
					if failure.ends_session() && failure != Failure::Capacity {
						self.fail(failure);
					} else {
						self.push_write_error(
							ActionKind::UpdateServerNotifications,
							label,
							failure,
						);
					}
					self.revision = self.revision.wrapping_add(1);
				}
			}
			return Ok(());
		}
		if let Action::Leave(guild) = action {
			let Some(snapshot) = self.server_actions.leaves.finish(&guild, request) else {
				return Ok(());
			};
			let valid_res = match result {
				Ok(None) => Ok(()),
				Ok(Some(_)) => Err(Failure::Ambiguous),
				Err(failure) => Err(failure),
			};
			match valid_res {
				Ok(()) => {
					if !snapshot.observed {
						self.remove_server(guild);
					}
					self.server_actions.status = None;
				}
				Err(failure) => {
					let label = snapshot.guild_label.clone();
					self.restore_left_server(snapshot);
					self.server_actions.status = Some((guild, failure.label()));
					self.status = failure.label();
					if failure.ends_session() && failure != Failure::Capacity {
						self.fail(failure);
					} else {
						self.push_write_error(ActionKind::LeaveServer, label, failure);
					}
				}
			}
			self.revision = self.revision.wrapping_add(1);
			return Ok(());
		}
		let Some((pending, sequence, observed)) = self.server_actions.pending else {
			return Ok(());
		};
		if pending != action || sequence != request {
			return Ok(());
		}
		self.server_actions.pending = None;
		let result = result.and_then(|code| match action {
			Action::CreateInvite { .. }
				if code.as_ref().is_some_and(|code| {
					crate::invites::valid_code(code) && code.capacity() <= 1024
				}) =>
			{
				Ok(code)
			}
			Action::Delete(_) if code.is_none() => Ok(None),
			_ => Err(Failure::Ambiguous),
		});
		let status = match result {
			Err(failure) => {
				if failure.ends_session() && failure != Failure::Capacity {
					self.fail(failure);
				} else {
					let label = self.guild_toast_label(action.guild());
					let action = match action {
						Action::CreateInvite { .. } => ActionKind::SendInvite,
						Action::Delete(_) => ActionKind::DeleteServer,
						_ => ActionKind::UpdateServer,
					};
					self.push_write_error(action, label, failure);
				}
				failure.label()
			}
			Ok(code) => match action {
				Action::CreateInvite {
					guild,
					channel,
					options,
				} => {
					if self.can_create_server_invite(guild, channel) {
						self.server_actions.sent.clear();
						self.server_actions.invite = Some((
							guild,
							channel,
							format!("https://discord.gg/{}", code.unwrap()),
							Instant::now(),
							options,
						));
						"Invite created"
					} else {
						"Invite created, but channel access changed; check Discord"
					}
				}
				Action::Delete(guild) => {
					if observed {
						"Request completed; latest server membership shown"
					} else {
						self.remove_server(guild);
						"Deleted server"
					}
				}
				_ => "Saved",
			},
		};
		self.server_actions.status = Some((action.guild(), status));
		self.status = status;
		Ok(())
	}
	fn remove_server(&mut self, guild: Id) {
		let removed = self
			.channels
			.iter()
			.filter(|c| c.guild == Some(guild))
			.map(|c| c.id)
			.collect();
		self.remove_channels(&removed);
		if self.selected.is_some_and(|id| removed.contains(&id)) {
			self.arrived_home();
		}
		self.guilds.retain(|g| g.id != guild);
		self.permissions.guilds.remove(&guild);
		self.permissions.clear_cache();
		self.invalidate_navigation();
		self.clear_server_action_result(guild);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Envelope, Event as CoreEvent};
	fn state() -> State {
		let mut state = State {
			user: Some(model::User {
				primary_guild: None,
				cosmetics: None,
				id: Id(1),
				name: "Synthetic".into(),
				avatar: None,
				webhook: false,
				kind: Default::default(),
				discriminator: 0,
			}),
			guilds: vec![model::Guild {
				banner: None,
				default_message_notifications: None,
				stickers: None,
				id: Id(2),
				name: "Synthetic server".into(),
				icon: None,
				emojis: None,
			}],
			channels: vec![model::Channel {
				user_limit: None,
				slowmode: 0,
				id: Id(3),
				guild: Some(Id(2)),
				name: "general".into(),
				kind: 0,
				parent_id: None,
				position: 0,
				recipients: vec![],
				last_message: None,
				icon: None,
				member_list_id: None,
				tags: None,
				message_count: None,
			}],
			selected: Some(Id(3)),
			auth: AuthState::Authenticated,
			gateway_connected: true,
			..State::default()
		};
		state
			.permissions
			.replace(model::permissions::Snapshot {
				guilds: vec![model::permissions::Guild {
					id: Id(2),
					owner: Some(Id(9)),
					roles: Some(vec![model::permissions::Role {
						id: Id(2),
						bits: VIEW_CHANNEL | CREATE_INSTANT_INVITE,
						name: String::new(),
						color: 0,
						secondary_color: None,
						tertiary_color: None,
						position: 0,
						hoist: false,
						managed: false,
					}]),
					member: Some(model::permissions::Member {
						roles: vec![],
						timeout_until: None,
					}),
				}],
				channels: vec![model::permissions::Channel {
					id: Id(3),
					guild: Id(2),
					overwrites: Some(vec![]),
				}],
			})
			.unwrap();
		state
	}
	fn finish(state: &mut State, command: Command, result: Result<Option<String>, Failure>) {
		let Command::ServerAction { action, request } = command else {
			panic!("wrong command");
		};
		let event = CoreEvent::ServerAction(Event::Written {
			action,
			request,
			result,
		});
		if matches!(
			&event,
			CoreEvent::ServerAction(Event::Written {
				action: Action::Leave(_),
				result: Ok(None),
				..
			})
		) {
			assert!(
				event.changes_access(),
				"confirmed leave must invalidate cached access"
			);
		}
		state.apply(Envelope {
			generation: state.generation,
			event,
		});
	}
	#[test]
	fn notification_level_acknowledgement_preserves_timed_server_mute() {
		let mut state = state();
		let guild = Id(2);
		let expires = i64::MAX;
		state
			.apply_notification_preferences(crate::notifications::Event::Settings {
				entries: vec![crate::notifications::Setting {
					guild: Some(guild),
					muted: Some(true),
					mute_until: Some(expires),
					..Default::default()
				}],
				replace: false,
			})
			.unwrap();
		let command = state
			.update_server_notifications(
				guild,
				NotificationOptions {
					level: Some(1),
					..Default::default()
				},
			)
			.unwrap();
		finish(&mut state, command, Ok(None));
		let setting = state.notification_preferences.setting(Some(guild)).unwrap();
		assert_eq!(setting.level, Some(1));
		assert_eq!(setting.mute_until, Some(expires));
	}
	#[test]
	fn notification_writes_need_membership_not_admin_and_confirm_after_unrelated_events() {
		let mut state = state();
		state.permissions.guilds.clear(); // An ordinary member need not manage the guild.
		let edit = NotificationOptions {
			level: Some(1),
			..Default::default()
		};
		assert!(state.update_server_notifications(Id(99), edit).is_none());
		assert!(
			state
				.update_server_notifications(Id(2), NotificationOptions::default())
				.is_none()
		);
		assert!(
			state
				.update_server_notifications(
					Id(2),
					NotificationOptions {
						level: Some(4),
						..edit
					}
				)
				.is_none()
		);
		let first = state.update_server_notifications(Id(2), edit).unwrap();
		assert!(state.update_server_notifications(Id(2), edit).is_none());
		finish(&mut state, first, Err(Failure::Forbidden));
		assert_eq!(state.server_notification_settings(Id(2)).level, None);
		let first = state.update_server_notifications(Id(2), edit).unwrap();
		finish(&mut state, first, Ok(None));
		assert_eq!(state.server_notification_settings(Id(2)).level, Some(1));
		let old = state
			.update_server_notifications(
				Id(2),
				NotificationOptions {
					level: Some(0),
					..Default::default()
				},
			)
			.unwrap();
		state
			.apply_notification_preferences(crate::notifications::Event::Settings {
				entries: vec![crate::notifications::Setting {
					guild: Some(Id(2)),
					level: Some(2),
					suppress_roles: Some(true),
					..Default::default()
				}],
				replace: false,
			})
			.unwrap();
		finish(&mut state, old, Ok(None));
		// Authoritative Gateway update during the in-flight write must not be overwritten.
		assert_eq!(state.server_notification_settings(Id(2)).level, Some(2));
		assert_eq!(
			state.server_notification_settings(Id(2)).suppress_roles,
			Some(true)
		);
		let old = state.update_server_notifications(Id(2), edit).unwrap();
		state.cancel_server_action();
		let newer = state
			.update_server_notifications(
				Id(2),
				NotificationOptions {
					level: Some(0),
					..Default::default()
				},
			)
			.unwrap();
		finish(&mut state, old, Ok(None));
		assert!(state.server_actions.has_fast_pending());
		finish(&mut state, newer, Ok(None));
		assert_eq!(state.server_notification_settings(Id(2)).level, Some(0));
		state.gateway_connected = false;
		assert!(!state.can_update_server_notifications(Id(2)));
		assert!(state.update_server_notifications(Id(2), edit).is_none());
		state.gateway_connected = true;
		let Command::ServerAction { action, request } =
			state.update_server_notifications(Id(2), edit).unwrap()
		else {
			panic!()
		};
		let generation = state.generation;
		state.logout();
		state.apply(Envelope {
			generation,
			event: CoreEvent::ServerAction(Event::Written {
				action,
				request,
				result: Ok(None),
			}),
		});
		assert_eq!(state.server_notification_settings(Id(2)).level, None);
	}

	#[test]
	fn server_invites_options_friends_acknowledgement_and_cancellation() {
		let mut state = state();
		let friend = model::User {
			primary_guild: None,
			cosmetics: None,
			id: Id(8),
			name: "Friend".into(),
			avatar: None,
			webhook: false,
			kind: Default::default(),
			discriminator: 0,
		};
		state
			.apply_user_action(crate::user_actions::Event::Relationships(Some(vec![(
				friend.id, false,
			)])))
			.unwrap();
		state
			.apply_user_action(crate::user_actions::Event::Friends(Some(vec![(
				friend.clone(),
				"friend_user".into(),
			)])))
			.unwrap();
		assert!(state.send_server_invite(Id(2), friend.id).is_none());
		assert!(
			!InviteOptions {
				max_age: 2592001,
				..Default::default()
			}
			.valid()
		);
		assert!(
			!InviteOptions {
				max_uses: 101,
				..Default::default()
			}
			.valid()
		);
		let options = InviteOptions {
			max_age: 0,
			max_uses: 10,
			temporary: true,
		};
		let command = state
			.create_server_invite_with_options(Id(2), Id(3), options)
			.unwrap();
		finish(&mut state, command, Ok(Some("invite_test".into())));
		assert_eq!(state.created_invite_options(Id(2)), Some(options));
		assert!(state.send_server_invite(Id(2), Id(99)).is_none());
		state.drafts.insert(Id(3), "Keep the draft".into());
		let Command::SendServerInvite {
			guild,
			user,
			nonce,
			request,
			..
		} = state.send_server_invite(Id(2), friend.id).unwrap()
		else {
			panic!()
		};
		assert_eq!(
			state.server_invite_status(guild, user),
			Some(InviteStatus::Sent)
		);
		assert!(state.send_server_invite(guild, user).is_none());
		assert!(state.create_server_invite(guild, Id(3)).is_none());
		state.clear_server_action_result(guild);
		assert!(state.created_invite(guild).is_some());
		let mut channel = state.channel(Id(3)).unwrap().clone();
		channel.id = Id(10);
		channel.guild = None;
		channel.kind = 1;
		channel.recipients = vec![friend];
		let mut message = crate::tests::message(100);
		message.channel = channel.id;
		message.author = state.user.clone().unwrap();
		message.nonce = Some(nonce);
		message.content = "https://discord.gg/invite_test".into();
		state
			.apply_server_action(Event::InviteSent {
				guild,
				user,
				request: request + 1,
				result: Ok(Box::new((channel.clone(), message.clone()))),
			})
			.unwrap();
		assert!(state.server_invite_pending());
		let expected_bytes = size_of::<CoreEvent>() + channel.bytes() + message.bytes();
		let event = CoreEvent::ServerAction(Event::InviteSent {
			guild,
			user,
			request,
			result: Ok(Box::new((channel, message))),
		});
		assert_eq!(event.bytes(), expected_bytes);
		state.apply(Envelope {
			generation: state.generation,
			event,
		});
		assert_eq!(
			state.server_invite_status(guild, user),
			Some(InviteStatus::Sent)
		);
		assert!(state.send_server_invite(guild, user).is_none());
		assert_eq!(state.selected, Some(Id(3)));
		assert_eq!(state.drafts[&Id(3)], "Keep the draft");
		assert!(state.channel(Id(10)).is_some());
		let command = state.create_server_invite(guild, Id(3)).unwrap();
		finish(&mut state, command, Ok(Some("new_link".into())));
		let command = state.send_server_invite(guild, user).unwrap();
		state.command_rejected(command);
		assert!(!state.server_invite_pending());
		let command = state.send_server_invite(guild, user).unwrap();
		state.cancel_server_action();
		assert_eq!(
			state.server_invite_status(guild, user),
			Some(InviteStatus::Failed(Failure::Ambiguous))
		);
		assert!(state.send_server_invite(guild, user).is_none());
		state.command_rejected(command);
		assert_eq!(
			state.server_invite_status(guild, user),
			Some(InviteStatus::Failed(Failure::Ambiguous))
		);
	}
	#[test]
	fn server_actions_scope_permissions_and_confirmed_removal() {
		let mut owner = state();
		assert!(!owner.can_delete_server(Id(2)));
		assert!(owner.delete_server(Id(2)).is_none());
		owner.permissions.guilds.get_mut(&Id(2)).unwrap().owner = Some(Id(1));
		assert!(owner.can_delete_server(Id(2)));
		let delete = owner.delete_server(Id(2)).unwrap();
		finish(&mut owner, delete, Ok(None));
		assert!(owner.guild(Id(2)).is_none());
		let mut state = state();
		assert_eq!(state.invite_channel(Id(2)), Some(Id(3)));
		assert_eq!(state.invite_channel(Id(8)), None);
		assert!(state.create_server_invite(Id(8), Id(3)).is_none());
		let invite = state.create_server_invite(Id(2), Id(3)).unwrap();
		assert!(state.leave_server(Id(2)).is_none());
		finish(&mut state, invite, Ok(Some("safe-code_1".into())));
		assert_eq!(
			state.created_invite(Id(2)),
			Some("https://discord.gg/safe-code_1")
		);
		assert_eq!(state.created_invite(Id(8)), None);
		let invite = state.create_server_invite(Id(2), Id(3)).unwrap();
		finish(&mut state, invite, Ok(Some("../malicious".into())));
		assert_eq!(state.created_invite(Id(2)), None);
		assert_eq!(
			state.server_action_status(Id(2)),
			Some(Failure::Ambiguous.label())
		);
		state.permissions.guilds.get_mut(&Id(2)).unwrap().owner = Some(Id(1));
		assert!(state.leave_server(Id(2)).is_none());
		state.permissions.guilds.get_mut(&Id(2)).unwrap().owner = None;
		assert!(state.leave_server(Id(2)).is_none());
		state.permissions.guilds.get_mut(&Id(2)).unwrap().owner = Some(Id(9));
		state.drafts.insert(Id(3), "Keep draft".into());
		let leave = state.leave_server(Id(2)).unwrap();
		assert!(state.guild(Id(2)).is_none());
		finish(&mut state, leave, Err(Failure::Ambiguous));
		assert!(state.guild(Id(2)).is_some());
		let old = state.leave_server(Id(2)).unwrap();
		assert!(state.guild(Id(2)).is_none());
		state.cancel_server_action();
		assert!(state.guild(Id(2)).is_some());
		let new = state.leave_server(Id(2)).unwrap();
		assert!(state.guild(Id(2)).is_none());
		finish(&mut state, old, Ok(None));
		assert!(state.server_actions.has_fast_pending());
		finish(&mut state, new, Ok(None));
		assert!(!state.server_actions.has_fast_pending());
		assert!(state.guild(Id(2)).is_none());
		assert!(state.channel(Id(3)).is_none());
		assert_eq!(state.selected, None);
		assert_eq!(state.drafts[&Id(3)], "Keep draft");
	}
	#[test]
	fn server_actions_reject_stale_sessions_and_keep_rejoined_guilds() {
		let mut state = state();
		let rejoined = state.guild(Id(2)).unwrap().clone();
		let rejoined_channel = state.channel(Id(3)).unwrap().clone();
		let rejoined_perm_guild = state.permissions.guilds.get(&Id(2)).cloned();
		let rejoined_perm_channel = state.permissions.channels.get(&Id(3)).cloned();
		let leave = state.leave_server(Id(2)).unwrap();
		assert!(state.guild(Id(2)).is_none());
		state.guilds.push(rejoined);
		state.channels.push(rejoined_channel);
		if let Some(pg) = rejoined_perm_guild {
			state.permissions.guilds.insert(Id(2), pg);
		}
		if let Some(pc) = rejoined_perm_channel {
			state.permissions.channels.insert(Id(3), pc);
		}
		state.observe_server_joined(Id(2));
		finish(&mut state, leave, Ok(None));
		assert!(state.guild(Id(2)).is_some());
		let invite = state.create_server_invite(Id(2), Id(3)).unwrap();
		state.command_rejected(invite);
		assert!(!state.server_action_pending());
		state.gateway_connected = false;
		assert!(state.create_server_invite(Id(2), Id(3)).is_none());
		state.gateway_connected = true;
		let old = state.create_server_invite(Id(2), Id(3)).unwrap();
		let Command::ServerAction { action, request } = old else {
			unreachable!()
		};
		let generation = state.generation;
		state.logout();
		state.apply(Envelope {
			generation,
			event: CoreEvent::ServerAction(Event::Written {
				action,
				request,
				result: Ok(Some("old".into())),
			}),
		});
		assert_eq!(state.created_invite(Id(2)), None);
	}
}
