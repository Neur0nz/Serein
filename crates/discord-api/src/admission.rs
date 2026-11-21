//! One in-flight request per normalized route or learned service bucket. Feedback is
//! applied before the next request is admitted; unrelated routes keep their concurrency.
//! Only fixed-size hashes are retained, never URLs, tokens or response header strings.
use client_core::auth::Failure;
use std::{
	hash::{Hash, Hasher},
	sync::Mutex,
	time::Duration,
};
use tokio::{
	sync::Notify,
	time::{Instant, sleep_until},
};
use wreq::{Method, header::HeaderMap};

const MAX_ENTRIES: usize = 128;
const MAX_BUCKET_BYTES: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Route {
	name: u64,
	major: u64,
}

fn hash(value: impl Hash) -> u64 {
	let mut hasher = std::collections::hash_map::DefaultHasher::new();
	value.hash(&mut hasher);
	hasher.finish()
}

fn route(method: &Method, path: &str) -> Route {
	let mut name = std::collections::hash_map::DefaultHasher::new();
	method.hash(&mut name);
	let mut major = std::collections::hash_map::DefaultHasher::new();
	let path = path.split('?').next().unwrap_or(path);
	let mut previous = "";
	for (index, part) in path.split('/').filter(|part| !part.is_empty()).enumerate() {
		let numeric = part.bytes().all(|byte| byte.is_ascii_digit());
		// Only a top-level snowflake names a major resource. In particular,
		// /guilds/templates/... must not invent a resource named "templates".
		if index == 1 && numeric && matches!(previous, "channels" | "guilds" | "webhooks") {
			(previous, part).hash(&mut major);
		}
		if numeric {
			"{id}".hash(&mut name);
		} else {
			part.hash(&mut name);
		}
		previous = part;
	}
	Route {
		name: name.finish(),
		major: major.finish(),
	}
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
	Route(Route),
	Bucket(u64, u64),
}

struct Slot {
	key: Key,
	busy: usize,
	until: Instant,
}

struct State {
	routes: Vec<(Route, Key)>,
	slots: Vec<Slot>,
}

const _: () = assert!(MAX_ENTRIES * (size_of::<(Route, Key)>() + size_of::<Slot>()) <= 16 * 1024);

impl Default for State {
	fn default() -> Self {
		Self {
			routes: Vec::with_capacity(MAX_ENTRIES),
			slots: Vec::with_capacity(MAX_ENTRIES),
		}
	}
}

impl State {
	fn collect(&mut self) {
		self.slots
			.retain(|slot| slot.busy != 0 || self.routes.iter().any(|(_, key)| *key == slot.key));
	}

	fn index(&mut self, route: Route, now: Instant) -> Result<usize, Failure> {
		if let Some((_, key)) = self.routes.iter().find(|(value, _)| *value == route) {
			return self
				.slots
				.iter()
				.position(|slot| slot.key == *key)
				.ok_or(Failure::Protocol);
		}
		if self.routes.len() == MAX_ENTRIES {
			let index = self
				.routes
				.iter()
				.position(|(_, key)| {
					self.slots
						.iter()
						.any(|slot| slot.key == *key && slot.busy == 0 && slot.until <= now)
				})
				.ok_or(Failure::Capacity)?;
			self.routes.remove(index);
			self.collect();
		}
		if self.slots.len() == MAX_ENTRIES {
			return Err(Failure::Capacity);
		}
		let key = Key::Route(route);
		self.routes.push((route, key));
		self.slots.push(Slot {
			key,
			busy: 0,
			until: now,
		});
		Ok(self.slots.len() - 1)
	}
}

#[derive(Default)]
pub(crate) struct Admission {
	state: Mutex<State>,
	changed: Notify,
}

pub(crate) struct Ticket<'a> {
	admission: &'a Admission,
	route: Route,
	key: Key,
}

impl Admission {
	pub(crate) async fn acquire(
		&self,
		method: &Method,
		path: &str,
		speculative: bool,
	) -> Result<Ticket<'_>, Failure> {
		let route = route(method, path);
		loop {
			// Register before inspecting state: a response/drop must not lose its wakeup.
			let changed = self.changed.notified();
			tokio::pin!(changed);
			changed.as_mut().enable();
			let until = {
				let now = Instant::now();
				let mut state = self.state.lock().map_err(|_| Failure::Protocol)?;
				let index = state.index(route, now)?;
				let slot = &mut state.slots[index];
				if slot.busy == 0 && slot.until <= now {
					slot.busy = 1;
					return Ok(Ticket {
						admission: self,
						route,
						key: slot.key,
					});
				}
				if speculative {
					return Err(Failure::Capacity);
				}
				(slot.until > now).then_some(slot.until)
			};
			match until {
				Some(until) => {
					tokio::select! { () = &mut changed => {}, () = sleep_until(until) => {} }
				}
				None => changed.await,
			}
		}
	}
}

impl Ticket<'_> {
	/// Merge aliases using the service's bucket id plus major resource. Unknown or
	/// malformed metadata retains route serialization rather than inventing a quota.
	pub(crate) fn feedback(&mut self, headers: &HeaderMap) {
		let Ok(mut state) = self.admission.state.lock() else {
			return;
		};
		let bucket = headers
			.get("x-ratelimit-bucket")
			.filter(|value| !value.is_empty() && value.len() <= MAX_BUCKET_BYTES)
			.map(|value| Key::Bucket(hash(value.as_bytes()), self.route.major));
		if let Some(key) = bucket.filter(|key| *key != self.key) {
			let target = state
				.slots
				.iter()
				.position(|slot| slot.key == key)
				.or_else(|| {
					(state.slots.len() < MAX_ENTRIES).then(|| {
						state.slots.push(Slot {
							key,
							busy: 0,
							until: Instant::now(),
						});
						state.slots.len() - 1
					})
				});
			if let Some(target) = target {
				let old = state
					.slots
					.iter()
					.position(|slot| slot.key == self.key)
					.expect("active admission slot");
				let until = state.slots[old].until;
				state.slots[old].busy -= 1;
				state.slots[target].busy += 1;
				state.slots[target].until = state.slots[target].until.max(until);
				if let Some((_, mapped)) = state
					.routes
					.iter_mut()
					.find(|(route, _)| *route == self.route)
				{
					*mapped = key;
				}
				self.key = key;
				state.collect();
			}
		}
		self.admission.changed.notify_waiters();
	}

	/// The caller validates service delays with the adapter's shared delay parser.
	pub(crate) fn hold(&self, delay: Duration) {
		if let Ok(mut state) = self.admission.state.lock()
			&& let Some(slot) = state.slots.iter_mut().find(|slot| slot.key == self.key)
		{
			slot.until = slot.until.max(Instant::now() + delay);
		}
	}
}

impl Drop for Ticket<'_> {
	fn drop(&mut self) {
		if let Ok(mut state) = self.admission.state.lock()
			&& let Some(slot) = state.slots.iter_mut().find(|slot| slot.key == self.key)
		{
			slot.busy -= 1;
		}
		self.admission.changed.notify_waiters();
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::DiscordApi;
	use client_core::auth::SessionSecret;
	use std::{
		future::{Future, poll_fn},
		pin::Pin,
		sync::{
			Arc,
			atomic::{AtomicBool, Ordering},
		},
		task::Poll,
	};

	async fn pending(mut future: Pin<&mut impl Future>) -> bool {
		poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await
	}

	#[tokio::test]
	async fn verification_expiring_during_admission_is_not_sent() {
		let api = DiscordApi::new(Arc::new(
			SessionSecret::from_owner_input("SYNTHETIC_SECRET_MARKER".into()).unwrap(),
		))
		.unwrap();
		let held = api
			.admission
			.acquire(&Method::POST, "/channels/1/messages", false)
			.await
			.unwrap();
		let expired = AtomicBool::new(false);
		let request = api.admit_request(&Method::POST, "/channels/1/messages", false, || {
			expired.load(Ordering::Relaxed)
		});
		tokio::pin!(request);
		assert!(pending(request.as_mut()).await);
		expired.store(true, Ordering::Relaxed);
		drop(held);
		assert!(matches!(request.await, Err(Failure::ProtocolAt(_))));
		assert!(
			api.admission
				.acquire(&Method::POST, "/channels/1/messages", true)
				.await
				.is_ok()
		);
	}

	#[tokio::test]
	async fn exhausted_success_holds_its_route_without_stalling_another_channel() {
		use tokio::{
			io::{AsyncReadExt, AsyncWriteExt},
			net::TcpListener,
		};
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let mut api = DiscordApi::new(Arc::new(
			SessionSecret::from_owner_input("SYNTHETIC_SECRET_MARKER".into()).unwrap(),
		))
		.unwrap();
		api.base = format!("http://{}", listener.local_addr().unwrap());
		let server = tokio::spawn(async move {
			for channel in [1, 2] {
				let (mut socket, _) = listener.accept().await.unwrap();
				let mut head = Vec::new();
				let mut buffer = [0; 2048];
				while !head.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
					let length = socket.read(&mut buffer).await.unwrap();
					assert!(length != 0 && head.len() + length <= 8192);
					head.extend_from_slice(&buffer[..length]);
				}
				assert!(
					String::from_utf8_lossy(&head)
						.starts_with(&format!("GET /channels/{channel}/messages "))
				);
				let remaining = if channel == 1 { 0 } else { 1 };
				socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-RateLimit-Bucket: shared\r\nX-RateLimit-Remaining: {remaining}\r\nX-RateLimit-Reset-After: 3600\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").as_bytes()).await.unwrap();
			}
		});
		api.request(Method::GET, "/channels/1/messages", None)
			.await
			.unwrap();
		assert_eq!(
			crate::SPECULATIVE
				.scope((), api.request(Method::GET, "/channels/1/messages", None))
				.await,
			Err(Failure::Capacity)
		);
		crate::SPECULATIVE
			.scope((), api.request(Method::GET, "/channels/2/messages", None))
			.await
			.unwrap();
		server.await.unwrap();
	}

	#[tokio::test]
	async fn security_stop_wakes_route_and_cooldown_waiters_without_dispatch() {
		let mut api = DiscordApi::new(Arc::new(
			SessionSecret::from_owner_input("SYNTHETIC_SECRET_MARKER".into()).unwrap(),
		))
		.unwrap();
		// A broken admission path must still never send a synthetic credential online.
		api.base = "http://127.0.0.1:9".into();
		let _held = api
			.admission
			.acquire(&Method::GET, "/users/@me", false)
			.await
			.unwrap();
		let request = api.request(Method::GET, "/users/@me", None);
		tokio::pin!(request);
		assert!(pending(request.as_mut()).await);
		*api.cooldown.lock().await = Instant::now() + Duration::from_secs(3600);
		let cooldown = api.wait_cooldown(false);
		tokio::pin!(cooldown);
		assert!(pending(cooldown.as_mut()).await);
		api.stop_with(Failure::AccountRestricted);
		assert_eq!(request.await, Err(Failure::AccountRestricted));
		assert_eq!(cooldown.await, Err(Failure::AccountRestricted));
	}

	#[tokio::test]
	async fn discovery_aliases_major_resources_and_cancellation() {
		let admission = Admission::default();
		let mut first = admission
			.acquire(&Method::GET, "/channels/1/messages?limit=50", false)
			.await
			.unwrap();
		let next = admission.acquire(&Method::GET, "/channels/1/messages?limit=10", false);
		tokio::pin!(next);
		assert!(pending(next.as_mut()).await);
		let headers = HeaderMap::from_iter([(
			"x-ratelimit-bucket".parse().unwrap(),
			"shared".parse().unwrap(),
		)]);
		first.feedback(&headers);
		drop(first);
		let next = next.await.unwrap();
		drop(next);
		let mut alias = admission
			.acquire(&Method::POST, "/channels/1/messages", false)
			.await
			.unwrap();
		alias.feedback(&headers);
		assert!(
			admission
				.acquire(&Method::GET, "/channels/1/messages", true)
				.await
				.is_err()
		);
		// The same server bucket does not merge different major resources.
		let mut other = admission
			.acquire(&Method::GET, "/channels/2/messages", false)
			.await
			.unwrap();
		other.feedback(&headers);
		drop(other);
		drop(alias);
		assert!(
			admission
				.acquire(&Method::GET, "/channels/1/messages", true)
				.await
				.is_ok()
		);
		let mut template = admission
			.acquire(&Method::GET, "/guilds/templates/example", false)
			.await
			.unwrap();
		template.feedback(&headers);
		// Both endpoints lack a top-level major ID. A static route word must
		// not split a shared service bucket into a fictitious per-resource one.
		let mut global = admission
			.acquire(&Method::GET, "/users/@me/guilds", false)
			.await
			.unwrap();
		global.feedback(&headers);
		assert!(
			admission
				.acquire(&Method::GET, "/guilds/templates/example", true)
				.await
				.is_err()
		);
	}

	#[tokio::test]
	async fn bounded_tracking_does_not_evict_a_live_cooldown() {
		let admission = Admission::default();
		let mut first = admission
			.acquire(&Method::GET, "/channels/1/messages", false)
			.await
			.unwrap();
		// An oversized bucket id must keep the bounded route gate rather than
		// creating a new service bucket. A subsequent hold must still apply.
		first.feedback(&HeaderMap::from_iter([(
			"x-ratelimit-bucket".parse().unwrap(),
			"x".repeat(MAX_BUCKET_BYTES + 1).parse().unwrap(),
		)]));
		drop(first);
		let first = admission
			.acquire(&Method::GET, "/channels/1/messages", true)
			.await
			.unwrap();
		first.hold(Duration::from_secs(60));
		drop(first);
		for channel in 2..=300 {
			drop(
				admission
					.acquire(
						&Method::GET,
						&format!("/channels/{channel}/messages"),
						false,
					)
					.await
					.unwrap(),
			);
		}
		assert!(
			admission
				.acquire(&Method::GET, "/channels/1/messages", true)
				.await
				.is_err()
		);
		let state = admission.state.lock().unwrap();
		assert!(state.routes.len() <= MAX_ENTRIES && state.slots.len() <= MAX_ENTRIES);
	}

	#[tokio::test]
	async fn pre_wire_cancel_aborts_admission_wait_without_touching_network() {
		let mut api = DiscordApi::new(Arc::new(
			SessionSecret::from_owner_input("SYNTHETIC_SECRET_MARKER".into()).unwrap(),
		))
		.unwrap();
		// Unreachable loopback port guarantees any accidental wire dispatch fails instead of cancelling.
		api.base = "http://127.0.0.1:9".into();
		let held = api
			.admission
			.acquire(&Method::DELETE, "/channels/1/messages/2", false)
			.await
			.unwrap();
		let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
		let fut = api.execute_background_write(
			client_core::Command::Delete {
				channel: model::Id(1),
				message: model::Id(2),
				request: 7,
			},
			Some(cancel_rx),
			Instant::now() + Duration::from_secs(25),
		);
		tokio::pin!(fut);
		assert!(pending(fut.as_mut()).await);
		assert_eq!(api.requests.available_permits(), crate::REST_PERMITS);
		let _ = cancel_tx.send(true);
		assert!(matches!(fut.await, crate::WriteOutcome::Cancelled));
		drop(held);
		assert!(
			api.admission
				.acquire(&Method::DELETE, "/channels/1/messages/2", true)
				.await
				.is_ok()
		);
	}

	async fn accept_request(
		listener: &tokio::net::TcpListener,
		expected: &str,
	) -> tokio::net::TcpStream {
		use tokio::io::AsyncReadExt as _;
		let (mut socket, _) = listener.accept().await.unwrap();
		let mut headers = Vec::new();
		while !headers.windows(4).any(|window| window == b"\r\n\r\n") {
			let mut chunk = [0; 512];
			let len = socket.read(&mut chunk).await.unwrap();
			assert!(len > 0 && headers.len() < 4096);
			headers.extend_from_slice(&chunk[..len]);
		}
		assert!(headers.starts_with(expected.as_bytes()));
		socket
	}

	#[tokio::test]
	async fn background_deadline_does_not_abort_dispatched_request() {
		use tokio::io::AsyncWriteExt as _;
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let mut api = DiscordApi::new(Arc::new(
			SessionSecret::from_owner_input("SYNTHETIC_SECRET_MARKER".into()).unwrap(),
		))
		.unwrap();
		api.base = format!("http://{}", listener.local_addr().unwrap());
		let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
		let mut task = tokio::spawn(async move {
			api.execute_background_write(
				client_core::Command::Delete {
					channel: model::Id(1),
					message: model::Id(2),
					request: 7,
				},
				Some(cancel_rx),
				Instant::now() + Duration::from_secs(5),
			)
			.await
		});
		let mut socket = tokio::select! {
			socket = accept_request(&listener, "DELETE /channels/1/messages/2 ") => socket,
			_ = &mut task => panic!("write finished before dispatch"),
		};
		tokio::time::pause();
		tokio::time::advance(Duration::from_secs(6)).await;
		tokio::time::resume();
		cancel_tx.send(true).unwrap();
		socket
			.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
			.await
			.unwrap();
		assert!(matches!(
			task.await.unwrap(),
			crate::WriteOutcome::Event(event)
				if matches!(*event, client_core::Event::Deleted { result: Ok(()), .. })
		));
	}

	#[tokio::test]
	async fn preflight_read_preserves_cancellation_before_patch() {
		use tokio::io::AsyncWriteExt as _;
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let mut api = DiscordApi::new(Arc::new(
			SessionSecret::from_owner_input("SYNTHETIC_SECRET_MARKER".into()).unwrap(),
		))
		.unwrap();
		api.base = format!("http://{}", listener.local_addr().unwrap());
		let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
		let mut task = tokio::spawn(async move {
			api.execute_background_write(
				client_core::Command::MessagingPermissions {
					request: 7,
					change: Some(model::messaging_permissions::Change::DefaultAllowDms(false)),
				},
				Some(cancel_rx),
				Instant::now() + Duration::from_secs(25),
			)
			.await
		});
		let mut socket = tokio::select! {
			socket = accept_request(&listener, "GET /users/@me/settings-proto/1 ") => socket,
			_ = &mut task => panic!("request finished before the settings read"),
		};
		cancel_tx.send(true).unwrap();
		let body = r#"{"settings":"CgIYBw=="}"#;
		socket
			.write_all(
				format!(
					"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
					body.len()
				)
				.as_bytes(),
			)
			.await
			.unwrap();
		tokio::select! {
			result = task => assert!(matches!(result.unwrap(), crate::WriteOutcome::Cancelled)),
			_ = listener.accept() => panic!("superseded PATCH reached the service"),
		}
	}
}
