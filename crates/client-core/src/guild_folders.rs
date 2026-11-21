//! Account server organization with optimistic updates, coalesced follow-ups, and rollback.
use crate::optimistic::ActionKind;
use crate::{
	Command, State,
	auth::{AuthState, Failure},
};
use model::guild_folders::Settings;

impl State {
	pub fn load_guild_folders(&mut self) -> Option<Command> {
		if self.folders_pending
			|| (!self.demo && (self.auth != AuthState::Authenticated || !self.gateway_connected))
		{
			return None;
		}
		self.folders_error = None;
		self.folders_stale = false;
		if self.demo {
			self.guild_folders.get_or_insert_with(Settings::default);
			return None;
		}
		self.folders_confirmed = None;
		self.folders_queued = None;
		Some(self.guild_folder_command(None))
	}

	pub fn save_guild_folders(&mut self, mut settings: Settings) -> Option<Command> {
		if self.guild_folders.is_none()
			|| (!self.demo && (self.auth != AuthState::Authenticated || !self.gateway_connected))
		{
			return None;
		}
		// Do not overlap with an initial GET load (folders_pending && folders_confirmed.is_none()).
		if self.folders_pending && self.folders_confirmed.is_none() {
			return None;
		}
		settings.folders.shrink_to_fit();
		for folder in &mut settings.folders {
			folder.guild_ids.shrink_to_fit();
		}
		if !settings.valid() {
			self.folders_error = Some("Server folder layout exceeds supported limits");
			return None;
		}
		self.folders_error = None;
		let current = self.guild_folders.clone()?;
		if current == settings {
			return None;
		}
		self.guild_folders = Some(settings.clone());
		self.revision = self.revision.wrapping_add(1);
		if self.demo {
			return None;
		}
		if self.folders_pending {
			// Coalesce latest desired folder layout while an earlier PATCH is in flight.
			self.folders_queued = Some(settings);
			return None;
		}
		self.folders_confirmed = Some(current.clone());
		Some(self.guild_folder_command(Some((current, settings))))
	}

	fn guild_folder_command(&mut self, change: Option<(Settings, Settings)>) -> Command {
		self.folders_request = self.folders_request.wrapping_add(1);
		self.folders_pending = true;
		Command::GuildFolders {
			request: self.folders_request,
			change,
		}
	}

	pub(crate) fn interrupt_guild_folders(&mut self) {
		if !self.folders_pending {
			return;
		}
		self.folders_pending = false;
		self.folders_stale = true;
		self.folders_queued = None;
		if let Some(confirmed) = self.folders_confirmed.take() {
			self.guild_folders = Some(confirmed);
		}
	}

	pub(crate) fn apply_guild_folders(&mut self, request: u64, result: Result<Settings, Failure>) {
		if !self.folders_pending || request != self.folders_request {
			return;
		}
		let had_write = self.folders_confirmed.is_some();
		match result {
			Ok(settings) if settings.valid() => {
				if let Some(mut next) = self.folders_queued.take()
					&& next.folders != settings.folders
				{
					next.version = settings.version;
					self.folders_confirmed = Some(settings.clone());
					self.guild_folders = Some(next.clone());
					self.folders_error = None;
					let command = self.guild_folder_command(Some((settings, next)));
					self.push_follow_up(command);
					self.revision = self.revision.wrapping_add(1);
					return;
				}
				self.folders_pending = false;
				self.folders_confirmed = None;
				self.guild_folders = Some(settings);
				self.folders_error = None;
				self.revision = self.revision.wrapping_add(1);
			}
			Ok(_) => {
				self.folders_pending = false;
				self.folders_queued = None;
				if let Some(confirmed) = self.folders_confirmed.take() {
					self.guild_folders = Some(confirmed);
				}
				self.folders_error = Some("Server folder response exceeds supported limits");
				if had_write {
					self.push_write_error(ActionKind::SaveFolders, "", Failure::Protocol);
				}
				self.revision = self.revision.wrapping_add(1);
			}
			Err(failure) => {
				self.folders_pending = false;
				self.folders_queued = None;
				if let Some(confirmed) = self.folders_confirmed.take() {
					self.guild_folders = Some(confirmed);
				}
				self.folders_error = Some(failure.label());
				if failure.ends_session() && failure != Failure::Capacity {
					self.fail(failure);
				} else if had_write {
					self.push_write_error(ActionKind::SaveFolders, "", failure);
				}
				self.revision = self.revision.wrapping_add(1);
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Envelope, Event};
	use model::{Id, guild_folders::Folder};

	#[test]
	fn folder_writes_apply_optimistically_coalesce_and_rollback_on_failure() {
		let original = Settings::default();
		let mut state = State {
			auth: AuthState::Authenticated,
			gateway_connected: true,
			guild_folders: Some(original.clone()),
			..State::default()
		};
		let changed = Settings {
			folders: vec![Folder {
				id: Some(1),
				guild_ids: vec![Id(10)],
				name: Some("First".into()),
				color: None,
			}],
			version: 1,
		};
		let changed2 = Settings {
			folders: vec![Folder {
				id: Some(1),
				guild_ids: vec![Id(10), Id(20)],
				name: Some("Second".into()),
				color: None,
			}],
			version: 2,
		};
		let command = state.save_guild_folders(changed.clone()).unwrap();
		// Applied optimistically right away.
		assert_eq!(state.guild_folders.as_ref(), Some(&changed));
		// Identical layout is a no-op; a newer layout coalesces without dispatching a second write.
		assert!(state.save_guild_folders(changed.clone()).is_none());
		assert!(state.save_guild_folders(changed2.clone()).is_none());
		assert_eq!(state.guild_folders.as_ref(), Some(&changed2));
		// Rejection restores the confirmed layout, drops queued edits, and pushes an action error.
		state.command_rejected(command);
		assert!(!state.folders_pending);
		assert!(state.folders_error.is_some());
		assert_eq!(state.guild_folders.as_ref(), Some(&original));
		assert_eq!(state.take_action_errors().len(), 1);
		// Retry succeeds and dispatches coalesced follow-up when a second edit arrives in flight.
		let Command::GuildFolders { request, .. } =
			state.save_guild_folders(changed.clone()).unwrap()
		else {
			panic!("expected a folder write");
		};
		assert!(state.save_guild_folders(changed2.clone()).is_none());
		let generation = state.generation;
		state.apply(Envelope {
			generation,
			event: Event::GuildFolders {
				request,
				result: Ok(changed.clone()),
			},
		});
		assert!(state.folders_pending);
		let follow_ups = state.take_follow_up_commands();
		let [Command::GuildFolders { request, .. }] = follow_ups.as_slice() else {
			panic!("expected one follow-up folder write");
		};
		let request = *request;
		state.apply(Envelope {
			generation,
			event: Event::GuildFolders {
				request,
				result: Ok(changed2.clone()),
			},
		});
		assert!(!state.folders_pending);
		assert_eq!(state.guild_folders.as_ref(), Some(&changed2));
		state.logout();
		state.apply(Envelope {
			generation,
			event: Event::GuildFolders {
				request,
				result: Ok(changed),
			},
		});
		assert!(state.guild_folders.is_none());
	}

	#[test]
	fn late_folder_response_does_not_settle_reload_after_resume() {
		let confirmed = Settings::default();
		let mut state = State {
			auth: AuthState::Authenticated,
			gateway_connected: true,
			guild_folders: Some(confirmed.clone()),
			..State::default()
		};
		let mut changed = confirmed.clone();
		changed.folders.push(Folder {
			id: None,
			guild_ids: vec![Id(10)],
			name: None,
			color: None,
		});
		let Command::GuildFolders {
			request: write_request,
			..
		} = state.save_guild_folders(changed.clone()).unwrap()
		else {
			panic!("expected a folder write");
		};
		let generation = state.generation;
		state.apply(Envelope {
			generation,
			event: Event::Disconnected,
		});
		state.apply(Envelope {
			generation,
			event: Event::Resumed,
		});
		assert!(state.folders_stale);
		let Command::GuildFolders {
			request: reload_request,
			..
		} = state.load_guild_folders().unwrap()
		else {
			panic!("expected a folder reload");
		};
		state.apply(Envelope {
			generation,
			event: Event::GuildFolders {
				request: write_request,
				result: Ok(changed),
			},
		});
		assert!(state.folders_pending);
		assert_eq!(state.guild_folders.as_ref(), Some(&confirmed));
		state.apply(Envelope {
			generation,
			event: Event::GuildFolders {
				request: reload_request,
				result: Ok(confirmed.clone()),
			},
		});
		assert!(!state.folders_pending);
		assert_eq!(state.guild_folders.as_ref(), Some(&confirmed));
	}
}
