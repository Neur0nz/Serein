//! Bounded poll actions: optimistic coalesced voting with authoritative readback and no replay.
use crate::optimistic::ActionKind;
use crate::{
	State,
	auth::{AuthState, Failure},
	optimistic::{Cancelled, CoalescedSlot, Finish, SetOutcome, WriteKind},
};
use model::{Freshness, Id, Message, Patch, polls::Create};
use std::collections::BTreeMap;

const MAX_POLL_VOTES: usize = 8;

pub enum Action {
	Create(Box<Create>),
	Vote(Vec<u32>),
	End,
	Read,
}
pub struct Request {
	pub channel: Id,
	pub message: Option<Id>,
	pub request: u64,
	pub nonce: String,
	pub action: Action,
}
pub enum Event {
	Result {
		channel: Id,
		message: Option<Id>,
		request: u64,
		result: Result<Box<Message>, Failure>,
	},
	Vote {
		channel: Id,
		message: Id,
		user: Id,
		answer: u32,
		add: bool,
	},
}
/// High bit separating coalesced poll vote sequence IDs from serialized End/Create/Read actions.
const VOTE_REQUEST_BIT: u64 = 1 << 62;

struct VoteMeta {
	confirmed_poll: model::polls::Poll,
	observed: bool,
	channel_label: String,
}
#[derive(Default)]
pub struct Polls {
	pub pending: Option<(Id, Option<Id>, u64)>,
	votes: CoalescedSlot<(Id, Id), Vec<u32>, MAX_POLL_VOTES>,
	vote_meta: BTreeMap<(Id, Id), VoteMeta>,
	pub created: u64,
	pub error: Option<&'static str>,
	sequence: u64,
	revision: Option<u64>,
}
impl Polls {
	pub fn reset(&mut self) {
		self.reset_view();
		let _ = self.votes.cancel_all();
		self.vote_meta.clear();
	}
	/// Retire view-bound requests without losing writes for a conversation parked in RAM.
	pub(crate) fn reset_view(&mut self) {
		self.pending = None;
		self.error = None;
		self.sequence = self.sequence.wrapping_add(1);
	}
	pub fn has_fast_pending(&self) -> bool {
		self.pending.is_some() || !self.votes.is_empty()
	}
	pub(crate) fn observe_finalization(
		&mut self,
		channel: Id,
		message: Id,
		poll: &mut model::polls::Poll,
	) {
		if poll.finalized
			&& let Some(meta) = self.vote_meta.get_mut(&(channel, message))
		{
			poll.retain_results(&meta.confirmed_poll);
			meta.confirmed_poll = poll.clone();
			meta.observed = true;
		}
	}
	pub fn matches_request(&self, channel: Id, message: Option<Id>, request: u64) -> bool {
		if request & VOTE_REQUEST_BIT != 0 {
			let raw = request & !VOTE_REQUEST_BIT;
			message.is_some_and(|m| {
				self.votes
					.entry(&(channel, m))
					.is_some_and(|e| e.in_flight.as_ref().map(|(seq, _)| *seq) == Some(raw))
			})
		} else {
			self.pending == Some((channel, message, request))
		}
	}
}
pub fn now_ms() -> i128 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis() as i128
}
fn apply_optimistic_vote(poll: &mut model::polls::Poll, selected: &[u32]) {
	for answer in &mut poll.answers {
		let want = selected.contains(&answer.id);
		if answer.me != want {
			if poll.results_known {
				answer.votes = if want {
					answer.votes.saturating_add(1)
				} else {
					answer.votes.saturating_sub(1)
				};
			}
			answer.me = want;
		}
	}
}
fn poll_patch(channel: Id, id: Id, poll: model::polls::Poll) -> model::MessagePatch {
	let mut patch = crate::message_actions::content_patch(channel, id, String::new());
	patch.content = Patch::Absent;
	patch.poll = Patch::Value(Some(Box::new(poll)));
	patch.extra_content.poll = Patch::Value(true);
	patch
}
impl State {
	pub fn can_create_poll(&self, channel: Id) -> bool {
		self.can_send(channel)
			&& self.permission(channel, model::permissions::SEND_POLLS) == Some(true)
	}
	pub fn can_vote_poll(&self, message: &Message) -> bool {
		self.auth == AuthState::Authenticated
			&& self.gateway_connected
			&& self.selected == Some(message.channel)
			&& self.freshness == Freshness::Fresh
			&& self.can_read_history(message.channel)
			&& !message.forwarded
			&& !message.ephemeral
			&& !self.timeline.is_deleted(message.id)
			&& message.poll.as_ref().is_some_and(|p| !p.ended(now_ms()))
	}
	pub(crate) fn cancel_polls(&mut self) {
		let metas = std::mem::take(&mut self.polls.vote_meta);
		self.polls.reset();
		for ((channel, msg_id), meta) in metas {
			self.resident.remove(channel);
			if self.selected == Some(channel) && self.can_read_history(channel) {
				let _ = self
					.timeline
					.patch(poll_patch(channel, msg_id, meta.confirmed_poll));
			}
		}
	}
	pub(crate) fn cancel_poll_pre_wire(&mut self, request: u64) {
		if request & VOTE_REQUEST_BIT == 0 {
			return;
		}
		let raw = request & !VOTE_REQUEST_BIT;
		let Some(((channel, message), outcome)) = self.polls.votes.cancelled_by_seq(raw) else {
			return;
		};
		match outcome {
			Cancelled::Stale => {}
			Cancelled::Settled => {
				self.polls.vote_meta.remove(&(channel, message));
				self.revision += 1;
			}
			Cancelled::DispatchFollowUp { seq, value } => {
				if let Some(meta) = self.polls.vote_meta.get_mut(&(channel, message)) {
					meta.observed = false;
				}
				self.send_sequence = self.send_sequence.wrapping_add(1);
				let nonce = crate::fingerprint::nonce(now_ms() as u128, self.send_sequence);
				self.push_follow_up(crate::Command::Polls(Request {
					channel,
					message: Some(message),
					request: seq | VOTE_REQUEST_BIT,
					nonce,
					action: Action::Vote(value),
				}));
			}
		}
	}
	pub fn prepare_poll(&mut self, message: Option<Id>, action: Action) -> Option<crate::Command> {
		let channel = self.selected?;
		if let Action::Vote(answers) = action {
			let msg_id = message?;
			if self
				.polls
				.pending
				.is_some_and(|(c, m, _)| c == channel && m == Some(msg_id))
			{
				self.status = "Wait for the current poll action";
				return None;
			}
			let source = self.timeline.get(msg_id)?;
			let poll = source.poll.as_ref()?;
			if source.channel != channel
				|| source.forwarded
				|| source.ephemeral
				|| self.timeline.is_deleted(source.id)
				|| !self.can_vote_poll(source)
				|| answers.len() > model::polls::MAX_ANSWERS
				|| (!poll.multiselect && answers.len() > 1)
				|| !answers.iter().enumerate().all(|(i, id)| {
					poll.answers.iter().any(|a| a.id == *id) && !answers[..i].contains(id)
				}) {
				self.status =
					"Poll action is unavailable with the current permissions or poll state";
				return None;
			}
			let mut sorted_answers = answers.clone();
			sorted_answers.sort_unstable();
			let key = (channel, msg_id);
			let confirmed_poll = self
				.polls
				.vote_meta
				.get(&key)
				.map(|m| m.confirmed_poll.clone())
				.unwrap_or_else(|| (**poll).clone());
			let mut confirmed_answers: Vec<u32> = confirmed_poll
				.answers
				.iter()
				.filter(|a| a.me)
				.map(|a| a.id)
				.collect();
			confirmed_answers.sort_unstable();
			let mut optimistic_poll = confirmed_poll.clone();
			apply_optimistic_vote(&mut optimistic_poll, &sorted_answers);
			let label = self.channel_toast_label(channel);
			let outcome = self
				.polls
				.votes
				.set(key, confirmed_answers, sorted_answers.clone());
			self.polls.error = None;
			match outcome {
				SetOutcome::Full => {
					self.status = "Wait for the current poll action";
					return None;
				}
				SetOutcome::Dispatch(seq) => {
					self.polls.vote_meta.insert(
						key,
						VoteMeta {
							confirmed_poll,
							observed: false,
							channel_label: label,
						},
					);
					self.resident.remove(channel);
					let _ = self
						.timeline
						.patch(poll_patch(channel, msg_id, optimistic_poll));
					self.revision += 1;
					self.send_sequence = self.send_sequence.wrapping_add(1);
					let nonce = crate::fingerprint::nonce(now_ms() as u128, self.send_sequence);
					return Some(crate::Command::Polls(Request {
						channel,
						message: Some(msg_id),
						request: seq | VOTE_REQUEST_BIT,
						nonce,
						action: Action::Vote(sorted_answers),
					}));
				}
				SetOutcome::Queued {
					cancel_in_flight: Some(cancel_seq),
				} => {
					self.resident.remove(channel);
					let _ = self
						.timeline
						.patch(poll_patch(channel, msg_id, optimistic_poll));
					self.revision += 1;
					return Some(crate::Command::CancelPendingWrite {
						kind: WriteKind::Poll,
						request: cancel_seq | VOTE_REQUEST_BIT,
					});
				}
				SetOutcome::Queued {
					cancel_in_flight: None,
				} => {
					self.resident.remove(channel);
					let _ = self
						.timeline
						.patch(poll_patch(channel, msg_id, optimistic_poll));
					self.revision += 1;
					return None;
				}
			}
		}
		if self.polls.pending.is_some() {
			self.status = "Wait for the current poll action";
			return None;
		}
		let valid = match &action {
			Action::Create(create) => {
				message.is_none() && create.valid() && self.can_create_poll(channel)
			}
			_ => {
				let source = self.timeline.get(message?)?;
				let _poll = source.poll.as_ref()?;
				if source.channel != channel
					|| source.forwarded
					|| source.ephemeral
					|| self.timeline.is_deleted(source.id)
				{
					return None;
				}
				match &action {
					Action::End => {
						self.can_vote_poll(source)
							&& self.user.as_ref().is_some_and(|u| u.id == source.author.id)
					}
					Action::Read => {
						self.auth == AuthState::Authenticated
							&& self.gateway_connected
							&& self.can_read_history(channel)
					}
					Action::Create(_) | Action::Vote(_) => false,
				}
			}
		};
		if !valid {
			self.status = "Poll action is unavailable with the current permissions or poll state";
			return None;
		}
		self.polls.sequence = self.polls.sequence.wrapping_add(1);
		let request = self.polls.sequence;
		self.polls.pending = Some((channel, message, request));
		self.polls.revision = message.and_then(|id| self.timeline.get(id).map(|m| m.revision));
		self.polls.error = None;
		self.send_sequence = self.send_sequence.wrapping_add(1);
		let nonce = crate::fingerprint::nonce(now_ms() as u128, self.send_sequence);
		Some(crate::Command::Polls(Request {
			channel,
			message,
			request,
			nonce,
			action,
		}))
	}
	pub(crate) fn apply_poll(&mut self, event: Event) -> Result<(), &'static str> {
		match event {
			Event::Result {
				channel,
				message,
				request,
				result,
			} => {
				if request & VOTE_REQUEST_BIT != 0 {
					let raw = request & !VOTE_REQUEST_BIT;
					let Some(msg_id) = message else {
						return Ok(());
					};
					if !self
						.polls
						.votes
						.entry(&(channel, msg_id))
						.is_some_and(|e| e.in_flight.as_ref().map(|(seq, _)| *seq) == Some(raw))
					{
						return Ok(());
					}
					let key = (channel, msg_id);
					let valid_res = match &result {
						Ok(updated)
							if updated.channel == channel
								&& updated.id == msg_id && updated.poll.is_some()
								&& session_cache::Timeline::valid_message(updated) =>
						{
							Ok(())
						}
						Ok(_) => Err(Failure::Protocol),
						Err(failure) => Err(*failure),
					};
					match self.polls.votes.finish(&key, raw, valid_res) {
						Finish::Stale => return Ok(()),
						Finish::Settled { .. } => {
							let meta = self.polls.vote_meta.remove(&key);
							self.resident.remove(channel);
							if let Ok(updated) = result
								&& let Some(poll) = updated.poll
								&& self.selected == Some(channel)
								&& self.can_read_history(channel)
								&& (meta.is_none_or(|m| {
									!m.observed
										|| (poll.results_known && !m.confirmed_poll.results_known)
								}) || poll.finalized)
							{
								self.timeline.patch(poll_patch(channel, msg_id, *poll))?;
								self.revision += 1;
							}
						}
						Finish::DispatchFollowUp { seq, value } => {
							if let Some(meta) = self.polls.vote_meta.get_mut(&key) {
								if let Ok(updated) = result
									&& let Some(poll) = updated.poll
									&& (poll.finalized || !meta.confirmed_poll.finalized)
								{
									meta.confirmed_poll = *poll;
								}
								if meta.confirmed_poll.finalized {
									let finalized = meta.confirmed_poll.clone();
									self.polls.votes.remove(&key);
									self.polls.vote_meta.remove(&key);
									self.resident.remove(channel);
									if self.selected == Some(channel)
										&& self.can_read_history(channel)
									{
										self.timeline
											.patch(poll_patch(channel, msg_id, finalized))?;
										self.revision += 1;
									}
									return Ok(());
								}
								meta.observed = false;
							}
							self.send_sequence = self.send_sequence.wrapping_add(1);
							let nonce =
								crate::fingerprint::nonce(now_ms() as u128, self.send_sequence);
							self.push_follow_up(crate::Command::Polls(Request {
								channel,
								message: Some(msg_id),
								request: seq | VOTE_REQUEST_BIT,
								nonce,
								action: Action::Vote(value),
							}));
						}
						Finish::RolledBack { failure, .. } => {
							let meta = self.polls.vote_meta.remove(&key);
							self.resident.remove(channel);
							if let Some(meta) = meta {
								if self.selected == Some(channel) && self.can_read_history(channel)
								{
									let _ = self.timeline.patch(poll_patch(
										channel,
										msg_id,
										meta.confirmed_poll,
									));
								}
								if failure.ends_session() && failure != Failure::Capacity {
									self.fail(failure);
								} else {
									self.push_write_error(
										ActionKind::PollVote,
										meta.channel_label,
										failure,
									);
								}
							} else if failure.ends_session() && failure != Failure::Capacity {
								self.fail(failure);
							}
							self.polls.error = Some(
								if matches!(failure, Failure::Network | Failure::Ambiguous) {
									"Poll action could not be confirmed. Refresh before trying again."
								} else {
									"Poll action failed. Check permissions and refresh the conversation."
								},
							);
							self.status = self.polls.error.unwrap();
							self.revision += 1;
						}
					}
					return Ok(());
				}
				if self.polls.pending != Some((channel, message, request)) {
					return Ok(());
				}
				self.polls.pending = None;
				match result {
					Ok(updated)
						if updated.channel == channel
							&& message.is_none_or(|id| id == updated.id)
							&& updated.poll.is_some()
							&& session_cache::Timeline::valid_message(&updated) =>
					{
						let updated = *updated;
						self.resident.remove(channel);
						if message.is_none() {
							self.polls.created = self.polls.created.wrapping_add(1);
							self.apply(crate::Envelope {
								generation: self.generation,
								event: crate::Event::Message(updated),
							});
						} else if self.selected == Some(channel)
							&& self.can_read_history(channel)
							&& self.timeline.get(updated.id).is_some_and(|m| {
								Some(m.revision) == self.polls.revision
									|| updated.poll.as_ref().is_some_and(|p| p.finalized)
							}) && let Some(poll) = updated.poll
						{
							self.timeline
								.patch(poll_patch(channel, updated.id, *poll))?;
							self.revision += 1;
						}
					}
					result => {
						let failure = result.err().unwrap_or(Failure::Protocol);
						if failure.ends_session() && failure != Failure::Capacity {
							self.fail(failure);
						} else {
							let label = self.channel_toast_label(channel);
							self.push_write_error(ActionKind::Poll, label, failure);
						}
						self.polls.error = Some(
							if matches!(failure, Failure::Network | Failure::Ambiguous) {
								"Poll action could not be confirmed. Refresh before trying again."
							} else {
								"Poll action failed. Check permissions and refresh the conversation."
							},
						);
						self.status = self.polls.error.unwrap();
					}
				}
			}
			Event::Vote {
				channel,
				message,
				user,
				answer,
				add,
			} => {
				self.resident.remove(channel);
				let own = self.user.as_ref().is_some_and(|u| u.id == user);
				if let Some(meta) = self.polls.vote_meta.get_mut(&(channel, message)) {
					if own {
						meta.observed = true;
					}
					if meta.confirmed_poll.results_known
						&& !meta.confirmed_poll.finalized
						&& let Some(option) = meta
							.confirmed_poll
							.answers
							.iter_mut()
							.find(|a| a.id == answer)
						&& !(own && option.me == add)
					{
						option.votes = if add {
							option.votes.saturating_add(1)
						} else {
							option.votes.saturating_sub(1)
						};
						if own {
							option.me = add;
						}
					}
				}
				if self.selected != Some(channel) || !self.can_read_history(channel) {
					return Ok(());
				}
				let Some(source) = self.timeline.get(message).filter(|m| !m.forwarded) else {
					return Ok(());
				};
				let Some(mut poll) = source
					.poll
					.clone()
					.filter(|p| p.results_known && !p.finalized)
				else {
					return Ok(());
				};
				let Some(option) = poll.answers.iter_mut().find(|a| a.id == answer) else {
					return Ok(());
				};
				if own && option.me == add {
					return Ok(());
				}
				option.votes = if add {
					option.votes.saturating_add(1)
				} else {
					option.votes.saturating_sub(1)
				};
				if own {
					option.me = add;
				}
				let mut patch =
					crate::message_actions::content_patch(channel, message, String::new());
				patch.content = Patch::Absent;
				patch.poll = Patch::Value(Some(poll));
				self.timeline.patch(patch)?;
				self.revision += 1;
			}
		}
		Ok(())
	}
}
