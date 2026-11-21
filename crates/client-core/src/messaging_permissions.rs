//! Account messaging preferences with optimistic toggles, bounded queue, and rollback.
use crate::optimistic::ActionKind;
use crate::{
	Command, State,
	auth::{AuthState, Failure},
	optimistic::WriteKind,
};
use model::messaging_permissions::{Change, Snapshot};
use std::collections::VecDeque;

const MAX_QUEUED_CHANGES: usize = 16;

#[derive(Default)]
pub struct Settings {
	pub snapshot: Option<Snapshot>,
	pub pending: bool,
	pub error: Option<Failure>,
	request: u64,
	confirmed: Option<Snapshot>,
	queued: VecDeque<Change>,
}

impl State {
	pub fn request_messaging_permissions(&mut self) -> Option<Command> {
		if self.messaging_permissions.pending {
			return None;
		}
		if !self.demo && (self.auth != AuthState::Authenticated || !self.gateway_connected) {
			self.messaging_permissions.error = Some(Failure::ProtocolAt(
				"Connect to Discord to update messaging permissions",
			));
			return None;
		}
		self.messaging_permissions.error = None;
		if self.demo {
			let snapshot = self
				.messaging_permissions
				.snapshot
				.clone()
				.unwrap_or_default();
			if snapshot.valid() {
				self.messaging_permissions.snapshot = Some(snapshot);
			} else {
				self.messaging_permissions.error = Some(Failure::Capacity);
			}
			return None;
		}
		self.messaging_permissions.request =
			self.messaging_permissions.request.wrapping_add(1).max(1);
		self.messaging_permissions.pending = true;
		self.messaging_permissions.confirmed = None;
		self.messaging_permissions.queued.clear();
		Some(Command::MessagingPermissions {
			request: self.messaging_permissions.request,
			change: None,
		})
	}

	pub fn update_messaging_permissions(&mut self, change: Change) -> Option<Command> {
		if !change.valid() || self.messaging_permissions.snapshot.is_none() {
			return None;
		}
		// Do not overlap with an initial GET load (pending && confirmed.is_none()).
		if self.messaging_permissions.pending && self.messaging_permissions.confirmed.is_none() {
			return None;
		}
		if !self.demo && (self.auth != AuthState::Authenticated || !self.gateway_connected) {
			self.messaging_permissions.error = Some(Failure::ProtocolAt(
				"Connect to Discord to update messaging permissions",
			));
			return None;
		}
		let current = self.messaging_permissions.snapshot.clone()?;
		let mut next = current.clone();
		change.apply(&mut next);
		if !next.valid() {
			self.messaging_permissions.error = Some(Failure::Capacity);
			return None;
		}
		if next == current {
			return None;
		}
		self.messaging_permissions.error = None;
		self.messaging_permissions.snapshot = Some(next.clone());
		self.revision = self.revision.wrapping_add(1);
		if self.demo {
			return None;
		}
		if self.messaging_permissions.pending {
			if self.messaging_permissions.queued.len() >= MAX_QUEUED_CHANGES {
				self.messaging_permissions.snapshot = Some(current);
				self.messaging_permissions.error = Some(Failure::Capacity);
				return None;
			}
			self.messaging_permissions.queued.push_back(change);
			if self
				.messaging_permissions
				.confirmed
				.as_ref()
				.is_some_and(|c| *c == next)
			{
				return Some(Command::CancelPendingWrite {
					kind: WriteKind::MessagingPermissions,
					request: self.messaging_permissions.request,
				});
			}
			return None;
		}
		self.messaging_permissions.confirmed = Some(current);
		self.messaging_permissions.request =
			self.messaging_permissions.request.wrapping_add(1).max(1);
		self.messaging_permissions.pending = true;
		Some(Command::MessagingPermissions {
			request: self.messaging_permissions.request,
			change: Some(change),
		})
	}

	pub(crate) fn cancel_messaging_permissions_pre_wire(&mut self, request: u64) {
		if !self.messaging_permissions.pending || request != self.messaging_permissions.request {
			return;
		}
		let Some(confirmed) = self.messaging_permissions.confirmed.clone() else {
			return;
		};
		if self.messaging_permissions.snapshot.as_ref() == Some(&confirmed) {
			self.messaging_permissions.pending = false;
			self.messaging_permissions.confirmed = None;
			self.messaging_permissions.queued.clear();
			self.revision = self.revision.wrapping_add(1);
			return;
		}
		while let Some(next_change) = self.messaging_permissions.queued.pop_front() {
			let mut candidate = confirmed.clone();
			next_change.apply(&mut candidate);
			if candidate == confirmed {
				continue;
			}
			self.messaging_permissions.request =
				self.messaging_permissions.request.wrapping_add(1).max(1);
			self.push_follow_up(Command::MessagingPermissions {
				request: self.messaging_permissions.request,
				change: Some(next_change),
			});
			return;
		}
		self.messaging_permissions.pending = false;
		self.messaging_permissions.confirmed = None;
		self.messaging_permissions.snapshot = Some(confirmed);
		self.revision = self.revision.wrapping_add(1);
	}

	pub fn apply_messaging_permissions(&mut self, request: u64, result: Result<Snapshot, Failure>) {
		if !self.messaging_permissions.pending || request != self.messaging_permissions.request {
			return;
		}
		let had_write = self.messaging_permissions.confirmed.is_some();
		let result = result.and_then(|snapshot| {
			if snapshot.valid() {
				Ok(snapshot)
			} else {
				Err(Failure::Capacity)
			}
		});
		match result {
			Ok(snapshot) => {
				while let Some(next_change) = self.messaging_permissions.queued.pop_front() {
					let mut after_next = snapshot.clone();
					next_change.apply(&mut after_next);
					if after_next == snapshot {
						continue;
					}
					let mut visible = after_next;
					for queued in &self.messaging_permissions.queued {
						queued.apply(&mut visible);
					}
					self.messaging_permissions.confirmed = Some(snapshot);
					self.messaging_permissions.snapshot = Some(visible);
					self.messaging_permissions.request =
						self.messaging_permissions.request.wrapping_add(1).max(1);
					self.messaging_permissions.error = None;
					self.push_follow_up(Command::MessagingPermissions {
						request: self.messaging_permissions.request,
						change: Some(next_change),
					});
					self.revision = self.revision.wrapping_add(1);
					return;
				}
				self.messaging_permissions.pending = false;
				self.messaging_permissions.confirmed = None;
				self.messaging_permissions.snapshot = Some(snapshot);
				self.messaging_permissions.error = None;
				self.revision = self.revision.wrapping_add(1);
			}
			Err(failure) => {
				self.messaging_permissions.pending = false;
				self.messaging_permissions.queued.clear();
				if let Some(confirmed) = self.messaging_permissions.confirmed.take() {
					self.messaging_permissions.snapshot = Some(confirmed);
				}
				self.messaging_permissions.error = Some(failure);
				if failure.ends_session() && failure != Failure::Capacity {
					self.fail(failure);
				} else if had_write {
					self.push_write_error(ActionKind::PrivacySetting, "", failure);
				}
				self.revision = self.revision.wrapping_add(1);
			}
		}
	}

	pub(crate) fn invalidate_messaging_permissions(&mut self, failure: Option<Failure>) {
		self.messaging_permissions.snapshot = None;
		self.messaging_permissions.confirmed = None;
		self.messaging_permissions.queued.clear();
		self.messaging_permissions.pending = false;
		self.messaging_permissions.error = failure;
		self.messaging_permissions.request = self.messaging_permissions.request.wrapping_add(1);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn optimistic_changes_reject_stale_responses_rollback_and_demo_stays_offline() {
		let mut state = State {
			auth: AuthState::Authenticated,
			gateway_connected: true,
			..State::default()
		};
		let Command::MessagingPermissions { request, .. } =
			state.request_messaging_permissions().unwrap()
		else {
			panic!()
		};
		state.apply_messaging_permissions(request + 1, Ok(Snapshot::default()));
		assert!(state.messaging_permissions.snapshot.is_none());
		state.apply_messaging_permissions(request, Ok(Snapshot::default()));
		let command = state
			.update_messaging_permissions(Change::DefaultAllowDms(false))
			.unwrap();
		// Optimistic toggle takes effect immediately.
		assert!(
			!state
				.messaging_permissions
				.snapshot
				.as_ref()
				.unwrap()
				.default_allow_dms
		);
		// Failure rolls back to confirmed snapshot and emits an action error toast.
		state.command_rejected(command);
		assert!(!state.messaging_permissions.pending);
		assert!(state.messaging_permissions.error.is_some());
		assert!(
			state
				.messaging_permissions
				.snapshot
				.as_ref()
				.unwrap()
				.default_allow_dms
		);
		assert_eq!(state.take_action_errors().len(), 1);
		let Command::MessagingPermissions { request, .. } =
			state.request_messaging_permissions().unwrap()
		else {
			panic!()
		};
		state.invalidate_messaging_permissions(None);
		state.apply_messaging_permissions(request, Ok(Snapshot::default()));
		assert!(state.messaging_permissions.snapshot.is_none());
		state.logout();
		state.demo = true;
		assert!(state.request_messaging_permissions().is_none());
		assert!(
			state
				.update_messaging_permissions(Change::DefaultAllowDms(false))
				.is_none()
		);
		assert!(
			!state
				.messaging_permissions
				.snapshot
				.unwrap()
				.default_allow_dms
		);
	}
}
