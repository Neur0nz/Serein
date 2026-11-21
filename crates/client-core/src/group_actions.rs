//! Bounded group writes: optimistic coalesced renames, optimistic confirmed leave, and icon upload.
use crate::optimistic::ActionKind;
use crate::{
	Command, State,
	auth::{AuthState, Failure},
	optimistic::{Cancelled, CoalescedSlot, Finish, InFlightSet, SetOutcome, WriteKind},
};
use model::{ChannelPatch, Id, Patch};
use std::collections::BTreeMap;

pub const MAX_ICON_DATA_URI: usize = 22 + 4 * (256_usize * 1024).div_ceil(3);
const MAX_GROUP_RENAMES: usize = 8;
const MAX_GROUP_LEAVES: usize = 8;
/// High bit separating coalesced group rename sequence IDs from serialized icon edits and leaves.
pub const RENAME_REQUEST_BIT: u64 = 1 << 62;
const LEAVE_REQUEST_BIT: u64 = 1 << 61;

pub enum Action {
	Leave(Id),
	Edit {
		channel: Id,
		name: Option<String>,
		icon: Patch<String>,
	},
}

impl Action {
	pub fn channel(&self) -> Id {
		match self {
			Self::Leave(channel) | Self::Edit { channel, .. } => *channel,
		}
	}
	pub fn valid(&self) -> bool {
		self.channel().0 != 0
			&& match self {
				Self::Leave(_) => true,
				Self::Edit { name, icon, .. } => {
					(name.is_some() || !matches!(icon, Patch::Absent))
						&& name.as_ref().is_none_or(|name| {
							!name.trim().is_empty()
								&& name.chars().count() <= 100
								&& name.capacity() <= 400
								&& !name.chars().any(char::is_control)
						}) && match icon {
						Patch::Value(uri) => {
							uri.capacity() <= MAX_ICON_DATA_URI
								&& uri
									.strip_prefix("data:image/png;base64,")
									.is_some_and(|data| {
										!data.is_empty()
											&& data.bytes().all(|b| {
												b.is_ascii_alphanumeric() || b"+/=".contains(&b)
											})
									})
						}
						_ => true,
					}
				}
			}
	}
}
pub enum Event {
	Written {
		channel: Id,
		request: u64,
		result: Result<Option<ChannelPatch>, Failure>,
	},
}
struct RenameMeta {
	observed: bool,
	channel_label: String,
}
struct LeftGroupSnapshot {
	channel: model::Channel,
	observed: bool,
	channel_label: String,
}
#[derive(Default)]
pub struct Actions {
	sequence: u64,
	// Icon-changing edits stay on `pending`: (channel, request, leaving, renaming, observed)
	pending: Option<(Id, u64, bool, bool, bool)>,
	renames: CoalescedSlot<Id, String, MAX_GROUP_RENAMES>,
	rename_meta: BTreeMap<Id, RenameMeta>,
	leaves: InFlightSet<Id, LeftGroupSnapshot, MAX_GROUP_LEAVES>,
	status: Option<(Id, &'static str)>,
	completed: Option<(Id, u64, bool)>,
}
impl Actions {
	pub(crate) fn reset(&mut self) {
		let mut renames = std::mem::take(&mut self.renames);
		let _ = renames.cancel_all();
		let mut leaves = std::mem::take(&mut self.leaves);
		let _ = leaves.cancel_all();
		*self = Self {
			sequence: self.sequence,
			renames,
			leaves,
			// READY can follow a canceled write without changing session generation.
			// Keep its bounded outcome so an open editor can leave the busy state.
			completed: self.completed,
			status: self.status,
			..Self::default()
		};
	}
	pub(crate) fn has_fast_pending(&self) -> bool {
		self.pending.is_some() || !self.renames.is_empty() || !self.leaves.is_empty()
	}
}
impl State {
	pub fn group_action_pending(&self) -> bool {
		self.group_actions.pending.is_some()
	}
	pub fn group_action_status(&self, channel: Id) -> Option<&'static str> {
		self.group_actions
			.status
			.filter(|(id, _)| *id == channel)
			.map(|(_, text)| text)
	}
	pub fn group_action_completed(&self, channel: Id, request: u64) -> Option<bool> {
		self.group_actions
			.completed
			.filter(|(id, seq, _)| *id == channel && *seq == request)
			.map(|(_, _, success)| success)
	}
	pub fn clear_group_action_result(&mut self, channel: Id) {
		if self
			.group_actions
			.status
			.is_some_and(|(id, _)| id == channel)
		{
			self.group_actions.status = None;
		}
	}
	pub fn is_group_dm(&self, channel: Id) -> bool {
		self.channel(channel)
			.is_some_and(|c| c.guild.is_none() && c.kind == 3)
	}
	pub fn leave_group_reason(&self, channel: Id) -> Option<&'static str> {
		if !self.is_group_dm(channel) {
			return Some("Group is no longer available");
		}
		if self.pending.iter().any(|p| p.channel == channel)
			|| self
				.voice
				.active
				.as_ref()
				.is_some_and(|call| call.channel == channel)
		{
			return Some("Finish pending messages and leave the call before leaving this group");
		}
		None
	}
	pub fn leave_group(&mut self, channel: Id) -> Option<Command> {
		if let Some(reason) = self.leave_group_reason(channel) {
			self.group_actions.status = Some((channel, reason));
			return None;
		}
		if self.group_actions.leaves.contains(&channel)
			|| self.group_actions.renames.entry(&channel).is_some()
			|| self
				.group_actions
				.pending
				.as_ref()
				.is_some_and(|(target, ..)| *target == channel)
		{
			return None;
		}
		if !self.demo && (self.auth != AuthState::Authenticated || !self.gateway_connected) {
			self.group_actions.status =
				Some((channel, "Group actions unavailable while disconnected"));
			return None;
		}
		let record = self.channel(channel)?.clone();
		let label = self.channel_toast_label(channel);
		let request = self
			.group_actions
			.leaves
			.insert(
				channel,
				LeftGroupSnapshot {
					channel: record,
					observed: false,
					channel_label: label,
				},
			)
			.ok()?;
		self.group_actions.status = None;
		self.group_actions.completed = None;
		self.remove_channels(&std::collections::BTreeSet::from([channel]));
		if self.selected == Some(channel) {
			self.arrived_home();
		}
		self.revision = self.revision.wrapping_add(1);
		Some(Command::GroupAction {
			action: Action::Leave(channel),
			request: request | LEAVE_REQUEST_BIT,
		})
	}
	pub fn edit_group(
		&mut self,
		channel: Id,
		name: Option<String>,
		icon: Patch<String>,
	) -> Option<Command> {
		if matches!(icon, Patch::Absent)
			&& let Some(new_name) = name
		{
			return self.optimistic_rename_group(channel, new_name);
		}
		self.request_group_action(Action::Edit {
			channel,
			name,
			icon,
		})
	}
	fn optimistic_rename_group(&mut self, channel: Id, name: String) -> Option<Command> {
		if !self.is_group_dm(channel) {
			return None;
		}
		let action = Action::Edit {
			channel,
			name: Some(name.clone()),
			icon: Patch::Absent,
		};
		if !action.valid() {
			self.group_actions.status = Some((
				channel,
				"Use a group name of 1–100 characters and a supported icon",
			));
			return None;
		}
		if !self.demo && (self.auth != AuthState::Authenticated || !self.gateway_connected) {
			self.group_actions.status =
				Some((channel, "Group actions unavailable while disconnected"));
			return None;
		}
		let current = self.channel(channel)?.name.clone();
		let label = self.channel_toast_label(channel);
		let outcome = self
			.group_actions
			.renames
			.set(channel, current.clone(), name.clone());
		self.group_actions.status = None;
		self.group_actions.completed = None;
		match outcome {
			SetOutcome::Full => {
				self.group_actions.status = Some((channel, "Too many pending group renames"));
				None
			}
			SetOutcome::Dispatch(seq) => {
				self.group_actions.rename_meta.insert(
					channel,
					RenameMeta {
						observed: false,
						channel_label: label,
					},
				);
				if let Some(group) = self
					.channels
					.iter_mut()
					.find(|c| c.id == channel && c.kind == 3 && c.guild.is_none())
				{
					group.name = name.clone();
					self.invalidate_navigation();
				}
				self.revision = self.revision.wrapping_add(1);
				Some(Command::GroupAction {
					action: Action::Edit {
						channel,
						name: Some(name),
						icon: Patch::Absent,
					},
					request: seq | RENAME_REQUEST_BIT,
				})
			}
			SetOutcome::Queued {
				cancel_in_flight: Some(cancel_seq),
			} => {
				if let Some(group) = self
					.channels
					.iter_mut()
					.find(|c| c.id == channel && c.kind == 3 && c.guild.is_none())
				{
					group.name = name;
					self.invalidate_navigation();
				}
				self.revision = self.revision.wrapping_add(1);
				Some(Command::CancelPendingWrite {
					kind: WriteKind::GroupAction,
					request: cancel_seq | RENAME_REQUEST_BIT,
				})
			}
			SetOutcome::Queued {
				cancel_in_flight: None,
			} => {
				if let Some(group) = self
					.channels
					.iter_mut()
					.find(|c| c.id == channel && c.kind == 3 && c.guild.is_none())
				{
					group.name = name;
					self.invalidate_navigation();
				}
				self.revision = self.revision.wrapping_add(1);
				None
			}
		}
	}
	pub(crate) fn cancel_group_action_pre_wire(&mut self, request: u64) {
		if request & RENAME_REQUEST_BIT == 0 {
			return;
		}
		let raw = request & !RENAME_REQUEST_BIT;
		let Some((channel, outcome)) = self.group_actions.renames.cancelled_by_seq(raw) else {
			return;
		};
		match outcome {
			Cancelled::Stale => {}
			Cancelled::Settled => {
				self.group_actions.rename_meta.remove(&channel);
				self.revision = self.revision.wrapping_add(1);
			}
			Cancelled::DispatchFollowUp { seq, value } => {
				if let Some(meta) = self.group_actions.rename_meta.get_mut(&channel) {
					meta.observed = false;
				}
				self.push_follow_up(Command::GroupAction {
					action: Action::Edit {
						channel,
						name: Some(value),
						icon: Patch::Absent,
					},
					request: seq | RENAME_REQUEST_BIT,
				});
			}
		}
	}
	fn request_group_action(&mut self, action: Action) -> Option<Command> {
		let channel = action.channel();
		if self.group_action_pending() || !self.is_group_dm(channel) {
			return None;
		}
		if !action.valid() {
			self.group_actions.status = Some((
				channel,
				"Use a group name of 1–100 characters and a supported icon",
			));
			return None;
		}
		if !self.demo && (self.auth != AuthState::Authenticated || !self.gateway_connected) {
			self.group_actions.status =
				Some((channel, "Group actions unavailable while disconnected"));
			return None;
		}
		self.group_actions.sequence = self.group_actions.sequence.wrapping_add(1);
		let request = self.group_actions.sequence;
		self.group_actions.pending = Some((
			channel,
			request,
			matches!(action, Action::Leave(_)),
			matches!(&action, Action::Edit { name: Some(_), .. }),
			false,
		));
		self.group_actions.status = None;
		self.group_actions.completed = None;
		Some(Command::GroupAction { action, request })
	}
	pub(crate) fn cancel_group_action(&mut self) {
		let renames = self.group_actions.renames.cancel_all();
		let metas = std::mem::take(&mut self.group_actions.rename_meta);
		for (channel, entry) in renames {
			let observed = metas.get(&channel).is_some_and(|m| m.observed);
			if !observed
				&& let Some(group) = self
					.channels
					.iter_mut()
					.find(|c| c.id == channel && c.kind == 3 && c.guild.is_none())
			{
				group.name = entry.confirmed;
				self.invalidate_navigation();
			}
			self.group_actions.status =
				Some((channel, "Outcome unknown; check Discord before retrying"));
			if let Some((seq, _)) = entry.in_flight {
				self.group_actions.completed = Some((channel, seq | RENAME_REQUEST_BIT, false));
			}
		}
		for (channel, snapshot) in self.group_actions.leaves.cancel_all() {
			if !snapshot.observed
				&& self.channel(channel).is_none()
				&& self.channels.len() < crate::MAX_NAV
			{
				self.channels.push(snapshot.channel);
				self.invalidate_navigation();
			}
			self.group_actions.status =
				Some((channel, "Outcome unknown; check Discord before retrying"));
		}
		if let Some((channel, request, _, _, _)) = self.group_actions.pending.take() {
			self.group_actions.status =
				Some((channel, "Outcome unknown; check Discord before retrying"));
			self.group_actions.completed = Some((channel, request, false));
		}
		self.revision = self.revision.wrapping_add(1);
	}
	pub(crate) fn observe_group_change(
		&mut self,
		channel: Id,
		recreated: bool,
		name_changed: bool,
	) {
		if name_changed && let Some(meta) = self.group_actions.rename_meta.get_mut(&channel) {
			meta.observed = true;
		}
		if recreated && let Some(snapshot) = self.group_actions.leaves.get_mut(&channel) {
			snapshot.observed = true;
		}
		if let Some((target, _, leaving, _, observed)) = &mut self.group_actions.pending
			&& *target == channel
			&& (recreated || !*leaving)
		{
			*observed = true;
		}
	}
	pub(crate) fn apply_group_action(&mut self, event: Event) -> Result<(), &'static str> {
		let Event::Written {
			channel,
			request,
			result,
		} = event;
		if request & RENAME_REQUEST_BIT != 0 {
			let raw = request & !RENAME_REQUEST_BIT;
			let validated = result.and_then(|patch| {
				if let Some(p) = patch
					&& p.id == channel
					&& match &p.name {
						Patch::Value(name) => {
							!name.is_empty()
								&& name.chars().count() <= 100
								&& name.capacity() <= 400
						}
						_ => false,
					} && match &p.icon {
					Patch::Value(hash) => model::valid_avatar_hash(hash) && hash.capacity() <= 128,
					Patch::Null | Patch::Absent => true,
				} {
					Ok(p)
				} else {
					Err(Failure::Ambiguous)
				}
			});
			let status_res = validated.as_ref().map(|_| ()).map_err(|f| *f);
			match self.group_actions.renames.finish(&channel, raw, status_res) {
				Finish::Stale => return Ok(()),
				Finish::Settled { .. } => {
					let meta = self.group_actions.rename_meta.remove(&channel);
					let observed = meta.is_some_and(|m| m.observed);
					if !observed
						&& let Ok(patch) = validated
						&& let Patch::Value(name) = patch.name
						&& let Some(group) = self
							.channels
							.iter_mut()
							.find(|c| c.id == channel && c.kind == 3 && c.guild.is_none())
					{
						group.name = name;
						self.invalidate_navigation();
					}
					self.group_actions.completed = Some((channel, request, true));
					self.group_actions.status = None;
					self.revision = self.revision.wrapping_add(1);
				}
				Finish::DispatchFollowUp { seq, value } => {
					if let Some(meta) = self.group_actions.rename_meta.get_mut(&channel) {
						meta.observed = false;
					}
					self.push_follow_up(Command::GroupAction {
						action: Action::Edit {
							channel,
							name: Some(value),
							icon: Patch::Absent,
						},
						request: seq | RENAME_REQUEST_BIT,
					});
				}
				Finish::RolledBack {
					previous, failure, ..
				} => {
					let meta = self.group_actions.rename_meta.remove(&channel);
					let observed = meta.as_ref().is_some_and(|m| m.observed);
					let label = meta
						.map(|m| m.channel_label)
						.unwrap_or_else(|| self.channel_toast_label(channel));
					if !observed
						&& let Some(group) = self
							.channels
							.iter_mut()
							.find(|c| c.id == channel && c.kind == 3 && c.guild.is_none())
					{
						group.name = previous;
						self.invalidate_navigation();
					}
					self.group_actions.completed = Some((channel, request, false));
					self.group_actions.status = Some((channel, failure.label()));
					self.status = failure.label();
					if failure.ends_session() && failure != Failure::Capacity {
						self.fail(failure);
					} else {
						self.push_write_error(ActionKind::RenameGroup, label, failure);
					}
					self.revision = self.revision.wrapping_add(1);
				}
			}
			return Ok(());
		}
		if request & LEAVE_REQUEST_BIT != 0 {
			let raw = request & !LEAVE_REQUEST_BIT;
			let Some(snapshot) = self.group_actions.leaves.finish(&channel, raw) else {
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
						self.remove_channels(&std::collections::BTreeSet::from([channel]));
						if self.selected == Some(channel) {
							self.arrived_home();
						}
					}
					self.group_actions.completed = Some((channel, request, true));
					self.group_actions.status = None;
				}
				Err(failure) => {
					if !snapshot.observed
						&& self.channel(channel).is_none()
						&& self.channels.len() < crate::MAX_NAV
					{
						self.channels.push(snapshot.channel);
						self.invalidate_navigation();
					}
					self.group_actions.completed = Some((channel, request, false));
					self.group_actions.status = Some((channel, failure.label()));
					self.status = failure.label();
					if failure.ends_session() && failure != Failure::Capacity {
						self.fail(failure);
					} else {
						self.push_write_error(
							ActionKind::LeaveGroup,
							snapshot.channel_label,
							failure,
						);
					}
				}
			}
			self.revision = self.revision.wrapping_add(1);
			return Ok(());
		}
		let Some((target, sequence, leaving, renaming, observed)) = self.group_actions.pending
		else {
			return Ok(());
		};
		if channel != target || request != sequence {
			return Ok(());
		}
		self.group_actions.pending = None;
		let result = result.and_then(|patch| {
			if leaving && patch.is_none() {
				return Ok(None);
			}
			if !leaving
				&& patch.as_ref().is_some_and(|p| {
					p.id == channel
						&& match &p.name {
							Patch::Value(name) => {
								!name.is_empty()
									&& name.chars().count() <= 100
									&& name.capacity() <= 400
							}
							Patch::Absent => !renaming,
							Patch::Null => false,
						} && match &p.icon {
						Patch::Value(hash) => {
							model::valid_avatar_hash(hash) && hash.capacity() <= 128
						}
						Patch::Null => true,
						Patch::Absent => false,
					}
				}) {
				Ok(patch)
			} else {
				Err(Failure::Ambiguous)
			}
		});
		self.group_actions.completed = Some((channel, request, result.is_ok()));
		let status = match result {
			Err(failure) => {
				if failure.ends_session() && failure != Failure::Capacity {
					self.fail(failure);
				} else {
					let label = self.channel_toast_label(channel);
					self.push_write_error(ActionKind::UpdateGroup, label, failure);
				}
				failure.label()
			}
			Ok(_) if observed => "Request completed; latest group settings shown",
			Ok(Some(patch)) => {
				if let Some(group) = self
					.channels
					.iter_mut()
					.find(|c| c.id == channel && c.kind == 3 && c.guild.is_none())
				{
					if let Patch::Value(name) = patch.name {
						group.name = name;
					}
					group.icon = match patch.icon {
						Patch::Value(hash) => Some(hash),
						_ => None,
					};
					self.invalidate_navigation();
				}
				"Group updated"
			}
			Ok(None) => {
				self.remove_channels(&std::collections::BTreeSet::from([channel]));
				if self.selected == Some(channel) {
					self.arrived_home();
				}
				"Left group"
			}
		};
		self.group_actions.status = Some((channel, status));
		self.status = status;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Envelope, Event as CoreEvent};
	fn state() -> State {
		State {
			demo: true,
			selected: Some(Id(10)),
			channels: vec![model::Channel {
				user_limit: None,
				slowmode: 0,
				id: Id(10),
				guild: None,
				kind: 3,
				name: "Group".into(),
				icon: None,
				last_message: None,
				parent_id: None,
				position: 0,
				recipients: vec![],
				member_list_id: None,
				tags: None,
				message_count: None,
			}],
			..State::default()
		}
	}
	fn patch(name: &str) -> ChannelPatch {
		ChannelPatch {
			user_limit: Patch::Absent,
			slowmode: Patch::Absent,
			id: Id(10),
			name: Patch::Value(name.into()),
			icon: Patch::Null,
			last_message: Patch::Absent,
			parent_id: Patch::Absent,
			position: Patch::Absent,
			kind: Patch::Absent,
			message_count: Patch::Absent,
			tags: Patch::Absent,
		}
	}
	fn finish(state: &mut State, command: Command, result: Result<Option<ChannelPatch>, Failure>) {
		let Command::GroupAction { action, request } = command else {
			panic!("wrong command");
		};
		state.apply(Envelope {
			generation: state.generation,
			event: CoreEvent::GroupAction(Event::Written {
				channel: action.channel(),
				request,
				result,
			}),
		});
	}
	#[test]
	fn group_actions_confirm_writes_keep_drafts_and_respect_newer_service_state() {
		{
			let mut state = state();
			assert!(state.close_dm(Id(10)).is_none());
			assert!(state.set_dm_muted(Id(10), true).is_some());
			assert!(
				state
					.edit_group(Id(10), Some(" ".into()), Patch::Absent)
					.is_none()
			);
			assert!(
				state
					.edit_group(Id(10), Some("x".repeat(101)), Patch::Absent)
					.is_none()
			);
			let edit = state
				.edit_group(Id(10), Some("New name".into()), Patch::Absent)
				.unwrap();
			let edit1_req = match &edit {
				Command::GroupAction { request, .. } => *request,
				_ => unreachable!(),
			};
			assert_eq!(state.channel(Id(10)).unwrap().name, "New name");
			assert!(state.leave_group(Id(10)).is_none());
			finish(&mut state, edit, Err(Failure::Forbidden));
			assert_eq!(state.group_action_completed(Id(10), edit1_req), Some(false));
			assert_eq!(state.channel(Id(10)).unwrap().name, "Group");
			let edit = state
				.edit_group(Id(10), Some("New name".into()), Patch::Absent)
				.unwrap();
			let edit2_req = match &edit {
				Command::GroupAction { request, .. } => *request,
				_ => unreachable!(),
			};
			finish(&mut state, edit, Ok(Some(patch("New name"))));
			assert_eq!(state.channel(Id(10)).unwrap().name, "New name");
			assert_eq!(state.group_action_completed(Id(10), edit2_req), Some(true));
			let edit = state
				.edit_group(Id(10), Some("Older".into()), Patch::Absent)
				.unwrap();
			state.apply(Envelope {
				generation: state.generation,
				event: CoreEvent::ChannelChanged(patch("Latest")),
			});
			finish(&mut state, edit, Ok(Some(patch("Older"))));
			assert_eq!(state.channel(Id(10)).unwrap().name, "Latest");
			let stale = state.leave_group(Id(10)).unwrap();
			assert!(state.channel(Id(10)).is_none());
			state.cancel_group_action();
			assert!(state.channel(Id(10)).is_some());
			let current = state.leave_group(Id(10)).unwrap();
			assert!(state.channel(Id(10)).is_none());
			finish(&mut state, stale, Ok(None));
			assert!(state.group_actions.has_fast_pending());
			state.drafts.insert(Id(10), "Keep draft".into());
			finish(&mut state, current, Ok(None));
			assert!(!state.group_actions.has_fast_pending());
			assert!(state.channel(Id(10)).is_none());
			assert_eq!(state.selected, None);
			assert_eq!(state.drafts[&Id(10)], "Keep draft");
		}
		{
			let mut state = state();
			let name = "A long recipient name, ".repeat(8);
			state.channels[0].name = name.clone();
			state.channels[0].icon = Some("0123456789abcdef0123456789abcdef".into());
			assert!(state.edit_group(Id(10), None, Patch::Absent).is_none());
			let edit = state.edit_group(Id(10), None, Patch::Null).unwrap();
			let mut response = patch("unused");
			response.name = Patch::Absent;
			finish(&mut state, edit, Ok(Some(response)));
			assert_eq!(state.channels[0].name, name);
			assert!(state.channels[0].icon.is_none());
			let rename = state
				.edit_group(Id(10), Some("Renamed".into()), Patch::Absent)
				.unwrap();
			let rename_req = match &rename {
				Command::GroupAction { request, .. } => *request,
				_ => unreachable!(),
			};
			let mut response = patch("unused");
			response.name = Patch::Absent;
			finish(&mut state, rename, Ok(Some(response)));
			assert_eq!(
				state.group_action_completed(Id(10), rename_req),
				Some(false)
			);
			assert_eq!(state.channels[0].name, name);
		}
	}
	#[test]
	fn group_rename_rejection_rolls_back_after_an_icon_update() {
		let mut state = state();
		let edit = state
			.edit_group(Id(10), Some("Renamed".into()), Patch::Absent)
			.unwrap();
		let mut icon_update = patch("unused");
		icon_update.name = Patch::Absent;
		let icon = "0123456789abcdef0123456789abcdef";
		icon_update.icon = Patch::Value(icon.into());
		state.apply(Envelope {
			generation: state.generation,
			event: CoreEvent::ChannelChanged(icon_update),
		});
		finish(&mut state, edit, Err(Failure::Forbidden));
		let group = state.channel(Id(10)).unwrap();
		assert_eq!(group.name, "Group");
		assert_eq!(group.icon.as_deref(), Some(icon));
	}
	#[test]
	fn group_leave_guards_rejoined_channel_pending_messages_and_session_reset() {
		let mut state = state();
		state.pending.push(crate::Pending {
			sticker: None,
			channel: Id(10),
			nonce: "pending".into(),
			content: "pending".into(),
			attachments: vec![],
			delivery: crate::Delivery::Sending,
			confirmed: None,
			forward: None,
			reply: None,
		});
		assert!(state.leave_group(Id(10)).is_none());
		state.pending.clear();
		let channel = state.channel(Id(10)).unwrap().clone();
		let leave = state.leave_group(Id(10)).unwrap();
		assert!(state.channel(Id(10)).is_none());
		state.apply(Envelope {
			generation: state.generation,
			event: CoreEvent::Unavailable(Id(10)),
		});
		state.apply(Envelope {
			generation: state.generation,
			event: CoreEvent::ChannelCreated(channel),
		});
		finish(&mut state, leave, Ok(None));
		assert!(state.channel(Id(10)).is_some());
		let edit = state
			.edit_group(Id(10), Some("New name".into()), Patch::Absent)
			.unwrap();
		state.command_rejected(edit);
		assert!(!state.group_actions.has_fast_pending());
		let ready_channels = state.channels.clone();
		let interrupted = state
			.edit_group(Id(10), Some("Interrupted".into()), Patch::Absent)
			.unwrap();
		let request = match &interrupted {
			Command::GroupAction { request, .. } => *request,
			_ => unreachable!(),
		};
		state.apply(Envelope {
			generation: state.generation,
			event: CoreEvent::Ready {
				user: model::User {
					primary_guild: None,
					cosmetics: None,
					id: Id(1),
					name: "Synthetic".into(),
					avatar: None,
					webhook: false,
					kind: Default::default(),
					discriminator: 0,
				},
				channels: ready_channels,
				guilds: vec![],
				permissions: Default::default(),
			},
		});
		assert_eq!(state.group_action_completed(Id(10), request), Some(false));
		finish(&mut state, interrupted, Ok(Some(patch("Interrupted"))));
		assert_eq!(state.channel(Id(10)).unwrap().name, "Group");
		state.demo = false;
		state.gateway_connected = false;
		assert!(state.leave_group(Id(10)).is_none());
		state.demo = true;
		let leave = state.leave_group(Id(10)).unwrap();
		let Command::GroupAction { action, request } = leave else {
			unreachable!()
		};
		let generation = state.generation;
		state.logout();
		state.apply(Envelope {
			generation,
			event: CoreEvent::GroupAction(Event::Written {
				channel: action.channel(),
				request,
				result: Ok(None),
			}),
		});
		assert_eq!(state.group_action_completed(Id(10), request), None);

		// Concurrent icon edit (pending) and name-only rename (renames) never collide on request id.
		let mut s2 = self::state();
		let icon_edit = s2.edit_group(Id(10), None, Patch::Null).unwrap();
		let rename = s2
			.edit_group(Id(10), Some("Renamed".into()), Patch::Absent)
			.unwrap();
		let icon_req = match &icon_edit {
			Command::GroupAction { request, .. } => *request,
			_ => unreachable!(),
		};
		let rename_req = match &rename {
			Command::GroupAction { request, .. } => *request,
			_ => unreachable!(),
		};
		assert_ne!(icon_req, rename_req);
		let mut icon_patch = patch("unused");
		icon_patch.name = Patch::Absent;
		icon_patch.icon = Patch::Null;
		finish(&mut s2, icon_edit, Ok(Some(icon_patch)));
		assert!(!s2.group_action_pending());
		assert_eq!(s2.group_action_completed(Id(10), icon_req), Some(true));
		finish(&mut s2, rename, Ok(Some(patch("Renamed"))));
		assert_eq!(s2.group_action_completed(Id(10), rename_req), Some(true));
		assert_eq!(s2.channel(Id(10)).unwrap().name, "Renamed");
	}
}
