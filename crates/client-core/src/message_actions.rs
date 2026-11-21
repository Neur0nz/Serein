//! Bounded optimistic edits, deletes, and pin toggles; server events remain authoritative.
use crate::optimistic::ActionKind;
use crate::{
	Command, State,
	auth::Failure,
	optimistic::{Cancelled, CoalescedSlot, Finish, InFlightSet, SetOutcome, WriteKind},
};
use model::{Id, Message, MessagePatch, Patch};
use std::{borrow::Cow, collections::BTreeMap};

const MAX_EDITS: usize = 8;
const MAX_DELETES: usize = 16;
const MAX_DELETE_SNAPSHOT_BYTES: usize = 256 * 1024;

#[derive(Default)]
pub struct MessageActions {
	sequence: u64,
	edits: CoalescedSlot<(Id, Id), String, MAX_EDITS>,
	edit_meta: BTreeMap<(Id, Id), EditMeta>,
	deletes: InFlightSet<(Id, Id), DeletedMessage, MAX_DELETES>,
	pins: BTreeMap<(Id, Id), Pin>,
	pub failed_edits: Vec<(Id, Id, String)>,
}
struct EditMeta {
	// An authoritative edit can be observed without a service timestamp.
	observed_at: Option<Option<i128>>,
	channel_label: String,
}
struct DeletedMessage {
	snapshot: Message,
	channel_label: String,
}
struct Pin {
	request: u64,
	value: bool,
	previous: bool,
	pending: bool,
}
impl MessageActions {
	pub fn pin(&self, channel: Id, message: Id) -> Option<bool> {
		self.pins.get(&(channel, message)).map(|pin| pin.value)
	}
	pub fn edit_pending(&self, channel: Id, message: Id) -> bool {
		self.edits.entry(&(channel, message)).is_some()
	}
	pub fn delete_pending(&self, channel: Id, message: Id) -> bool {
		self.deletes.contains(&(channel, message))
	}
	pub fn pin_pending(&self, channel: Id, message: Id) -> bool {
		self.pins
			.get(&(channel, message))
			.is_some_and(|pin| pin.pending)
	}
	pub fn has_fast_pending(&self) -> bool {
		!self.edits.is_empty()
			|| !self.deletes.is_empty()
			|| self.pins.values().any(|pin| pin.pending)
	}
	pub(crate) fn reconcile_pins(&mut self, channel: Id, page: &model::SearchPage, first: bool) {
		self.pins.retain(|(c, id), pin| {
			*c != channel
				|| pin.pending
				|| (!page.hits.iter().any(|hit| hit.id == *id) && !(first && !page.partial))
		});
	}
	pub(crate) fn observe_content(
		&mut self,
		channel: Id,
		message: Id,
		content: &mut String,
		edited_at: Option<i128>,
	) {
		let key = (channel, message);
		if let Some(meta) = self.edit_meta.get_mut(&key)
			&& let Some(desired) = self.edits.desired(&key)
		{
			meta.observed_at = Some(edited_at);
			let confirmed = std::mem::replace(content, desired.clone());
			self.edits.observe_authoritative(&key, confirmed);
		}
	}
	pub(crate) fn observe_delete(&mut self, channel: Id, message: Id) {
		self.deletes.remove(&(channel, message));
	}
}
impl State {
	/// Returns the active history window before pending edits and deletes are applied.
	pub fn confirmed_history_messages(&self) -> impl Iterator<Item = Cow<'_, Message>> {
		self.timeline
			.iter()
			.chain(self.message_actions.deletes.iter().filter_map(
				move |((channel, id), deleted)| {
					(self.selected == Some(*channel)
						&& self.timeline.is_deleted(*id)
						&& self.timeline.contains_row(*id))
					.then_some(&deleted.snapshot)
				},
			))
			.map(move |message| {
				if let Some(edit) = self
					.message_actions
					.edits
					.entry(&(message.channel, message.id))
					&& message.content != edit.confirmed
				{
					let mut confirmed = message.clone();
					confirmed.content.clone_from(&edit.confirmed);
					Cow::Owned(confirmed)
				} else {
					Cow::Borrowed(message)
				}
			})
	}
	pub(crate) fn cancel_message_actions(&mut self) {
		self.cancel_polls();
		let edits = self.message_actions.edits.cancel_all();
		self.message_actions.edit_meta.clear();
		for ((channel, message), entry) in edits {
			self.resident.remove(channel);
			if self.selected == Some(channel)
				&& self
					.timeline
					.get(message)
					.is_some_and(|m| m.content == entry.desired)
			{
				let _ = self
					.timeline
					.patch(content_patch(channel, message, entry.confirmed));
			}
			if self.message_actions.failed_edits.len() < MAX_EDITS {
				self.message_actions
					.failed_edits
					.push((channel, message, entry.desired));
			}
		}
		for ((channel, message), deleted) in self.message_actions.deletes.cancel_all() {
			self.resident.remove(channel);
			if self.selected == Some(channel) && self.timeline.get(message).is_none() {
				let _ = self.timeline.restore_optimistic_delete(deleted.snapshot);
			}
		}
		for pin in self
			.message_actions
			.pins
			.values_mut()
			.filter(|pin| pin.pending)
		{
			pin.value = pin.previous;
			pin.pending = false;
		}
		self.revision += 1;
	}
	pub(crate) fn cancel_edit_pre_wire(&mut self, request: u64) {
		let Some(((channel, message), outcome)) =
			self.message_actions.edits.cancelled_by_seq(request)
		else {
			return;
		};
		match outcome {
			Cancelled::Stale => {}
			Cancelled::Settled => {
				self.message_actions.edit_meta.remove(&(channel, message));
				self.revision += 1;
			}
			Cancelled::DispatchFollowUp { seq, value } => {
				self.push_follow_up(Command::Edit {
					request: seq,
					channel,
					message,
					content: value,
				});
			}
		}
	}
	pub(crate) fn optimistic_edit(
		&mut self,
		channel: Id,
		message: Id,
		content: String,
	) -> Option<Command> {
		let key = (channel, message);
		let already_editing = self.message_actions.edits.entry(&key).is_some();
		if !already_editing
			&& self.message_actions.edits.len() + self.message_actions.failed_edits.len()
				>= MAX_EDITS
		{
			self.status = "Wait for the previous message edit to finish";
			return None;
		}
		let current = self
			.timeline
			.get(message)
			.filter(|m| m.channel == channel)?;
		// Eight snapshots, each limited to a 64 KiB source plus MAX_CONTENT UTF-8 text.
		if current.content.len() > 64 * 1024 {
			return None;
		}
		let before = current.content.clone();
		if self
			.timeline
			.patch(content_patch(channel, message, content.clone()))
			.is_err()
		{
			self.status = "Edit could not be applied; reload this conversation";
			return None;
		}
		self.resident.remove(channel);
		let label = self.channel_toast_label(channel);
		let outcome = self
			.message_actions
			.edits
			.set(key, before.clone(), content.clone());
		match outcome {
			SetOutcome::Full => {
				let _ = self.timeline.patch(content_patch(channel, message, before));
				self.status = "Wait for the previous message edit to finish";
				None
			}
			SetOutcome::Dispatch(seq) => {
				self.message_actions.edit_meta.insert(
					key,
					EditMeta {
						observed_at: None,
						channel_label: label,
					},
				);
				self.revision += 1;
				Some(Command::Edit {
					request: seq,
					channel,
					message,
					content,
				})
			}
			SetOutcome::Queued {
				cancel_in_flight: Some(cancel_seq),
			} => {
				self.revision += 1;
				Some(Command::CancelPendingWrite {
					kind: WriteKind::Edit,
					request: cancel_seq,
				})
			}
			SetOutcome::Queued {
				cancel_in_flight: None,
			} => {
				self.revision += 1;
				None
			}
		}
	}
	pub fn apply_edit_result(
		&mut self,
		channel: Id,
		message: Id,
		request: u64,
		result: Result<Message, Failure>,
	) {
		let key = (channel, message);
		let Some(entry) = self.message_actions.edits.entry(&key) else {
			return;
		};
		if entry.in_flight.as_ref().map(|(seq, _)| *seq) != Some(request) {
			return;
		}
		let failed_desired = entry.desired.clone();
		let valid_updated = match &result {
			Ok(updated)
				if updated.channel == channel
					&& updated.id == message
					&& session_cache::Timeline::valid_message(updated) =>
			{
				Ok(())
			}
			Ok(_) => Err(Failure::Protocol),
			Err(failure) => Err(*failure),
		};
		let fallback_content = result
			.as_ref()
			.ok()
			.filter(|updated| {
				self.message_actions
					.edit_meta
					.get(&key)
					.and_then(|meta| meta.observed_at)
					.is_some_and(|observed| {
						updated
							.edited_at
							.is_none_or(|edited| observed.is_some_and(|at| edited < at))
					})
			})
			.map(|_| entry.confirmed.clone());
		self.resident.remove(channel);
		match self
			.message_actions
			.edits
			.finish(&key, request, valid_updated)
		{
			Finish::Stale => return,
			Finish::Settled { .. } => {
				self.message_actions.edit_meta.remove(&key);
				if let Ok(updated) = result
					&& self.selected == Some(channel)
					&& self.can_view(channel)
				{
					let mut patch = content_patch(channel, message, String::new());
					if let Some(content) = fallback_content {
						patch.content = Patch::Value(content);
					} else {
						patch.content = Patch::Value(updated.content);
						patch.edited = updated.edited_at.map_or(Patch::Null, Patch::Value);
						patch.mentions = Patch::Value(updated.mentions);
					}
					let _ = self.timeline.patch(patch);
				}
			}
			Finish::DispatchFollowUp { seq, value } => {
				if let Ok(updated) = result {
					if fallback_content.is_none() {
						if let Some(meta) = self.message_actions.edit_meta.get_mut(&key) {
							meta.observed_at = Some(updated.edited_at);
						}
						if self.selected == Some(channel) && self.can_view(channel) {
							let mut patch = content_patch(channel, message, value.clone());
							patch.edited = updated.edited_at.map_or(Patch::Null, Patch::Value);
							patch.mentions = Patch::Value(updated.mentions);
							let _ = self.timeline.patch(patch);
						}
					}
					self.message_actions
						.edits
						.observe_authoritative(&key, fallback_content.unwrap_or(updated.content));
				}
				self.push_follow_up(Command::Edit {
					request: seq,
					channel,
					message,
					content: value,
				});
			}
			Finish::RolledBack {
				previous, failure, ..
			} => {
				let meta = self.message_actions.edit_meta.remove(&key);
				let label = meta
					.map(|m| m.channel_label)
					.unwrap_or_else(|| self.channel_toast_label(channel));
				if self.selected == Some(channel)
					&& self
						.timeline
						.get(message)
						.is_some_and(|m| m.content == failed_desired)
				{
					let _ = self
						.timeline
						.patch(content_patch(channel, message, previous));
				}
				if self.message_actions.failed_edits.len() < MAX_EDITS {
					self.message_actions
						.failed_edits
						.push((channel, message, failed_desired));
				}
				if failure.ends_session() && failure != Failure::Capacity {
					self.fail(failure);
				} else {
					self.status = "Message edit failed; your edit was kept for retry";
					self.push_write_error(ActionKind::EditMessage, label, failure);
				}
			}
		}
		self.revision += 1;
	}
	pub(crate) fn optimistic_delete(&mut self, channel: Id, message: Id) -> Option<u64> {
		let key = (channel, message);
		if self.message_actions.deletes.contains(&key) {
			return None;
		}
		let snapshot = self
			.timeline
			.get(message)
			.filter(|m| m.channel == channel)?
			.clone();
		if snapshot.bytes() > MAX_DELETE_SNAPSHOT_BYTES {
			return None;
		}
		let channel_label = self.channel_toast_label(channel);
		let request = self
			.message_actions
			.deletes
			.insert(
				key,
				DeletedMessage {
					snapshot,
					channel_label,
				},
			)
			.ok()?;
		self.resident.remove(channel);
		if self.selected == Some(channel) {
			let _ = self.timeline.delete(message);
		}
		if self.selected == Some(channel) && self.reply.is_some_and(|r| r.target() == message) {
			self.reply = None;
		}
		self.revision += 1;
		Some(request)
	}
	pub(crate) fn optimistic_pin(&mut self, channel: Id, message: Id, pinned: bool) -> Option<u64> {
		if self.message_actions.pin_pending(channel, message)
			|| self
				.message_actions
				.pins
				.values()
				.filter(|p| p.pending)
				.count() >= 8
		{
			self.status = "Wait for the previous pin action to finish";
			return None;
		}
		let previous = self.is_pinned(channel, message);
		if self.message_actions.pins.len() >= 128
			&& let Some(key) = self
				.message_actions
				.pins
				.iter()
				.find_map(|(key, pin)| (!pin.pending).then_some(*key))
		{
			self.message_actions.pins.remove(&key);
		}
		self.message_actions.sequence = self.message_actions.sequence.wrapping_add(1);
		let request = self.message_actions.sequence;
		self.message_actions.pins.insert(
			(channel, message),
			Pin {
				request,
				value: pinned,
				previous,
				pending: true,
			},
		);
		self.revision += 1;
		Some(request)
	}
	pub fn apply_pin_result(
		&mut self,
		channel: Id,
		message: Id,
		pinned: bool,
		request: u64,
		result: Result<(), Failure>,
	) {
		let Some(pin) = self
			.message_actions
			.pins
			.get_mut(&(channel, message))
			.filter(|pin| pin.pending && pin.value == pinned && pin.request == request)
		else {
			return;
		};
		pin.pending = false;
		if let Err(failure) = result {
			pin.value = pin.previous;
			if matches!(failure, Failure::Ambiguous | Failure::Network) {
				self.pins_changed = Some(channel);
			}
			if failure.ends_session() && failure != Failure::Capacity {
				self.fail(failure);
			} else {
				let label = self.channel_toast_label(channel);
				let action = if pinned {
					ActionKind::PinMessage
				} else {
					ActionKind::UnpinMessage
				};
				self.status = "Pin action failed; the previous state was restored";
				self.push_write_error(action, label, failure);
			}
		} else {
			if !pinned
				&& let Some(page) = self
					.search
					.as_mut()
					.filter(|v| v.pins && v.channel == channel)
					.and_then(|v| v.page.as_mut())
			{
				page.hits.retain(|hit| hit.id != message);
			}
			self.pins_changed = Some(channel);
		}
		self.revision += 1;
	}
	pub fn apply_delete_result(
		&mut self,
		channel: Id,
		message: Id,
		request: u64,
		result: Result<(), Failure>,
	) {
		let Some(deleted) = self
			.message_actions
			.deletes
			.finish(&(channel, message), request)
		else {
			return;
		};
		match result {
			Ok(()) => {
				self.apply(crate::Envelope {
					generation: self.generation,
					event: crate::Event::Delete {
						channel,
						id: message,
					},
				});
			}
			Err(failure) => {
				self.resident.remove(channel);
				if self.selected == Some(channel) && self.timeline.get(message).is_none() {
					let _ = self.timeline.restore_optimistic_delete(deleted.snapshot);
				}
				if failure.ends_session() && failure != Failure::Capacity {
					self.fail(failure);
				} else if failure == Failure::Forbidden {
					self.push_write_error(
						ActionKind::DeleteMessage,
						deleted.channel_label,
						failure,
					);
					self.status = "You cannot delete messages in this channel";
				} else {
					self.push_write_error(
						ActionKind::DeleteMessage,
						deleted.channel_label,
						failure,
					);
					self.status = failure.label();
				}
			}
		}
		self.revision += 1;
	}
}
pub(crate) fn content_patch(channel: Id, id: Id, content: String) -> MessagePatch {
	MessagePatch {
		poll: model::Patch::Absent,
		sticker_items: Patch::Absent,
		channel,
		id,
		content: Patch::Value(content),
		edited: Patch::Absent,
		mentions: Patch::Absent,
		reactions: Patch::Absent,
		embeds: Patch::Absent,
		embeds_suppressed: Patch::Absent,
		attachments: Patch::Absent,
		components: model::Patch::Absent,
		flags: model::Patch::Absent,
		application_id: model::Patch::Absent,
		extra_content: Default::default(),
	}
}
