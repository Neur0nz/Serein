use crate::optimistic::ActionKind;
use crate::{
	State,
	auth::{AuthState, Failure},
	optimistic::{Cancelled, CoalescedSlot, Finish, SetOutcome, WriteKind},
};
use model::{Freshness, Id, Reaction, ReactionEmoji, User};
use std::collections::{BTreeMap, BTreeSet};

const MAX_REACTION_WRITES: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ReactionKey {
	channel: Id,
	message: Id,
	emoji_id: Option<Id>,
	emoji_name: Option<String>,
}
impl ReactionKey {
	fn new(channel: Id, message: Id, emoji: &ReactionEmoji) -> Self {
		if emoji.id.is_some() {
			Self {
				channel,
				message,
				emoji_id: emoji.id,
				emoji_name: None,
			}
		} else {
			Self {
				channel,
				message,
				emoji_id: None,
				emoji_name: emoji.name.clone(),
			}
		}
	}
}

pub enum Command {
	Read {
		channel: Id,
		message: Id,
		request: u64,
	},
	Set {
		channel: Id,
		message: Id,
		emoji: ReactionEmoji,
		add: bool,
		request: u64,
	},
	Users {
		channel: Id,
		message: Id,
		emoji: ReactionEmoji,
		after: Option<Id>,
		request: u64,
	},
}
pub enum Event {
	Delta {
		channel: Id,
		message: Id,
		user: Id,
		emoji: ReactionEmoji,
		add: bool,
		burst: bool,
	},
	Cleared {
		channel: Id,
		message: Id,
		emoji: Option<ReactionEmoji>,
	},
	Changed {
		channel: Id,
		message: Id,
	},
	Read {
		channel: Id,
		message: Id,
		request: u64,
		result: Result<Vec<Reaction>, Failure>,
	},
	Written {
		channel: Id,
		message: Id,
		request: u64,
		result: Result<(), Failure>,
	},
	Users {
		channel: Id,
		message: Id,
		emoji: ReactionEmoji,
		request: u64,
		result: Result<Vec<User>, Failure>,
	},
}
pub const REACTION_USER_PAGE: usize = 100;
pub const MAX_REACTION_USERS: usize = 1_000;
pub const MAX_REACTION_USER_BYTES: usize = 256 * 1024;

pub struct ReactionUsers {
	pub channel: Id,
	pub message: Id,
	pub emoji: ReactionEmoji,
	pub users: Vec<User>,
	pub loading: bool,
	pub exhausted: bool,
	pub open: bool,
	pub error: Option<&'static str>,
	bytes: usize,
	request: u64,
}
#[derive(Default)]
pub struct Reactions {
	dirty: BTreeSet<Id>,
	read: Option<(Id, u64)>,
	writes: CoalescedSlot<ReactionKey, bool, MAX_REACTION_WRITES>,
	write_emojis: BTreeMap<ReactionKey, ReactionEmoji>,
	// Bounded per-message (confirmed_before, displayed_after) previews for active/awaiting-readback writes.
	previews: BTreeMap<Id, (Vec<Reaction>, Vec<Reaction>)>,
	sequence: u64,
	pub users: Option<ReactionUsers>,
}
impl Reactions {
	pub fn cancel_read(&mut self) {
		if let Some((message, _)) = self.read.take() {
			self.dirty.insert(message);
		}
	}
	pub fn reset(&mut self) {
		self.dirty.clear();
		self.read = None;
		let _ = self.writes.cancel_all();
		self.write_emojis.clear();
		self.previews.clear();
		self.users = None;
		self.sequence = self.sequence.wrapping_add(1);
	}
	pub fn has_fast_pending(&self) -> bool {
		!self.writes.is_empty()
	}
	pub fn busy(&self) -> bool {
		self.writes.len() >= MAX_REACTION_WRITES
	}
	fn has_write_for(&self, message: Id) -> bool {
		self.writes.iter().any(|(k, _)| k.message == message)
	}
	pub fn display<'a>(&'a self, message: &'a model::Message) -> Option<&'a [Reaction]> {
		self.previews
			.get(&message.id)
			.map(|(_, after)| after.as_slice())
			.or(message.reactions.as_deref())
	}
	pub fn invalidated(&self, id: Id) -> bool {
		self.dirty.contains(&id) || self.read.is_some_and(|(message, _)| message == id)
	}
	fn recompute_preview(&mut self, channel: Id, message: Id) {
		let Some((before, after)) = self.previews.get_mut(&message) else {
			return;
		};
		let mut computed = before.clone();
		for (key, entry) in self.writes.iter() {
			if key.channel != channel || key.message != message {
				continue;
			}
			let Some(emoji) = self.write_emojis.get(key) else {
				continue;
			};
			apply_optimistic_toggle(&mut computed, emoji, entry.desired);
		}
		*after = computed;
	}
}
fn apply_optimistic_toggle(values: &mut Vec<Reaction>, emoji: &ReactionEmoji, desired_me: bool) {
	if let Some(reaction) = values.iter_mut().find(|r| r.emoji.same(emoji)) {
		if reaction.me != desired_me {
			reaction.count = if desired_me {
				reaction.count.saturating_add(1)
			} else {
				reaction.count.saturating_sub(1)
			};
			reaction.me = desired_me;
		}
	} else if desired_me && values.len() < model::MAX_REACTIONS {
		values.push(Reaction {
			emoji: emoji.clone(),
			count: 1,
			me: true,
			me_burst: false,
		});
	}
	values.retain(|r| r.count > 0);
}
impl State {
	pub fn request_reaction_users(
		&mut self,
		message: Id,
		emoji: ReactionEmoji,
		open: bool,
	) -> Option<crate::Command> {
		if self.auth != AuthState::Authenticated
			|| !self.gateway_connected
			|| self.freshness != Freshness::Fresh
			|| !emoji.valid()
			|| emoji.name.is_none()
		{
			return None;
		}
		let channel = self.selected?;
		let source = self.timeline.get(message)?;
		if !self.can_read_history(channel)
			|| source.channel != channel
			|| !source
				.reactions
				.as_ref()?
				.iter()
				.any(|reaction| reaction.emoji.same(&emoji))
		{
			return None;
		}
		if let Some(details) = &mut self.reactions.users
			&& details.message == message
			&& details.emoji.same(&emoji)
		{
			details.open |= open;
			return None;
		}
		self.reactions.sequence = self.reactions.sequence.wrapping_add(1);
		let request = self.reactions.sequence;
		self.reactions.users = Some(ReactionUsers {
			channel,
			message,
			emoji: emoji.clone(),
			users: Vec::new(),
			loading: true,
			exhausted: false,
			open,
			error: None,
			bytes: 0,
			request,
		});
		Some(crate::Command::Reactions(Command::Users {
			channel,
			message,
			emoji,
			after: None,
			request,
		}))
	}
	pub fn next_reaction_users_page(&mut self) -> Option<crate::Command> {
		let details = self.reactions.users.as_mut()?;
		if details.loading || details.exhausted || details.users.len() >= MAX_REACTION_USERS {
			return None;
		}
		details.loading = true;
		details.error = None;
		self.reactions.sequence = self.reactions.sequence.wrapping_add(1);
		details.request = self.reactions.sequence;
		Some(crate::Command::Reactions(Command::Users {
			channel: details.channel,
			message: details.message,
			emoji: details.emoji.clone(),
			after: details.users.last().map(|user| user.id),
			request: details.request,
		}))
	}
	pub fn close_reaction_users(&mut self) {
		self.reactions.users = None;
	}
	fn update_reactions(
		&mut self,
		channel: Id,
		message: Id,
		update: impl Fn(&mut Vec<Reaction>) -> Result<(), &'static str>,
	) -> Result<(), &'static str> {
		if self
			.reactions
			.users
			.as_ref()
			.is_some_and(|details| details.message == message && !details.open)
		{
			self.reactions.users = None;
		}
		if self.selected != Some(channel)
			|| !self.can_view(channel)
			|| self.freshness == Freshness::Unavailable
			|| !self.gateway_connected
		{
			return Ok(());
		}
		let preview = self.reactions.previews.get(&message);
		let known = self
			.timeline
			.get(message)
			.and_then(|m| m.reactions.as_ref())
			.or_else(|| preview.map(|(before, _)| before));
		let Some(mut values) = known.cloned() else {
			// A delta cannot reconstruct a missing snapshot. Keep the bounded readback.
			self.refresh_reactions(message);
			return Ok(());
		};
		update(&mut values)?;
		let mut updated_preview = preview.cloned();
		if let Some((before, after)) = &mut updated_preview {
			update(before)?;
			if !self.reactions.has_write_for(message) {
				update(after)?;
			}
		}
		// Keep the existing coalesced verification: a snapshot can already include a
		// delayed Gateway event. Paint the delta now, never an empty loading row.
		if !self.queue_reaction_read(message) {
			return Ok(());
		}
		self.timeline.set_reactions(message, Some(values))?;
		if let Some(preview) = updated_preview {
			self.reactions.previews.insert(message, preview);
			if self.reactions.has_write_for(message) {
				self.reactions.recompute_preview(channel, message);
			}
		}
		self.revision += 1;
		Ok(())
	}
	pub fn refresh_reactions(&mut self, message: Id) {
		if self.queue_reaction_read(message) {
			let _ = self.timeline.set_reactions(message, None);
			self.revision += 1;
		}
	}
	pub(super) fn queue_reaction_read(&mut self, message: Id) -> bool {
		if self.timeline.get(message).is_none() && !self.history_pending {
			return false;
		}
		if self.reactions.dirty.len() >= session_cache::MAX_MUTATIONS
			&& !self.reactions.dirty.contains(&message)
		{
			self.fail(Failure::Capacity);
			return false;
		}
		self.reactions.dirty.insert(message);
		true
	}
	pub fn next_reaction_read(&mut self) -> Option<crate::Command> {
		let writes = &self.reactions.writes;
		self.reactions.previews.retain(|id, _| {
			writes.iter().any(|(k, _)| k.message == *id) || self.timeline.get(*id).is_some()
		});
		if self.auth != AuthState::Authenticated
			|| !self.gateway_connected
			|| self.freshness != Freshness::Fresh
			|| self.history_pending
			|| self.reactions.read.is_some()
		{
			return None;
		}
		let channel = self.selected?;
		if !self.can_read_history(channel) {
			return None;
		}
		self.reactions
			.dirty
			.retain(|id| self.timeline.get(*id).is_some());
		let message = self
			.reactions
			.dirty
			.iter()
			.copied()
			.find(|id| !self.reactions.has_write_for(*id))?;
		self.reactions.dirty.remove(&message);
		self.reactions.sequence = self.reactions.sequence.wrapping_add(1);
		let request = self.reactions.sequence;
		self.reactions.read = Some((message, request));
		Some(crate::Command::Reactions(Command::Read {
			channel,
			message,
			request,
		}))
	}
	pub(crate) fn cancel_reaction_pre_wire(&mut self, request: u64) {
		let Some((key, outcome)) = self.reactions.writes.cancelled_by_seq(request) else {
			return;
		};
		match outcome {
			Cancelled::Stale => {}
			Cancelled::Settled => {
				self.reactions.write_emojis.remove(&key);
				if self.reactions.has_write_for(key.message) {
					self.reactions.recompute_preview(key.channel, key.message);
				} else {
					self.reactions.previews.remove(&key.message);
				}
				self.revision += 1;
			}
			Cancelled::DispatchFollowUp { seq, value } => {
				if let Some(emoji) = self.reactions.write_emojis.get(&key).cloned() {
					self.push_follow_up(crate::Command::Reactions(Command::Set {
						channel: key.channel,
						message: key.message,
						emoji,
						add: value,
						request: seq,
					}));
				}
			}
		}
	}
	/// Set the desired membership without toggling a state that changed since approval.
	pub fn prepare_set_reaction(
		&mut self,
		message: Id,
		emoji: ReactionEmoji,
		add: bool,
	) -> Result<Option<crate::Command>, &'static str> {
		const UNAVAILABLE: &str =
			"This reaction is unavailable with the current access or pending action";
		if !emoji.valid()
			|| emoji.name.is_none()
			|| self.timeline.is_deleted(message)
			|| !self.can_react(message, Some(&emoji), add)
		{
			return Err(UNAVAILABLE);
		}
		let current_list = self
			.reactions
			.previews
			.get(&message)
			.map(|(_, after)| after.as_slice())
			.or_else(|| {
				self.timeline
					.get(message)
					.and_then(|message| message.reactions.as_deref())
			})
			.ok_or(UNAVAILABLE)?;
		if current_list
			.iter()
			.any(|reaction| reaction.emoji.same(&emoji) && reaction.me)
			== add
		{
			return Ok(None);
		}
		if let Some(command) = self.prepare_reaction(message, emoji.clone()) {
			return Ok(Some(command));
		}
		if self.selected.is_some_and(|channel| {
			self.reactions
				.writes
				.entry(&ReactionKey::new(channel, message, &emoji))
				.is_some()
		}) {
			return Ok(None);
		}
		Err(UNAVAILABLE)
	}
	pub fn prepare_reaction(
		&mut self,
		message: Id,
		emoji: ReactionEmoji,
	) -> Option<crate::Command> {
		if self.auth != AuthState::Authenticated
			|| !self.gateway_connected
			|| self.freshness != Freshness::Fresh
			|| !emoji.valid()
			|| emoji.name.is_none()
		{
			return None;
		}
		let channel = self.selected?;
		if self.timeline.is_deleted(message) {
			return None;
		}
		let key = ReactionKey::new(channel, message, &emoji);
		let before_list = self
			.reactions
			.previews
			.get(&message)
			.map(|(before, _)| before.clone())
			.or_else(|| self.timeline.get(message)?.reactions.clone())?;
		let displayed_list = self
			.reactions
			.previews
			.get(&message)
			.map(|(_, after)| after.as_slice())
			.unwrap_or(before_list.as_slice());
		let existing_displayed = displayed_list.iter().find(|r| r.emoji.same(&emoji));
		if existing_displayed.is_none() && displayed_list.len() >= model::MAX_REACTIONS {
			self.status = "Reaction limit reached";
			return None;
		}
		let add = existing_displayed.is_none_or(|r| !r.me);
		if !self.can_react(message, Some(&emoji), add) {
			return None;
		}
		if add
			&& let Some(r) = existing_displayed
			&& r.count == u32::MAX
		{
			return None;
		}
		let confirmed_me = before_list.iter().any(|r| r.emoji.same(&emoji) && r.me);
		let outcome = self.reactions.writes.set(key.clone(), confirmed_me, add);
		match outcome {
			SetOutcome::Full => {
				self.status = "Too many pending reactions";
				None
			}
			SetOutcome::Dispatch(seq) => {
				self.reactions.write_emojis.insert(key, emoji.clone());
				if !self.reactions.previews.contains_key(&message) {
					if self.reactions.previews.len() >= MAX_REACTION_WRITES {
						let evict = self
							.reactions
							.previews
							.keys()
							.copied()
							.find(|id| !self.reactions.has_write_for(*id));
						if let Some(evict) = evict {
							self.reactions.previews.remove(&evict);
						}
					}
					self.reactions
						.previews
						.insert(message, (before_list.clone(), before_list));
				}
				if self.reactions.read.is_some_and(|(id, _)| id == message) {
					self.reactions.cancel_read();
				}
				self.reactions.recompute_preview(channel, message);
				self.revision += 1;
				Some(crate::Command::Reactions(Command::Set {
					channel,
					message,
					emoji,
					add,
					request: seq,
				}))
			}
			SetOutcome::Queued {
				cancel_in_flight: Some(cancel_seq),
			} => {
				self.reactions.write_emojis.entry(key).or_insert(emoji);
				self.reactions.recompute_preview(channel, message);
				self.revision += 1;
				Some(crate::Command::CancelPendingWrite {
					kind: WriteKind::Reaction,
					request: cancel_seq,
				})
			}
			SetOutcome::Queued {
				cancel_in_flight: None,
			} => {
				self.reactions.write_emojis.entry(key).or_insert(emoji);
				self.reactions.recompute_preview(channel, message);
				self.revision += 1;
				None
			}
		}
	}
	pub fn apply_reactions(&mut self, event: Event) -> Result<(), &'static str> {
		match event {
			Event::Delta {
				channel,
				message,
				user,
				emoji,
				add,
				burst,
			} => {
				if !emoji.valid() || user.0 == 0 {
					return Err("Invalid reaction delta");
				}
				let own = self.user.as_ref().is_some_and(|me| me.id == user);
				if own && !burst {
					let key = ReactionKey::new(channel, message, &emoji);
					self.reactions.writes.observe_authoritative(&key, add);
				}
				self.update_reactions(channel, message, |values| {
					apply_delta(values, &emoji, own, add, burst)
				})?;
			}
			Event::Cleared {
				channel,
				message,
				emoji,
			} => {
				if emoji.as_ref().is_some_and(|e| !e.valid()) {
					return Err("Invalid reaction emoji");
				}
				let matching_keys: Vec<ReactionKey> = self
					.reactions
					.writes
					.iter()
					.filter_map(|(k, _)| {
						if k.channel != channel || k.message != message {
							return None;
						}
						let matches_emoji = emoji.as_ref().is_none_or(|e| {
							self.reactions
								.write_emojis
								.get(k)
								.is_some_and(|we| we.same(e))
						});
						matches_emoji.then(|| k.clone())
					})
					.collect();
				for key in matching_keys {
					self.reactions.writes.observe_authoritative(&key, false);
				}
				self.update_reactions(channel, message, |values| {
					values.retain(|r| emoji.as_ref().is_some_and(|e| !r.emoji.same(e)));
					Ok(())
				})?;
			}
			Event::Changed { channel, message } => {
				if self.selected == Some(channel)
					&& self.can_view(channel)
					&& self.freshness != Freshness::Unavailable
					&& self.gateway_connected
				{
					self.refresh_reactions(message);
				}
			}
			Event::Read {
				channel,
				message,
				request,
				result,
			} => {
				if self.selected != Some(channel) || self.reactions.read != Some((message, request))
				{
					return Ok(());
				}
				self.reactions.read = None;
				if !self.gateway_connected
					|| self.freshness == Freshness::Unavailable
					|| !self.can_read_history(channel)
				{
					return Ok(());
				}
				match result {
					Ok(reactions) if !self.reactions.dirty.contains(&message) => {
						self.timeline
							.set_reactions(message, Some(reactions.clone()))?;
						if self.reactions.has_write_for(message) {
							self.reactions
								.previews
								.insert(message, (reactions.clone(), reactions));
							self.reactions.recompute_preview(channel, message);
						} else {
							self.reactions.previews.remove(&message);
						}
					}
					Ok(_) => {} // A newer invalidation schedules one fresh read, never an old snapshot.
					Err(failure) => {
						if !self.reactions.has_write_for(message)
							&& let Some((_, after)) = self.reactions.previews.remove(&message)
						{
							self.timeline.set_reactions(message, Some(after))?;
						}
						self.reactions.dirty.remove(&message); // No retry storm on a rejected read.
						self.status = if self
							.timeline
							.get(message)
							.is_some_and(|m| m.reactions.is_some())
						{
							"Reaction counts could not be refreshed; reload the conversation to retry"
						} else {
							"Reactions unavailable; use Reload reactions to retry"
						};
						if failure == Failure::Forbidden {
							self.apply(crate::Envelope {
								generation: self.generation,
								event: crate::Event::Unavailable(channel),
							});
						}
						if failure.ends_session() {
							self.fail(failure);
						}
					}
				}
			}
			Event::Written {
				channel,
				message,
				request,
				result,
			} => {
				let Some((key, finish)) = self.reactions.writes.finish_by_seq(request, result)
				else {
					return Ok(());
				};
				if key.channel != channel || key.message != message {
					return Ok(());
				}
				match finish {
					Finish::Stale => return Ok(()),
					Finish::Settled { confirmed } => {
						let emoji = self.reactions.write_emojis.remove(&key);
						if let Some(emoji) = &emoji
							&& let Some((before, _)) = self.reactions.previews.get_mut(&message)
						{
							apply_optimistic_toggle(before, emoji, confirmed);
						}
						if self.reactions.has_write_for(message) {
							self.reactions.recompute_preview(channel, message);
						} else if self.selected == Some(channel) {
							self.refresh_reactions(message);
						} else {
							self.reactions.previews.remove(&message);
						}
					}
					Finish::DispatchFollowUp { seq, value } => {
						if let Some(emoji) = self.reactions.write_emojis.get(&key).cloned() {
							if let Some((before, _)) = self.reactions.previews.get_mut(&message) {
								apply_optimistic_toggle(before, &emoji, !value);
							}
							self.reactions.recompute_preview(channel, message);
							self.push_follow_up(crate::Command::Reactions(Command::Set {
								channel,
								message,
								emoji,
								add: value,
								request: seq,
							}));
						}
					}
					Finish::RolledBack { failure, .. } => {
						self.reactions.write_emojis.remove(&key);
						if self.reactions.has_write_for(message) {
							self.reactions.recompute_preview(channel, message);
						} else {
							let before = self
								.reactions
								.previews
								.remove(&message)
								.map(|(before, _)| before);
							if failure == Failure::Ambiguous {
								if self.selected == Some(channel) {
									self.refresh_reactions(message);
								}
							} else if let Some(before) = before
								&& self.selected == Some(channel)
								&& self.timeline.get(message).is_some()
							{
								self.timeline.set_reactions(message, Some(before))?;
							}
						}
						self.revision += 1;
						self.status = failure.label();
						if failure.ends_session() && failure != Failure::Capacity {
							self.fail(failure);
						} else {
							self.push_write_error(ActionKind::Reaction, "", failure);
						}
					}
				}
			}
			Event::Users {
				channel,
				message,
				emoji,
				request,
				result,
			} => {
				let Some(details) = &mut self.reactions.users else {
					return Ok(());
				};
				if details.channel != channel
					|| details.message != message
					|| !details.emoji.same(&emoji)
					|| details.request != request
				{
					return Ok(());
				}
				details.loading = false;
				match result {
					Ok(users) => {
						if users.len() > REACTION_USER_PAGE
							|| users.iter().any(|user| user.id.0 == 0)
						{
							return Err("Invalid reaction users");
						}
						let full_page = users.len() == REACTION_USER_PAGE;
						for user in users {
							if details.users.len() >= MAX_REACTION_USERS {
								break;
							}
							if !details.users.iter().any(|known| known.id == user.id) {
								let bytes = std::mem::size_of::<User>() + user.heap_bytes();
								if details.bytes.saturating_add(bytes) > MAX_REACTION_USER_BYTES {
									details.exhausted = true;
									break;
								}
								details.bytes += bytes;
								details.users.push(user);
							}
						}
						details.exhausted |=
							!full_page || details.users.len() >= MAX_REACTION_USERS;
					}
					Err(failure) => {
						details.error = Some(failure.label());
						if failure.ends_session() {
							self.fail(failure);
						}
					}
				}
			}
		}
		Ok(())
	}
}

fn apply_delta(
	values: &mut Vec<Reaction>,
	emoji: &ReactionEmoji,
	own: bool,
	add: bool,
	burst: bool,
) -> Result<(), &'static str> {
	if let Some(reaction) = values.iter_mut().find(|r| r.emoji.same(emoji)) {
		let me = if burst {
			&mut reaction.me_burst
		} else {
			&mut reaction.me
		};
		// Own Gateway echoes must not double-apply the optimistic toggle or a snapshot.
		if own && *me == add {
			return Ok(());
		}
		reaction.count = if add {
			reaction
				.count
				.checked_add(1)
				.ok_or("Reaction count exceeds capacity")?
		} else {
			reaction.count.saturating_sub(1)
		};
		if own {
			*me = add;
		}
	} else if add {
		if values.len() >= model::MAX_REACTIONS {
			return Err("Reaction data exceeds safe capacity");
		}
		values.push(Reaction {
			emoji: emoji.clone(),
			count: 1,
			me: own && !burst,
			me_burst: own && burst,
		});
	}
	values.retain(|r| r.count > 0);
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use model::{Channel, Message, User, permissions as p};

	#[test]
	fn reaction_users_are_paged_deduplicated_and_session_only() {
		let mut message = crate::tests::message(10);
		let channel = message.channel;
		let id = message.id;
		let emoji = ReactionEmoji {
			id: None,
			name: Some("👍".into()),
		};
		message.reactions = Some(vec![Reaction {
			emoji: emoji.clone(),
			count: 101,
			me: false,
			me_burst: false,
		}]);
		let mut state = State {
			selected: Some(channel),
			user: Some(message.author.clone()),
			channels: vec![Channel {
				user_limit: None,
				slowmode: 0,
				id: channel,
				guild: None,
				parent_id: None,
				kind: 1,
				name: "Synthetic".into(),
				position: 0,
				recipients: vec![],
				last_message: None,
				icon: None,
				member_list_id: None,
				tags: None,
				message_count: None,
			}],
			gateway_connected: true,
			auth: AuthState::Authenticated,
			freshness: Freshness::Fresh,
			..State::default()
		};
		state.timeline.insert(message, false, false).unwrap();
		let Some(crate::Command::Reactions(Command::Users { request, .. })) =
			state.request_reaction_users(id, emoji.clone(), true)
		else {
			panic!()
		};
		let user = |id| User {
			primary_guild: None,
			cosmetics: None,
			id: Id(id),
			name: format!("User {id}"),
			avatar: None,
			discriminator: 0,
			kind: Default::default(),
			webhook: false,
		};
		state
			.apply_reactions(Event::Users {
				channel,
				message: id,
				emoji: emoji.clone(),
				request,
				result: Ok((1..=REACTION_USER_PAGE as u64).map(user).collect()),
			})
			.unwrap();
		let Some(crate::Command::Reactions(Command::Users {
			after: Some(after),
			request,
			..
		})) = state.next_reaction_users_page()
		else {
			panic!()
		};
		assert_eq!(after, Id(REACTION_USER_PAGE as u64));
		state
			.apply_reactions(Event::Users {
				channel,
				message: id,
				emoji,
				request,
				result: Ok(vec![user(100), user(101)]),
			})
			.unwrap();
		let details = state.reactions.users.as_ref().unwrap();
		assert_eq!(details.users.len(), 101);
		assert!(details.exhausted && details.open);
		state.close_reaction_users();
		assert!(state.reactions.users.is_none());
	}

	#[test]
	fn live_reaction_deltas_preserve_counts_echoes_and_history_races() {
		{
			let message = crate::tests::message(10);
			let channel = message.channel;
			let id = message.id;
			let own = message.author.id;
			let emoji = ReactionEmoji {
				id: None,
				name: Some("x".into()),
			};
			let other = ReactionEmoji {
				id: Some(Id(99)),
				name: None,
			};
			let mut state = State {
				selected: Some(channel),
				user: Some(message.author.clone()),
				channels: vec![Channel {
					user_limit: None,
					slowmode: 0,
					id: channel,
					guild: None,
					parent_id: None,
					kind: 1,
					name: "Synthetic".into(),
					position: 0,
					recipients: vec![],
					last_message: None,
					icon: None,
					member_list_id: None,
					tags: None,
					message_count: None,
				}],
				gateway_connected: true,
				auth: AuthState::Authenticated,
				freshness: Freshness::Fresh,
				..State::default()
			};
			state
				.timeline
				.insert(message.clone(), false, false)
				.unwrap();
			let delta = |user, emoji: &ReactionEmoji, add, burst| Event::Delta {
				channel,
				message: id,
				user,
				emoji: emoji.clone(),
				add,
				burst,
			};
			let visible = |s: &State| {
				s.reactions
					.display(s.timeline.get(id).unwrap())
					.unwrap()
					.to_vec()
			};
			state
				.apply_reactions(delta(Id(500), &emoji, true, false))
				.unwrap();
			state
				.apply_reactions(delta(Id(501), &emoji, true, true))
				.unwrap();
			state
				.apply_reactions(delta(Id(500), &other, true, false))
				.unwrap();
			assert_eq!(
				visible(&state).iter().map(|r| r.count).collect::<Vec<_>>(),
				[2, 1]
			);
			assert!(state.reactions.invalidated(id));
			// Another user and our own echo update both sides of the optimistic write.
			let Some(crate::Command::Reactions(Command::Set { request, .. })) =
				state.prepare_reaction(id, emoji.clone())
			else {
				panic!()
			};
			state
				.apply_reactions(delta(Id(502), &emoji, true, false))
				.unwrap();
			assert_eq!(visible(&state)[0].count, 4);
			state
				.apply_reactions(delta(own, &emoji, true, false))
				.unwrap();
			assert_eq!(visible(&state)[0].count, 4);
			assert!(visible(&state)[0].me);
			state
				.apply_reactions(Event::Written {
					channel,
					message: id,
					request,
					result: Err(Failure::RateLimited),
				})
				.unwrap();
			assert_eq!(
				visible(&state)[0].count,
				4,
				"an observed echo survives a late write error"
			);
			// Normal and burst membership are independent, including optimistic removal.
			state
				.apply_reactions(delta(own, &emoji, true, true))
				.unwrap();
			let Some(crate::Command::Reactions(Command::Set {
				request,
				add: false,
				..
			})) = state.prepare_reaction(id, emoji.clone())
			else {
				panic!()
			};
			state
				.apply_reactions(delta(own, &emoji, false, false))
				.unwrap();
			assert_eq!(visible(&state)[0].count, 4);
			assert!(!visible(&state)[0].me && visible(&state)[0].me_burst);
			state
				.apply_reactions(Event::Written {
					channel,
					message: id,
					request,
					result: Ok(()),
				})
				.unwrap();
			let Some(crate::Command::Reactions(Command::Read { request, .. })) =
				state.next_reaction_read()
			else {
				panic!()
			};
			state
				.apply_reactions(delta(Id(503), &emoji, true, false))
				.unwrap();
			assert_eq!(visible(&state)[0].count, 5);
			state
				.apply_reactions(Event::Read {
					channel,
					message: id,
					request,
					result: Ok(vec![]),
				})
				.unwrap();
			assert_eq!(
				visible(&state)[0].count,
				5,
				"stale HTTP reply cannot undo a live update"
			);
			let Some(crate::Command::Reactions(Command::Read { request, .. })) =
				state.next_reaction_read()
			else {
				panic!()
			};
			state
				.apply_reactions(Event::Read {
					channel,
					message: id,
					request,
					result: Ok(visible(&state)),
				})
				.unwrap();
			let _ = state.history(None);
			state
				.apply_reactions(delta(Id(504), &emoji, true, false))
				.unwrap();
			let mut history_message = message;
			history_message.content = "Newer history content".into();
			state.apply(crate::Envelope {
				generation: state.generation,
				event: crate::Event::History {
					channel,
					request: state.request,
					older: false,
					messages: vec![history_message],
				},
			});
			assert_eq!(
				visible(&state)[0].count,
				6,
				"history must preserve a newer live delta"
			);
			assert_eq!(
				state.timeline.get(id).unwrap().content,
				"Newer history content"
			);
			let Some(crate::Command::Reactions(Command::Read { request, .. })) =
				state.next_reaction_read()
			else {
				panic!()
			};
			state
				.apply_reactions(Event::Read {
					channel,
					message: id,
					request,
					result: Ok(visible(&state)),
				})
				.unwrap();
			state
				.apply_reactions(Event::Cleared {
					channel,
					message: id,
					emoji: Some(emoji),
				})
				.unwrap();
			assert_eq!(visible(&state).len(), 1);
			state
				.apply_reactions(delta(Id(500), &other, false, false))
				.unwrap();
			assert!(visible(&state).is_empty());
			state
				.apply_reactions(delta(Id(500), &other, true, false))
				.unwrap();
			state
				.apply_reactions(Event::Cleared {
					channel,
					message: id,
					emoji: None,
				})
				.unwrap();
			assert!(visible(&state).is_empty());
			assert!(state.reactions.invalidated(id));
			// A successful write/readback can arrive before our Gateway echo.
			let named = ReactionEmoji {
				id: None,
				name: Some("x".into()),
			};
			let Some(crate::Command::Reactions(Command::Set { request, .. })) =
				state.prepare_reaction(id, named.clone())
			else {
				panic!()
			};
			state
				.apply_reactions(Event::Written {
					channel,
					message: id,
					request,
					result: Ok(()),
				})
				.unwrap();
			let Some(crate::Command::Reactions(Command::Read { request, .. })) =
				state.next_reaction_read()
			else {
				panic!()
			};
			state
				.apply_reactions(Event::Read {
					channel,
					message: id,
					request,
					result: Ok(visible(&state)),
				})
				.unwrap();
			state
				.apply_reactions(delta(own, &named, true, false))
				.unwrap();
			assert_eq!(visible(&state)[0].count, 1);
			state.timeline.set_reactions(id, None).unwrap();
			state
				.apply_reactions(delta(Id(500), &other, true, false))
				.unwrap();
			assert!(
				state.timeline.get(id).unwrap().reactions.is_none(),
				"unknown is not zero"
			);
			assert!(state.next_reaction_read().is_some());
		}
		{
			let mut message = crate::tests::message(10);
			let channel = message.channel;
			let id = message.id;
			let emoji = ReactionEmoji {
				id: None,
				name: Some("x".into()),
			};
			let snapshot = |count| {
				vec![Reaction {
					emoji: emoji.clone(),
					count,
					me: false,
					me_burst: false,
				}]
			};
			message.reactions = Some(snapshot(1));
			let mut state = State {
				selected: Some(channel),
				user: Some(message.author.clone()),
				channels: vec![Channel {
					user_limit: None,
					slowmode: 0,
					id: channel,
					guild: None,
					parent_id: None,
					kind: 1,
					name: "Synthetic".into(),
					position: 0,
					recipients: vec![],
					last_message: None,
					icon: None,
					member_list_id: None,
					tags: None,
					message_count: None,
				}],
				gateway_connected: true,
				auth: AuthState::Authenticated,
				freshness: Freshness::Fresh,
				..State::default()
			};
			state.timeline.insert(message, false, false).unwrap();
			let delta = || Event::Delta {
				channel,
				message: id,
				user: Id(500),
				emoji: emoji.clone(),
				add: true,
				burst: false,
			};
			let visible =
				|s: &State| s.reactions.display(s.timeline.get(id).unwrap()).unwrap()[0].count;
			assert!(state.queue_reaction_read(id));
			assert_eq!(visible(&state), 1);
			let Some(crate::Command::Reactions(Command::Read { request, .. })) =
				state.next_reaction_read()
			else {
				panic!()
			};
			// The HTTP snapshot includes an add whose Gateway event has not arrived yet.
			state
				.apply_reactions(Event::Read {
					channel,
					message: id,
					request,
					result: Ok(snapshot(2)),
				})
				.unwrap();
			assert_eq!(visible(&state), 2);
			assert!(state.next_reaction_read().is_none());
			state.apply(crate::Envelope {
				generation: state.generation,
				event: crate::Event::Reactions(delta()),
			});
			assert_eq!(visible(&state), 3);
			assert!(state.timeline.get(id).unwrap().reactions.is_some());
			let Some(crate::Command::Reactions(Command::Read {
				channel: read_channel,
				message: read_message,
				request,
			})) = state.next_reaction_read()
			else {
				panic!()
			};
			assert_eq!((read_channel, read_message), (channel, id));
			assert!(
				state.next_reaction_read().is_none(),
				"only one verification is in flight"
			);
			assert_eq!(
				visible(&state),
				3,
				"verification must not hide known counts"
			);
			state
				.apply_reactions(Event::Read {
					channel,
					message: id,
					request,
					result: Ok(snapshot(2)),
				})
				.unwrap();
			assert_eq!(
				visible(&state),
				2,
				"verification repairs cross-transport ordering"
			);
			assert!(state.next_reaction_read().is_none());
			for unknown in [false, true] {
				if unknown {
					state.refresh_reactions(id);
				} else {
					assert!(state.queue_reaction_read(id));
				}
				let Some(crate::Command::Reactions(Command::Read { request, .. })) =
					state.next_reaction_read()
				else {
					panic!()
				};
				let mut replacement = crate::tests::message(id.0);
				replacement.content = "New replacement body".into();
				replacement.reactions = Some(snapshot(999));
				state.apply(crate::Envelope {
					generation: state.generation,
					event: crate::Event::Message(replacement),
				});
				assert_eq!(
					state.timeline.get(id).unwrap().content,
					"New replacement body"
				);
				state.apply(crate::Envelope {
					generation: state.generation,
					event: crate::Event::Patch(model::MessagePatch {
						poll: model::Patch::Absent,
						sticker_items: model::Patch::Absent,
						id,
						channel,
						content: model::Patch::Value("New patch body".into()),
						reactions: model::Patch::Value(snapshot(998)),
						mentions: model::Patch::Absent,
						edited: model::Patch::Absent,
						embeds: model::Patch::Absent,
						embeds_suppressed: model::Patch::Absent,
						attachments: model::Patch::Absent,
						components: model::Patch::Absent,
						flags: model::Patch::Absent,
						application_id: model::Patch::Absent,
						extra_content: Default::default(),
					}),
				});
				assert_eq!(state.timeline.get(id).unwrap().content, "New patch body");
				let expected = if unknown { None } else { Some(snapshot(2)) };
				assert_eq!(state.timeline.get(id).unwrap().reactions, expected);
				state
					.apply_reactions(Event::Read {
						channel,
						message: id,
						request,
						result: Ok(snapshot(997)),
					})
					.unwrap();
				assert_eq!(state.timeline.get(id).unwrap().reactions, expected);
				let Some(crate::Command::Reactions(Command::Read { request, .. })) =
					state.next_reaction_read()
				else {
					panic!()
				};
				state
					.apply_reactions(Event::Read {
						channel,
						message: id,
						request,
						result: Ok(snapshot(2)),
					})
					.unwrap();
				assert_eq!(visible(&state), 2);
			}

			let events = || {
				[
					delta(),
					Event::Cleared {
						channel,
						message: id,
						emoji: None,
					},
				]
			};
			for event in events() {
				state.apply(crate::Envelope {
					generation: state.generation - 1,
					event: crate::Event::Reactions(event),
				});
				assert_eq!(visible(&state), 2);
				assert!(!state.reactions.invalidated(id));
			}
			state.gateway_connected = false;
			for event in events() {
				state.apply_reactions(event).unwrap();
				assert_eq!(visible(&state), 2);
				assert!(!state.reactions.invalidated(id));
			}
			state.gateway_connected = true;
			state.channels[0].guild = Some(Id(99));
			assert!(
				!state.can_view(channel),
				"unknown guild permissions fail closed"
			);
			for event in events() {
				state.apply_reactions(event).unwrap();
				assert_eq!(visible(&state), 2);
				assert!(!state.reactions.invalidated(id));
			}
			state.channels[0].guild = None;
			state.freshness = Freshness::Unavailable;
			for event in events() {
				state.apply_reactions(event).unwrap();
				assert_eq!(visible(&state), 2);
				assert!(!state.reactions.invalidated(id));
			}
			state.freshness = Freshness::Fresh;
			state.selected = Some(Id(99));
			for event in events() {
				state.apply_reactions(event).unwrap();
				assert_eq!(visible(&state), 2);
				assert!(!state.reactions.invalidated(id));
			}
		}
		{
			let mut values: Vec<_> = (1..=model::MAX_REACTIONS)
				.map(|id| Reaction {
					emoji: ReactionEmoji {
						id: Some(Id(id as u64)),
						name: Some("x".repeat(128)),
					},
					count: 1,
					me: false,
					me_burst: false,
				})
				.collect();
			let extra = ReactionEmoji {
				id: Some(Id(1000)),
				name: None,
			};
			let before = values.clone();
			assert!(apply_delta(&mut values, &extra, false, true, false).is_err());
			assert_eq!(
				values, before,
				"a rejected new emoji cannot mutate a full list"
			);
			apply_delta(&mut values, &extra, false, false, false).unwrap();
			assert_eq!(
				values, before,
				"removing an unknown emoji must not create a row"
			);
			let existing = values[0].emoji.clone();
			apply_delta(&mut values, &existing, false, true, false).unwrap();
			assert_eq!(
				values[0].count, 2,
				"a full list still accepts existing emoji updates"
			);
			assert_eq!(values.len(), model::MAX_REACTIONS);
			values[0].count = u32::MAX;
			let before = values.clone();
			assert!(apply_delta(&mut values, &existing, false, true, true).is_err());
			assert_eq!(
				values, before,
				"overflow must not change count or membership"
			);
			values[0].me = true;
			apply_delta(&mut values, &existing, true, true, false).unwrap();
			assert_eq!(
				values[0].count,
				u32::MAX,
				"an own echo remains idempotent at capacity"
			);
			values[0].count = 1;
			apply_delta(&mut values, &existing, true, false, false).unwrap();
			assert!(!values.iter().any(|r| r.emoji.same(&existing)));
			apply_delta(&mut values, &existing, true, false, false).unwrap();
			assert!(model::valid_reactions(&values));
			assert_eq!(values.len(), model::MAX_REACTIONS - 1);
		}
	}

	#[test]
	fn reaction_permissions_distinguish_existing_emoji_and_late_reads() {
		let user = User {
			primary_guild: None,
			cosmetics: None,
			id: Id(2),
			name: "Synthetic".into(),
			avatar: None,
			webhook: false,
			kind: Default::default(),
			discriminator: 0,
		};
		let mut state = State {
			auth: AuthState::Authenticated,
			gateway_connected: true,
			freshness: Freshness::Fresh,
			selected: Some(Id(10)),
			user: Some(user.clone()),
			guilds: vec![model::Guild {
				banner: None,
				default_message_notifications: None,
				stickers: None,
				id: Id(1),
				name: "Synthetic".into(),
				icon: None,
				emojis: None,
			}],
			channels: vec![Channel {
				user_limit: None,
				slowmode: 0,
				id: Id(10),
				guild: Some(Id(1)),
				kind: 0,
				name: "Synthetic".into(),
				parent_id: None,
				last_message: None,
				position: 0,
				recipients: vec![],
				icon: None,
				member_list_id: None,
				tags: None,
				message_count: None,
			}],
			..State::default()
		};
		state
			.permissions
			.replace(p::Snapshot {
				guilds: vec![p::Guild {
					id: Id(1),
					owner: Some(Id(999)),
					roles: Some(vec![p::Role {
						name: String::new(),
						color: 0,
						secondary_color: None,
						tertiary_color: None,
						position: 0,
						hoist: false,
						managed: false,
						id: Id(1),
						bits: p::VIEW_CHANNEL | p::READ_MESSAGE_HISTORY,
					}]),
					member: Some(p::Member {
						roles: vec![],
						timeout_until: None,
					}),
				}],
				channels: vec![p::Channel {
					id: Id(10),
					guild: Id(1),
					overwrites: Some(vec![]),
				}],
			})
			.unwrap();
		let emoji = ReactionEmoji {
			id: None,
			name: Some("a".into()),
		};
		state
			.timeline
			.insert(
				Message {
					poll: None,
					sticker_items: Vec::new(),
					kind: 0,
					id: Id(50),
					channel: Id(10),
					author: user,
					content: "Synthetic".into(),
					reactions: Some(vec![]),
					author_nick: None,
					author_roles: vec![],
					mention_roles: vec![],
					mention_everyone: false,
					suppress_notifications: false,
					mentions: vec![],
					edited: false,
					edited_at: None,
					revision: 0,
					nonce: None,
					reply_to: None,
					reply_deleted: false,
					interaction: None,
					forwarded: false,
					forward_origin: None,
					unsupported: false,
					components: vec![],
					application_id: None,
					flags: 0,
					ephemeral: false,
					extra_content: Default::default(),
					embeds: vec![],
					embeds_suppressed: false,
					attachments: vec![],
				},
				true,
				false,
			)
			.unwrap();
		assert!(state.prepare_reaction(Id(50), emoji.clone()).is_none());
		state
			.timeline
			.set_reactions(
				Id(50),
				Some(vec![Reaction {
					emoji: emoji.clone(),
					count: 1,
					me: false,
					me_burst: false,
				}]),
			)
			.unwrap();
		let Some(crate::Command::Reactions(Command::Set {
			add: true, request, ..
		})) = state.prepare_reaction(Id(50), emoji.clone())
		else {
			panic!("expected reaction add");
		};
		let visible = state
			.reactions
			.display(state.timeline.get(Id(50)).unwrap())
			.unwrap();
		assert!(visible[0].me);
		assert_eq!(visible[0].count, 2);
		state
			.apply_reactions(Event::Written {
				channel: Id(10),
				message: Id(50),
				request,
				result: Err(Failure::Forbidden),
			})
			.unwrap();
		let visible = state
			.reactions
			.display(state.timeline.get(Id(50)).unwrap())
			.unwrap();
		assert!(!visible[0].me);
		assert_eq!(visible[0].count, 1);
		state.reactions.reset();
		state
			.timeline
			.set_reactions(
				Id(50),
				Some(vec![Reaction {
					emoji: emoji.clone(),
					count: 1,
					me: true,
					me_burst: false,
				}]),
			)
			.unwrap();
		let Some(crate::Command::Reactions(Command::Set {
			add: false,
			request,
			..
		})) = state.prepare_reaction(Id(50), emoji.clone())
		else {
			panic!("expected reaction remove");
		};
		assert!(
			state
				.reactions
				.display(state.timeline.get(Id(50)).unwrap())
				.unwrap()
				.is_empty()
		);
		state
			.apply_reactions(Event::Written {
				channel: Id(10),
				message: Id(50),
				request,
				result: Err(Failure::RateLimited),
			})
			.unwrap();
		assert!(
			state
				.reactions
				.display(state.timeline.get(Id(50)).unwrap())
				.unwrap()[0]
				.me
		);
		state.reactions.reset();
		state.refresh_reactions(Id(50));
		let Some(crate::Command::Reactions(Command::Read { request, .. })) =
			state.next_reaction_read()
		else {
			panic!()
		};
		state
			.permissions
			.guilds
			.get_mut(&Id(1))
			.unwrap()
			.roles
			.as_mut()
			.unwrap()[0]
			.bits = p::VIEW_CHANNEL;
		state.permissions.clear_cache();
		state
			.apply_reactions(Event::Read {
				channel: Id(10),
				message: Id(50),
				request,
				result: Ok(vec![Reaction {
					emoji,
					count: 1,
					me: true,
					me_burst: false,
				}]),
			})
			.unwrap();
		assert!(state.timeline.get(Id(50)).unwrap().reactions.is_none());
		state.refresh_reactions(Id(50));
		assert!(state.next_reaction_read().is_none());
		assert!(
			state.can_mark_read(Id(50)),
			"Observed live messages may be marked read without history permission"
		);
		let Some(crate::Command::MarkRead { request, .. }) = state.prepare_mark_read(Id(50)) else {
			panic!()
		};
		state
			.permissions
			.guilds
			.get_mut(&Id(1))
			.unwrap()
			.roles
			.as_mut()
			.unwrap()[0]
			.bits = 0;
		state.permissions.clear_cache();
		state
			.apply_read_state(crate::read_state::Event::Result {
				channel: Id(10),
				message: Id(50),
				request,
				result: Ok(()),
			})
			.unwrap();
		assert!(state.read_marker(Id(10)).is_none());
		assert!(!state.can_mark_read(Id(50)));
		state
			.permissions
			.guilds
			.get_mut(&Id(1))
			.unwrap()
			.roles
			.as_mut()
			.unwrap()[0]
			.bits = p::VIEW_CHANNEL;
		state.permissions.clear_cache();
		assert!(
			state.read_marker(Id(10)).flatten().is_none(),
			"A late success cannot record a revoked read marker"
		);
	}
}
