//! Explicit forwards reuse the bounded send lifecycle without touching composer drafts.
use crate::{
	Command, ForwardPart, Pending, State,
	auth::{AuthState, Failure},
	optimistic::ActionKind,
};
use model::{Delivery, Id};

impl State {
	pub fn can_forward(&self, message: Id) -> bool {
		self.auth == AuthState::Authenticated
			&& self.gateway_connected
			&& self.timeline.get(message).is_some_and(|m| {
				self.selected == Some(m.channel)
					&& self.can_read_history(m.channel)
					&& matches!(m.kind, 0 | 19 | 20 | 23)
					&& !m.ephemeral && !m.unsupported
					&& !m.extra_content.poll
			})
	}

	pub fn prepare_forward(&mut self, message: Id, targets: &[Id], note: &str) -> Vec<Command> {
		let count = targets.len() * if note.trim().is_empty() { 1 } else { 2 };
		if !self.can_forward(message)
			|| targets.is_empty()
			|| targets.len() > 5
			|| targets
				.iter()
				.enumerate()
				.any(|(i, id)| !self.can_compose(*id) || targets[..i].contains(id))
			|| !model::message_options::valid(note, self.content_limit(), note.trim().is_empty())
			|| self.pending.len() + count > 64
			|| self.draft_bytes() + count * (note.len() + 128 + size_of::<Pending>())
				> crate::MAX_DRAFT_BYTES
		{
			self.status = "Forward unavailable: check destinations, connection and message limits";
			return Vec::new();
		}
		let source = self.timeline.get(message).unwrap().channel;
		let guild = self.channel(source).and_then(|c| c.guild);
		let mut commands = Vec::with_capacity(count);
		for &channel in targets {
			for content in
				std::iter::once(None).chain((!note.trim().is_empty()).then_some(Some(note)))
			{
				self.send_sequence += 1;
				let epoch = std::time::SystemTime::now()
					.duration_since(std::time::UNIX_EPOCH)
					.unwrap_or_default()
					.as_millis();
				let nonce = crate::fingerprint::nonce(epoch, self.send_sequence);
				self.pending.push(Pending {
					sticker: None,
					channel,
					content: content.unwrap_or("Forwarded message").into(),
					attachments: Vec::new(),
					nonce: nonce.clone(),
					delivery: Delivery::Sending,
					confirmed: None,
					forward: Some(if content.is_some() {
						ForwardPart::Note
					} else {
						ForwardPart::Message
					}),
					reply: None,
				});
				commands.push(if let Some(content) = content {
					Command::Send {
						sticker: None,
						channel,
						content: content.into(),
						nonce,
						reply: None,
					}
				} else {
					Command::Forward {
						source,
						message,
						guild,
						channel,
						nonce,
					}
				});
			}
		}
		commands
	}

	pub(crate) fn report_forward_failure(
		&mut self,
		channel: Id,
		part: ForwardPart,
		failure: Failure,
	) {
		let label = self.channel_toast_label(channel);
		let action = match part {
			ForwardPart::Message => ActionKind::Forward,
			ForwardPart::Note => ActionKind::ForwardNote,
		};
		self.push_write_error(action, label, failure);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Envelope, Event, tests::message};

	#[test]
	fn failed_note_is_distinct_from_the_confirmed_forward() {
		let channel = Id(1);
		let source = message(10);
		let mut state = State {
			auth: AuthState::Authenticated,
			gateway_connected: true,
			user: Some(source.author.clone()),
			selected: Some(channel),
			channels: vec![model::Channel {
				id: channel,
				kind: 1,
				name: "Synthetic destination".into(),
				guild: None,
				parent_id: None,
				position: 0,
				recipients: vec![],
				last_message: None,
				icon: None,
				member_list_id: None,
				tags: None,
				message_count: None,
				user_limit: None,
				slowmode: 0,
			}],
			..State::default()
		};
		state.timeline.insert(source, false, false).unwrap();
		let commands = state.prepare_forward(Id(10), &[channel], "Optional note");
		let [
			Command::Forward { nonce: forward, .. },
			Command::Send { nonce: note, .. },
		] = commands.as_slice()
		else {
			panic!("Forward followed by its optional note");
		};
		assert!(state.pending[0].is_forwarded_message());
		assert!(!state.pending[1].is_forwarded_message());
		state.apply(Envelope {
			generation: state.generation,
			event: Event::SendResult {
				nonce: forward.clone(),
				result: Ok(message(11)),
			},
		});
		state.apply(Envelope {
			generation: state.generation,
			event: Event::SendResult {
				nonce: note.clone(),
				result: Err(Failure::Forbidden),
			},
		});
		assert_eq!(state.pending.len(), 1);
		assert_eq!(state.pending[0].content, "Optional note");
		assert_eq!(state.pending[0].delivery, Delivery::Rejected);
		let errors = state.take_action_errors();
		assert_eq!(errors.len(), 1);
		assert_eq!(errors[0].action, ActionKind::ForwardNote);
	}
}
