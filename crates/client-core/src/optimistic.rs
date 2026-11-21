//! Bounded optimistic write state machines shared across client-core domains.
//!
//! - [`CoalescedSlot`] provides latest-wins coalescing for idempotent state transitions
//!   (mutes, notification settings, reactions, notes, nicknames, message edits, folder layout,
//!   poll votes, messaging permissions). Never auto-retries on failure.
//! - [`InFlightSet`] provides per-target deduplicated concurrency for one-shot transitions
//!   (friend requests, message deletes, server invites, closing DMs, confirmed leaves).

use crate::auth::Failure;
use std::collections::BTreeMap;

/// Default maximum tracked keys per [`CoalescedSlot`] domain.
pub const MAX_KEYS: usize = 32;
/// Default maximum concurrent targets per [`InFlightSet`] domain.
pub const MAX_IN_FLIGHT: usize = 16;
/// Maximum queued error notices surfaced to the UI toast queue.
pub const MAX_ACTION_ERRORS: usize = 16;
/// Byte ceiling for a display name in an error notice.
pub const MAX_ACTION_ERROR_BYTES: usize = 256;
/// Maximum follow-up commands buffered during event application.
pub const MAX_FOLLOW_UP_COMMANDS: usize = 64;

/// Identifies a cancellable background write lane for pre-wire admission cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WriteKind {
	UserAction,
	ServerAction,
	ChannelAction,
	GroupAction,
	Reaction,
	ReadState,
	Edit,
	Pin,
	Poll,
	GuildFolders,
	MessagingPermissions,
}

/// One tracked key inside [`CoalescedSlot`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry<V> {
	pub confirmed: V,
	pub in_flight: Option<(u64, V)>,
	pub desired: V,
}

/// Result of calling [`CoalescedSlot::set`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetOutcome {
	/// Slot reached its compile-time key ceiling; no state changed.
	Full,
	/// Desired value recorded without dispatching a new HTTP write right now.
	/// If `cancel_in_flight` is `Some(seq)`, the currently queued request is now superseded
	/// and may be cancelled pre-wire before reaching HTTP dispatch.
	Queued { cancel_in_flight: Option<u64> },
	/// Dispatch a new HTTP write with this request sequence immediately.
	Dispatch(u64),
}

/// Result of completing an in-flight write in [`CoalescedSlot`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Finish<V> {
	/// Sequence was stale or already cleared.
	Stale,
	/// Confirmed state matches desired; entry removed cleanly.
	Settled { confirmed: V },
	/// Confirmed state updated, but desired differs; dispatch one coalesced follow-up.
	DispatchFollowUp { seq: u64, value: V },
	/// Write failed without retry; entry removed and visible state reverted to `previous`.
	RolledBack {
		previous: V,
		failed: V,
		failure: Failure,
	},
}

/// Result of a pre-wire cancellation acknowledgement in [`CoalescedSlot`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cancelled<V> {
	/// Sequence was stale or already completed.
	Stale,
	/// Pre-wire cancellation succeeded and `desired == confirmed`; zero network writes needed.
	Settled,
	/// Pre-wire cancellation succeeded and `desired != confirmed`; dispatch `value` now.
	DispatchFollowUp { seq: u64, value: V },
}

/// Bounded latest-wins coalescing slot for idempotent state transitions.
#[derive(Clone, Debug)]
pub struct CoalescedSlot<K, V, const MAX: usize = MAX_KEYS> {
	entries: BTreeMap<K, Entry<V>>,
	next_seq: u64,
}

impl<K: Ord, V, const MAX: usize> Default for CoalescedSlot<K, V, MAX> {
	fn default() -> Self {
		Self {
			entries: BTreeMap::new(),
			next_seq: 0,
		}
	}
}

impl<K: Ord, V: Clone + Eq, const MAX: usize> CoalescedSlot<K, V, MAX> {
	pub fn new() -> Self {
		Self::default()
	}

	fn alloc_seq(&mut self) -> u64 {
		self.next_seq = self.next_seq.wrapping_add(1).max(1);
		self.next_seq
	}

	/// Updates `desired` for `key`, returning whether to dispatch immediately, coalesce
	/// behind an active write (with optional pre-wire cancellation), or reject at capacity.
	pub fn set(&mut self, key: K, confirmed_baseline: V, next_value: V) -> SetOutcome {
		if let Some(entry) = self.entries.get_mut(&key) {
			entry.desired = next_value;
			if let Some((seq, ref wire_value)) = entry.in_flight {
				let cancel_in_flight = (entry.desired != *wire_value).then_some(seq);
				return SetOutcome::Queued { cancel_in_flight };
			}
			if entry.desired == entry.confirmed {
				self.entries.remove(&key);
				return SetOutcome::Queued {
					cancel_in_flight: None,
				};
			}
			let seq = self.alloc_seq();
			let wire = self
				.entries
				.get(&key)
				.map(|entry| entry.desired.clone())
				.expect("entry present");
			if let Some(entry) = self.entries.get_mut(&key) {
				entry.in_flight = Some((seq, wire));
			}
			return SetOutcome::Dispatch(seq);
		}
		if next_value == confirmed_baseline {
			return SetOutcome::Queued {
				cancel_in_flight: None,
			};
		}
		if self.entries.len() >= MAX {
			return SetOutcome::Full;
		}
		let seq = self.alloc_seq();
		self.entries.insert(
			key,
			Entry {
				confirmed: confirmed_baseline,
				in_flight: Some((seq, next_value.clone())),
				desired: next_value,
			},
		);
		SetOutcome::Dispatch(seq)
	}

	/// Adopts a newer authoritative value (e.g. from a Gateway event) without clobbering
	/// a newer in-flight or desired local edit.
	pub fn observe_authoritative(&mut self, key: &K, value: V) {
		let remove = if let Some(entry) = self.entries.get_mut(key) {
			entry.confirmed = value;
			entry.in_flight.is_none() && entry.desired == entry.confirmed
		} else {
			false
		};
		if remove {
			self.entries.remove(key);
		}
	}

	/// Completes an in-flight write whose server-confirmed value equals the sent value on `Ok(())`.
	pub fn finish(&mut self, key: &K, seq: u64, result: Result<(), Failure>) -> Finish<V> {
		let Some(entry) = self.entries.get_mut(key) else {
			return Finish::Stale;
		};
		if entry.in_flight.as_ref().map(|(s, _)| *s) != Some(seq) {
			return Finish::Stale;
		}
		let (_, wire_value) = entry.in_flight.take().expect("checked above");
		let result = result.map(|()| wire_value.clone());
		self.finish_taken(key, wire_value, result)
	}

	/// Completes an in-flight write when the server returns an authoritative confirmed value.
	pub fn finish_with_confirmed(
		&mut self,
		key: &K,
		seq: u64,
		result: Result<V, Failure>,
	) -> Finish<V> {
		let Some(entry) = self.entries.get_mut(key) else {
			return Finish::Stale;
		};
		if entry.in_flight.as_ref().map(|(s, _)| *s) != Some(seq) {
			return Finish::Stale;
		}
		let (_, wire_value) = entry.in_flight.take().expect("checked above");
		self.finish_taken(key, wire_value, result)
	}

	fn finish_taken(&mut self, key: &K, wire_value: V, result: Result<V, Failure>) -> Finish<V> {
		match result {
			Ok(confirmed_value) => {
				let follow_up = {
					let entry = self.entries.get_mut(key).expect("entry present");
					entry.confirmed = confirmed_value;
					(entry.desired != entry.confirmed).then(|| entry.desired.clone())
				};
				if let Some(value) = follow_up {
					let next_seq = self.alloc_seq();
					if let Some(entry) = self.entries.get_mut(key) {
						entry.in_flight = Some((next_seq, value.clone()));
					}
					Finish::DispatchFollowUp {
						seq: next_seq,
						value,
					}
				} else {
					let entry = self.entries.remove(key).expect("entry present");
					Finish::Settled {
						confirmed: entry.confirmed,
					}
				}
			}
			Err(failure) => {
				let entry = self.entries.remove(key).expect("entry present");
				Finish::RolledBack {
					previous: entry.confirmed,
					failed: wire_value,
					failure,
				}
			}
		}
	}

	pub fn desired(&self, key: &K) -> Option<&V> {
		self.entries.get(key).map(|entry| &entry.desired)
	}

	pub fn entry(&self, key: &K) -> Option<&Entry<V>> {
		self.entries.get(key)
	}

	pub(crate) fn remove(&mut self, key: &K) {
		self.entries.remove(key);
	}

	pub fn is_in_flight(&self) -> bool {
		self.entries.values().any(|entry| entry.in_flight.is_some())
	}

	pub fn in_flight_count(&self) -> usize {
		self.entries
			.values()
			.filter(|entry| entry.in_flight.is_some())
			.count()
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn iter(&self) -> impl Iterator<Item = (&K, &Entry<V>)> {
		self.entries.iter()
	}

	/// Clears all tracked entries while keeping `next_seq` monotonic so late responses
	/// from prior sessions/disconnects are ignored as stale.
	pub fn cancel_all(&mut self) -> Vec<(K, Entry<V>)> {
		let entries = std::mem::take(&mut self.entries);
		entries.into_iter().collect()
	}

	pub fn clear(&mut self) {
		self.entries.clear();
	}
}

impl<K: Ord + Clone, V: Clone + Eq, const MAX: usize> CoalescedSlot<K, V, MAX> {
	pub fn find_key_by_seq(&self, seq: u64) -> Option<K> {
		self.entries.iter().find_map(|(key, entry)| {
			(entry.in_flight.as_ref().map(|(s, _)| *s) == Some(seq)).then(|| key.clone())
		})
	}

	pub fn finish_by_seq(
		&mut self,
		seq: u64,
		result: Result<(), Failure>,
	) -> Option<(K, Finish<V>)> {
		let key = self.find_key_by_seq(seq)?;
		let outcome = self.finish(&key, seq, result);
		Some((key, outcome))
	}

	pub fn finish_by_seq_with_confirmed(
		&mut self,
		seq: u64,
		result: Result<V, Failure>,
	) -> Option<(K, Finish<V>)> {
		let key = self.find_key_by_seq(seq)?;
		let outcome = self.finish_with_confirmed(&key, seq, result);
		Some((key, outcome))
	}

	/// Handles a confirmed pre-wire cancellation (the request never touched the network).
	pub fn cancelled_by_seq(&mut self, seq: u64) -> Option<(K, Cancelled<V>)> {
		let key = self.find_key_by_seq(seq)?;
		let follow_up = {
			let entry = self.entries.get_mut(&key)?;
			entry.in_flight = None;
			(entry.desired != entry.confirmed).then(|| entry.desired.clone())
		};
		if let Some(value) = follow_up {
			let next_seq = self.alloc_seq();
			if let Some(entry) = self.entries.get_mut(&key) {
				entry.in_flight = Some((next_seq, value.clone()));
			}
			Some((
				key,
				Cancelled::DispatchFollowUp {
					seq: next_seq,
					value,
				},
			))
		} else {
			self.entries.remove(&key);
			Some((key, Cancelled::Settled))
		}
	}
}

/// Error returned when [`InFlightSet::insert`] refuses a target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InsertError<T> {
	Duplicate(T),
	Full(T),
}

/// Bounded per-target deduplicated concurrent set for one-shot transitions.
#[derive(Clone, Debug)]
pub struct InFlightSet<K, T, const MAX: usize = MAX_IN_FLIGHT> {
	entries: BTreeMap<K, (u64, T)>,
	next_seq: u64,
}

impl<K: Ord, T, const MAX: usize> Default for InFlightSet<K, T, MAX> {
	fn default() -> Self {
		Self {
			entries: BTreeMap::new(),
			next_seq: 0,
		}
	}
}

impl<K: Ord, T, const MAX: usize> InFlightSet<K, T, MAX> {
	pub fn new() -> Self {
		Self::default()
	}

	fn alloc_seq(&mut self) -> u64 {
		self.next_seq = self.next_seq.wrapping_add(1).max(1);
		self.next_seq
	}

	pub fn insert(&mut self, key: K, payload: T) -> Result<u64, InsertError<T>> {
		if self.entries.contains_key(&key) {
			return Err(InsertError::Duplicate(payload));
		}
		if self.entries.len() >= MAX {
			return Err(InsertError::Full(payload));
		}
		let seq = self.alloc_seq();
		self.entries.insert(key, (seq, payload));
		Ok(seq)
	}

	pub fn finish(&mut self, key: &K, seq: u64) -> Option<T> {
		if self.entries.get(key).map(|(s, _)| *s) != Some(seq) {
			return None;
		}
		self.entries.remove(key).map(|(_, payload)| payload)
	}

	pub fn remove(&mut self, key: &K) -> Option<(u64, T)> {
		self.entries.remove(key)
	}

	pub fn get(&self, key: &K) -> Option<&T> {
		self.entries.get(key).map(|(_, payload)| payload)
	}

	pub fn seq(&self, key: &K) -> Option<u64> {
		self.entries.get(key).map(|(seq, _)| *seq)
	}

	pub fn get_mut(&mut self, key: &K) -> Option<&mut T> {
		self.entries.get_mut(key).map(|(_, payload)| payload)
	}

	pub fn contains(&self, key: &K) -> bool {
		self.entries.contains_key(key)
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	pub fn iter(&self) -> impl Iterator<Item = (&K, &T)> {
		self.entries
			.iter()
			.map(|(key, (_, payload))| (key, payload))
	}

	pub fn cancel_all(&mut self) -> Vec<(K, T)> {
		let entries = std::mem::take(&mut self.entries);
		entries
			.into_iter()
			.map(|(key, (_, payload))| (key, payload))
			.collect()
	}

	pub fn clear(&mut self) {
		self.entries.clear();
	}
}

impl<K: Ord + Clone, T, const MAX: usize> InFlightSet<K, T, MAX> {
	pub fn finish_by_seq(&mut self, seq: u64) -> Option<(K, T)> {
		let key = self
			.entries
			.iter()
			.find_map(|(key, (s, _))| (*s == seq).then(|| key.clone()))?;
		self.entries.remove(&key).map(|(_, payload)| (key, payload))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn coalesced_slot_rapid_toggle_coalesces_and_reverts_without_retry() {
		let mut slot = CoalescedSlot::<u64, bool>::new();
		// Mute -> Unmute -> Mute while first write is on the wire sends only the 1st write.
		let seq1 = match slot.set(1, false, true) {
			SetOutcome::Dispatch(seq) => seq,
			other => panic!("expected dispatch, got {other:?}"),
		};
		assert_eq!(slot.desired(&1), Some(&true));
		// Second click back to false requests pre-wire cancel of seq1.
		assert_eq!(
			slot.set(1, false, false),
			SetOutcome::Queued {
				cancel_in_flight: Some(seq1)
			}
		);
		// Third click back to true matches wire_value again.
		assert_eq!(
			slot.set(1, false, true),
			SetOutcome::Queued {
				cancel_in_flight: None
			}
		);
		assert_eq!(
			slot.finish(&1, seq1, Ok(())),
			Finish::Settled { confirmed: true }
		);
		assert!(slot.is_empty());

		// Mute -> Unmute while in-flight dispatches exactly one follow-up on completion.
		let seq2 = match slot.set(2, false, true) {
			SetOutcome::Dispatch(seq) => seq,
			other => panic!("expected dispatch, got {other:?}"),
		};
		assert_eq!(
			slot.set(2, false, false),
			SetOutcome::Queued {
				cancel_in_flight: Some(seq2)
			}
		);
		let seq3 = match slot.finish(&2, seq2, Ok(())) {
			Finish::DispatchFollowUp { seq, value: false } => seq,
			other => panic!("expected follow-up, got {other:?}"),
		};
		assert_ne!(seq2, seq3);
		// Failure rolls back to confirmed (true, from seq2) without auto-retry.
		assert_eq!(
			slot.finish(&2, seq3, Err(Failure::RateLimited)),
			Finish::RolledBack {
				previous: true,
				failed: false,
				failure: Failure::RateLimited,
			}
		);
		assert!(slot.is_empty());
	}

	#[test]
	fn coalesced_slot_pre_wire_cancel_settles_or_dispatches_latest() {
		let mut slot = CoalescedSlot::<u64, &'static str>::new();
		// Case A: Toggle A -> B -> A cancelled pre-wire settles with zero HTTP sends.
		let seq1 = match slot.set(1, "a", "b") {
			SetOutcome::Dispatch(seq) => seq,
			other => panic!("expected dispatch, got {other:?}"),
		};
		assert_eq!(
			slot.set(1, "a", "a"),
			SetOutcome::Queued {
				cancel_in_flight: Some(seq1)
			}
		);
		assert_eq!(slot.cancelled_by_seq(seq1), Some((1, Cancelled::Settled)));
		assert!(slot.is_empty());

		// Case B: Edit v0 -> v1 -> v2 cancelled pre-wire dispatches v2 directly.
		let seq2 = match slot.set(2, "v0", "v1") {
			SetOutcome::Dispatch(seq) => seq,
			other => panic!("expected dispatch, got {other:?}"),
		};
		assert_eq!(
			slot.set(2, "v0", "v2"),
			SetOutcome::Queued {
				cancel_in_flight: Some(seq2)
			}
		);
		match slot.cancelled_by_seq(seq2) {
			Some((2, Cancelled::DispatchFollowUp { seq, value: "v2" })) => {
				assert_ne!(seq, seq2);
				assert_eq!(
					slot.finish(&2, seq, Ok(())),
					Finish::Settled { confirmed: "v2" }
				);
			}
			other => panic!("expected DispatchFollowUp(v2), got {other:?}"),
		}
	}

	#[test]
	fn coalesced_slot_and_in_flight_set_enforce_bounds_and_monotonic_sequences() {
		let mut slot = CoalescedSlot::<usize, u32, 2>::new();
		let s1 = match slot.set(1, 0, 1) {
			SetOutcome::Dispatch(s) => s,
			_ => panic!("expected dispatch"),
		};
		assert!(matches!(slot.set(2, 0, 1), SetOutcome::Dispatch(_)));
		assert_eq!(slot.set(3, 0, 1), SetOutcome::Full);
		let cancelled = slot.cancel_all();
		assert_eq!(cancelled.len(), 2);
		assert_eq!(slot.finish(&1, s1, Ok(())), Finish::Stale);
		// Sequence remains monotonic after cancel_all so old responses cannot collide.
		let s_after = match slot.set(1, 0, 2) {
			SetOutcome::Dispatch(s) => s,
			_ => panic!("expected dispatch"),
		};
		assert!(s_after > s1);

		let mut set = InFlightSet::<usize, &'static str, 2>::new();
		let i1 = set.insert(10, "a").unwrap();
		assert_eq!(set.insert(10, "dup"), Err(InsertError::Duplicate("dup")));
		let i2 = set.insert(20, "b").unwrap();
		assert_eq!(set.insert(30, "full"), Err(InsertError::Full("full")));
		assert_eq!(set.finish(&10, i1), Some("a"));
		assert_eq!(set.finish_by_seq(i2), Some((20, "b")));
		assert!(set.is_empty());
	}
}
