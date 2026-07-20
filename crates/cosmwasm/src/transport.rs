//! Pluggable JSON-RPC transports for the live CosmWasm provider.
//!
//! The seam is deliberately a *string envelope*: tendermint-rpc's `Client` trait is generic
//! (`perform<R>`), therefore not object safe, and its `#[async_trait]` demands `Send`, which
//! this Rc-based, current-thread codebase cannot offer. So a [`CosmosTransport`] moves complete
//! JSON-RPC 2.0 envelopes (`{"jsonrpc","id","method","params"}`) as raw strings, while
//! tendermint-rpc stays in charge of serialization and typed parsing on either side.
//!
//! Two transports ship:
//!
//! - [`HttpTransport`]: one POST per call, behaviorally identical to the `HttpClient` the
//!   provider built per request before this seam existed.
//! - [`BatchHttpTransport`]: merges concurrent calls into CometBFT JSON-RPC batch requests
//!   (JSON array bodies), timer paced in the style of interchainjs' batch client: a lazy
//!   interval timer runs only while there is work, each tick drains at most `max_size` queued
//!   calls into one POST, and bursts simply ride later ticks (there is no fire-when-full early
//!   flush). A dispatched POST never blocks the tick loop, so slow responses overlap the next
//!   tick's POST. Note that some public RPC gateways reject array bodies, so batching is opt
//!   in, never the default.

use std::cell::{Cell, RefCell};
use std::future::{poll_fn, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::task::Poll;
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::{Instant, MissedTickBehavior};

use crate::error::CwError;

/// A boxed non-`Send` future, matching the repo's Rc/current-thread world while keeping
/// [`CosmosTransport`] dyn safe.
pub type TransportFuture<'a> = Pin<Box<dyn Future<Output = Result<String, CwError>> + 'a>>;

/// One CometBFT JSON-RPC call.
///
/// `request` is a complete JSON-RPC 2.0 envelope (`{"jsonrpc","id","method","params"}`, the
/// exact output of tendermint-rpc's `RequestMessage::into_json`); the call resolves to the raw
/// response envelope for that id, ready for `Response::from_string`. JSON-RPC *error* envelopes
/// come back `Ok` too: converting them into typed errors is the parser's job, not the
/// transport's.
pub trait CosmosTransport {
    /// Send one request envelope and return the matching response envelope.
    fn call(&self, request: String) -> TransportFuture<'_>;
}

/// The seam under [`BatchHttpTransport`]: POST an arbitrary JSON body (a bare envelope or a
/// batch array) and return the response body text. [`HttpTransport`] implements it for real;
/// unit tests inject a fake so the batch algorithm runs without a network.
pub(crate) trait JsonRpcPost {
    /// POST `body` as `application/json` and return the response body text.
    fn post(&self, body: String) -> TransportFuture<'_>;
}

/// One POST per call over reqwest. The default transport; behavior matches the per-request
/// `HttpClient` the provider used before the transport seam existed.
pub struct HttpTransport {
    /// Normalized endpoint; `None` when the chain has no `rpc_url` (or an empty one), which
    /// surfaces as an error at the first call rather than at construction, preserving the
    /// provider's infallible-constructor contract.
    url: Option<String>,
    /// Kept only for the no-url error message.
    chain_id: String,
    http: reqwest::Client,
}

impl HttpTransport {
    /// Build a transport for `rpc_url`. Tendermint convention allows `tcp://` endpoints, which
    /// reqwest rejects, so those map to `http://`. A missing or empty url is accepted here and
    /// errors lazily on the first [`call`](CosmosTransport::call).
    pub fn new(rpc_url: Option<&str>, chain_id: &str) -> Self {
        Self {
            url: rpc_url.filter(|u| !u.is_empty()).map(normalize_url),
            chain_id: chain_id.to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// The endpoint to POST to, or the same no-url error the provider has always raised.
    fn endpoint(&self) -> Result<&str, CwError> {
        self.url.as_deref().ok_or_else(|| {
            CwError::Rpc(format!(
                "chain '{}' has no rpc_url; use a chain preset with an endpoint",
                self.chain_id
            ))
        })
    }
}

/// Map a `tcp://` endpoint to `http://`; anything else passes through verbatim.
fn normalize_url(url: &str) -> String {
    match url.strip_prefix("tcp://") {
        Some(rest) => format!("http://{rest}"),
        None => url.to_string(),
    }
}

impl JsonRpcPost for HttpTransport {
    fn post(&self, body: String) -> TransportFuture<'_> {
        Box::pin(async move {
            let url = self.endpoint()?;
            let resp = self
                .http
                .post(url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body)
                .send()
                .await
                .map_err(|e| CwError::Rpc(e.to_string()))?
                .error_for_status()
                .map_err(|e| CwError::Rpc(e.to_string()))?;
            resp.text().await.map_err(|e| CwError::Rpc(e.to_string()))
        })
    }
}

impl CosmosTransport for HttpTransport {
    fn call(&self, request: String) -> TransportFuture<'_> {
        self.post(request)
    }
}

/// How a [`BatchHttpTransport`] coalesces calls: a timer ticks every `interval`, and each tick
/// drains at most `max_size` queued calls into one POST. A fuller queue waits for later ticks;
/// nothing flushes early.
#[derive(Clone, Copy, Debug)]
pub struct BatchConfig {
    /// The tick period: how often the leader drains the queue. The first drain happens one
    /// full `interval` after the leader starts, never immediately.
    pub interval: Duration,
    /// The largest batch a single POST carries; anything beyond it rides the next tick.
    pub max_size: usize,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(20),
            max_size: 20,
        }
    }
}

/// A call parked in the batch queue: its envelope, its id (for routing the response), and the
/// channel its caller awaits.
struct Pending {
    id: serde_json::Value,
    envelope: String,
    tx: oneshot::Sender<Result<String, CwError>>,
}

/// Merges concurrent JSON-RPC calls into CometBFT batch requests.
///
/// Leader driven, no `spawn_local`: the first caller to find the leader flag clear becomes the
/// leader and runs the tick loop, draining at most `max_size` queued calls into one POST per
/// tick. Everyone else just parks a [`Pending`] and awaits its oneshot. Batch responses arrive
/// in arbitrary order, so routing matches on the JSON-RPC id, never on position.
pub struct BatchHttpTransport {
    poster: Rc<dyn JsonRpcPost>,
    cfg: BatchConfig,
    queue: RefCell<Vec<Pending>>,
    /// True while some call future is driving the tick loop. Guarded by [`LeaderGuard`] so a
    /// cancelled leader hands the queue to the next caller instead of stranding it.
    leader: Cell<bool>,
}

impl BatchHttpTransport {
    /// Build a batching transport over a real [`HttpTransport`] for `rpc_url`. The same lazy
    /// no-url behavior applies: construction never fails, the first flush does.
    pub fn new(rpc_url: Option<&str>, chain_id: &str, cfg: BatchConfig) -> Self {
        Self::with_poster(Rc::new(HttpTransport::new(rpc_url, chain_id)), cfg)
    }

    /// Test seam: run the batch algorithm over an arbitrary poster.
    pub(crate) fn with_poster(poster: Rc<dyn JsonRpcPost>, mut cfg: BatchConfig) -> Self {
        // A zero max_size would drain zero-length chunks forever; one is the working floor.
        cfg.max_size = cfg.max_size.max(1);
        Self {
            poster,
            cfg,
            queue: RefCell::new(Vec::new()),
            leader: Cell::new(false),
        }
    }

    /// The leader's tick loop: every `interval`, drain at most `max_size` queued calls and
    /// start their POST, without awaiting it. In-flight POSTs are driven concurrently with the
    /// timer, so a slow response never delays the next tick's dispatch. The loop (and with it
    /// the timer) ends once the queue is empty and no POST is in flight; the next call that
    /// arrives after that starts a fresh leader.
    async fn lead(&self) {
        let mut interval =
            tokio::time::interval_at(Instant::now() + self.cfg.interval, self.cfg.interval);
        // A tick delayed by a slow poll reschedules from when it fired, keeping ticks paced
        // rather than bursting to catch up.
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // Flushes dispatched on earlier ticks, still awaiting their response.
        let mut in_flight: Vec<Pin<Box<dyn Future<Output = ()> + '_>>> = Vec::new();
        loop {
            // Wait for the next tick while polling in-flight flushes alongside; done entirely
            // when everything is drained and answered.
            let ticked = poll_fn(|cx| {
                in_flight.retain_mut(|flush| flush.as_mut().poll(cx).is_pending());
                if interval.poll_tick(cx).is_ready() {
                    return Poll::Ready(true);
                }
                if in_flight.is_empty() && self.queue.borrow().is_empty() {
                    return Poll::Ready(false);
                }
                Poll::Pending
            })
            .await;
            if !ticked {
                break;
            }
            // Drain before posting: no queue borrow may live across a poll of the flush.
            let chunk: Vec<Pending> = {
                let mut queue = self.queue.borrow_mut();
                let n = queue.len().min(self.cfg.max_size);
                queue.drain(..n).collect()
            };
            if !chunk.is_empty() {
                // Start the flush but do not await it: the next loop iteration polls it via
                // `in_flight`, concurrently with the timer.
                in_flight.push(Box::pin(self.flush(chunk)));
            }
        }
    }

    /// POST one chunk and route the response. A chunk of one posts the bare envelope (matching
    /// what a non-batching client would send); larger chunks post a JSON array. A failed POST
    /// errors every pending in the chunk.
    async fn flush(&self, chunk: Vec<Pending>) {
        let body = if chunk.len() == 1 {
            chunk[0].envelope.clone()
        } else {
            // The envelopes are already serialized JSON; join them rather than re-parsing.
            let joined = chunk
                .iter()
                .map(|p| p.envelope.as_str())
                .collect::<Vec<_>>()
                .join(",");
            format!("[{joined}]")
        };
        match self.poster.post(body).await {
            Ok(text) => route(chunk, &text),
            Err(e) => {
                let msg = format!("batch POST failed: {e}");
                for pending in chunk {
                    // A closed receiver means the caller cancelled; nothing to tell it.
                    let _ = pending.tx.send(Err(CwError::Rpc(msg.clone())));
                }
            }
        }
    }
}

/// Clears the leader flag when dropped, so a leader cancelled mid-flush (its future dropped at
/// an await point) hands leadership to the next caller instead of wedging the queue behind a
/// flag nobody holds.
struct LeaderGuard<'a>(&'a Cell<bool>);

impl Drop for LeaderGuard<'_> {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

impl CosmosTransport for BatchHttpTransport {
    fn call(&self, request: String) -> TransportFuture<'_> {
        Box::pin(async move {
            let id = request_id(&request)?;
            let (tx, rx) = oneshot::channel();
            self.queue.borrow_mut().push(Pending {
                id,
                envelope: request,
                tx,
            });
            // Single-threaded, and no await between the read and the set: the check is race
            // free. The guard outlives `lead()` so cancellation at any await inside it clears
            // the flag.
            if !self.leader.get() {
                self.leader.set(true);
                let _leading = LeaderGuard(&self.leader);
                self.lead().await;
            }
            match rx.await {
                Ok(result) => result,
                // The sender was dropped without a send: the leader was cancelled after
                // draining this pending out of the queue but before routing a response.
                Err(_) => Err(CwError::Rpc(
                    "batch flush was cancelled before a response arrived".into(),
                )),
            }
        })
    }
}

/// Extract the JSON-RPC id from a request envelope (tendermint-rpc stamps a UUIDv4 string, but
/// routing compares raw JSON values, so any non-null id works).
fn request_id(envelope: &str) -> Result<serde_json::Value, CwError> {
    let parsed: serde_json::Value = serde_json::from_str(envelope)
        .map_err(|e| CwError::Rpc(format!("request envelope is not JSON: {e}")))?;
    match parsed.get("id") {
        Some(id) if !id.is_null() => Ok(id.clone()),
        _ => Err(CwError::Rpc("request envelope carries no id".into())),
    }
}

/// Route a response body (a JSON array for a batch, a single object for a bare envelope) to the
/// chunk's pendings by JSON-RPC id. Pendings the response never mentions get an error; response
/// elements nobody claims are dropped (their caller already cancelled or never existed).
fn route(mut chunk: Vec<Pending>, body: &str) {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("batch response is not JSON: {e}");
            for pending in chunk {
                let _ = pending.tx.send(Err(CwError::Rpc(msg.clone())));
            }
            return;
        }
    };
    let elements = match parsed {
        serde_json::Value::Array(items) => items,
        single => vec![single],
    };
    // An `error` element that no pending owns (id null, absent, or unknown) is a whole-response
    // failure, not one call's error, so its message goes to every caller the response never
    // answered. Matched elements still route first; the first such orphan error wins.
    let mut whole_response_error: Option<String> = None;
    for element in elements {
        if let Some(pos) = chunk.iter().position(|p| Some(&p.id) == element.get("id")) {
            let pending = chunk.swap_remove(pos);
            // A matched-id error envelope rides through as Ok(text): `Response::from_string`
            // types it later.
            let _ = pending.tx.send(Ok(element.to_string()));
        } else if whole_response_error.is_none() {
            if let Some(err) = element.get("error") {
                whole_response_error = Some(rpc_error_message(err));
            }
        }
    }
    for pending in chunk {
        let msg = whole_response_error.clone().unwrap_or_else(|| {
            format!(
                "batch response carried no entry for request id {}",
                pending.id
            )
        });
        let _ = pending.tx.send(Err(CwError::Rpc(msg)));
    }
}

/// Render a JSON-RPC `error` object as an `Rpc` message: its `message`, plus `code` when the
/// node supplies one.
fn rpc_error_message(err: &serde_json::Value) -> String {
    let message = err
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown error");
    match err.get("code") {
        Some(code) if !code.is_null() => format!("batch response error: {message} (code {code})"),
        _ => format!("batch response error: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::future::poll_fn;
    use std::task::Poll;

    /// A request envelope shaped like `RequestMessage::into_json` output.
    fn envelope(id: &str, method: &str) -> String {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": {}}).to_string()
    }

    /// The result envelope a well-behaved node would return for `env`.
    fn result_for(env: &Value) -> Value {
        json!({"jsonrpc": "2.0", "id": env["id"], "result": {"echo": env["id"]}})
    }

    /// Answer every envelope in `body` with its own result, preserving order and arity
    /// (array in, array out; bare object in, bare object out).
    fn echo(body: &str) -> Result<String, CwError> {
        let parsed: Value = serde_json::from_str(body).expect("posted body is JSON");
        Ok(match parsed {
            Value::Array(items) => Value::Array(items.iter().map(result_for).collect()).to_string(),
            single => result_for(&single).to_string(),
        })
    }

    /// How a [`FakePoster`] answers a posted body.
    type Responder = Box<dyn Fn(&str) -> Result<String, CwError>>;

    /// Fake [`JsonRpcPost`]: records every posted body and answers via a closure.
    struct FakePoster {
        bodies: RefCell<Vec<String>>,
        respond: Responder,
    }

    impl FakePoster {
        fn with(respond: impl Fn(&str) -> Result<String, CwError> + 'static) -> Rc<Self> {
            Rc::new(Self {
                bodies: RefCell::new(Vec::new()),
                respond: Box::new(respond),
            })
        }

        fn echoing() -> Rc<Self> {
            Self::with(echo)
        }
    }

    impl JsonRpcPost for FakePoster {
        fn post(&self, body: String) -> TransportFuture<'_> {
            Box::pin(async move {
                self.bodies.borrow_mut().push(body.clone());
                (self.respond)(&body)
            })
        }
    }

    fn batch(poster: &Rc<FakePoster>, cfg: BatchConfig) -> BatchHttpTransport {
        BatchHttpTransport::with_poster(Rc::clone(poster) as Rc<dyn JsonRpcPost>, cfg)
    }

    fn parsed(response: Result<String, CwError>) -> Value {
        serde_json::from_str(&response.expect("call succeeds")).expect("response is JSON")
    }

    /// Polls `fut` exactly once and asserts it is still pending; drives paused-time tests one
    /// deterministic step at a time.
    async fn poll_pending(fut: &mut Pin<Box<impl Future>>) {
        poll_fn(|cx| {
            assert!(
                fut.as_mut().poll(cx).is_pending(),
                "future resolved too early"
            );
            Poll::Ready(())
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn calls_queued_before_the_first_tick_merge_into_one_post() {
        let poster = FakePoster::echoing();
        let transport = batch(&poster, BatchConfig::default());

        let (a, b) = tokio::join!(
            transport.call(envelope("id-a", "status")),
            transport.call(envelope("id-b", "abci_query")),
        );

        let bodies = poster.bodies.borrow();
        assert_eq!(bodies.len(), 1, "both calls rode one POST");
        let body: Value = serde_json::from_str(&bodies[0]).unwrap();
        assert_eq!(
            body.as_array().map(Vec::len),
            Some(2),
            "body is a 2-element array"
        );
        assert_eq!(parsed(a)["id"], "id-a");
        assert_eq!(parsed(b)["id"], "id-b");
    }

    #[tokio::test(start_paused = true)]
    async fn out_of_order_response_ids_route_to_the_right_callers() {
        let poster = FakePoster::with(|body| {
            let Value::Array(items) = serde_json::from_str(body).unwrap() else {
                panic!("expected a batch array");
            };
            let mut results: Vec<Value> = items.iter().map(result_for).collect();
            results.reverse();
            Ok(Value::Array(results).to_string())
        });
        let transport = batch(&poster, BatchConfig::default());

        let (a, b) = tokio::join!(
            transport.call(envelope("id-a", "status")),
            transport.call(envelope("id-b", "status")),
        );

        assert_eq!(parsed(a)["id"], "id-a");
        assert_eq!(parsed(b)["id"], "id-b");
    }

    #[tokio::test(start_paused = true)]
    async fn five_calls_at_max_size_two_pace_across_three_ticks() {
        let poster = FakePoster::echoing();
        let transport = batch(
            &poster,
            BatchConfig {
                interval: Duration::from_millis(20),
                max_size: 2,
            },
        );

        let mut calls = Box::pin(async {
            tokio::join!(
                transport.call(envelope("id-1", "status")),
                transport.call(envelope("id-2", "status")),
                transport.call(envelope("id-3", "status")),
                transport.call(envelope("id-4", "status")),
                transport.call(envelope("id-5", "status")),
            )
        });

        // All five enqueue immediately, but nothing may go out before the first tick.
        poll_pending(&mut calls).await;
        assert!(
            poster.bodies.borrow().is_empty(),
            "no POST before the leader's first tick"
        );
        tokio::time::advance(Duration::from_millis(19)).await;
        poll_pending(&mut calls).await;
        assert!(
            poster.bodies.borrow().is_empty(),
            "no POST before the first interval elapses"
        );

        // Tick 1 drains the first chunk; ticks 2 and 3 pace out the rest.
        tokio::time::advance(Duration::from_millis(1)).await;
        poll_pending(&mut calls).await;
        assert_eq!(poster.bodies.borrow().len(), 1, "tick 1 posts one chunk");
        tokio::time::advance(Duration::from_millis(20)).await;
        poll_pending(&mut calls).await;
        assert_eq!(
            poster.bodies.borrow().len(),
            2,
            "tick 2 posts the next chunk"
        );
        tokio::time::advance(Duration::from_millis(20)).await;
        let (r1, r2, r3, r4, r5) = calls.await;

        let bodies = poster.bodies.borrow();
        assert_eq!(bodies.len(), 3, "5 calls at max_size 2 pace into 3 POSTs");
        let sizes: Vec<usize> = bodies
            .iter()
            .map(|b| match serde_json::from_str::<Value>(b).unwrap() {
                Value::Array(items) => items.len(),
                _ => 1,
            })
            .collect();
        assert_eq!(sizes, vec![2, 2, 1]);
        for (response, id) in [
            (r1, "id-1"),
            (r2, "id-2"),
            (r3, "id-3"),
            (r4, "id-4"),
            (r5, "id-5"),
        ] {
            assert_eq!(parsed(response)["id"], id);
        }
    }

    /// A poster whose responses take `delay` of (paused) time: records the body at dispatch,
    /// answers after the sleep. Lets tests observe a POST that is started but unresolved.
    struct SlowPoster {
        bodies: RefCell<Vec<String>>,
        delay: Duration,
    }

    impl JsonRpcPost for SlowPoster {
        fn post(&self, body: String) -> TransportFuture<'_> {
            Box::pin(async move {
                self.bodies.borrow_mut().push(body.clone());
                tokio::time::sleep(self.delay).await;
                echo(&body)
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_post_does_not_block_the_next_tick() {
        // Each POST takes 50ms against a 20ms interval: tick 2's POST must go out while tick
        // 1's is still in flight.
        let poster = Rc::new(SlowPoster {
            bodies: RefCell::new(Vec::new()),
            delay: Duration::from_millis(50),
        });
        let transport = BatchHttpTransport::with_poster(
            Rc::clone(&poster) as Rc<dyn JsonRpcPost>,
            BatchConfig {
                interval: Duration::from_millis(20),
                max_size: 2,
            },
        );

        let mut calls = Box::pin(async {
            tokio::join!(
                transport.call(envelope("id-1", "status")),
                transport.call(envelope("id-2", "status")),
                transport.call(envelope("id-3", "status")),
            )
        });

        poll_pending(&mut calls).await;
        assert!(poster.bodies.borrow().is_empty());

        // t=20ms: tick 1 dispatches the first chunk; its response lands at t=70ms.
        tokio::time::advance(Duration::from_millis(20)).await;
        poll_pending(&mut calls).await;
        assert_eq!(poster.bodies.borrow().len(), 1, "tick 1 dispatched");

        // t=40ms: tick 2 dispatches the second chunk while POST 1 is still unresolved (the
        // join is still pending, so no response has been routed): the POSTs overlap.
        tokio::time::advance(Duration::from_millis(20)).await;
        poll_pending(&mut calls).await;
        assert_eq!(
            poster.bodies.borrow().len(),
            2,
            "tick 2's POST started while tick 1's was in flight"
        );

        // t=70ms: POST 1 resolves; id-3's caller still waits on POST 2's 90ms landing.
        tokio::time::advance(Duration::from_millis(30)).await;
        poll_pending(&mut calls).await;

        // t=90ms: POST 2 resolves and every caller gets its own response.
        tokio::time::advance(Duration::from_millis(20)).await;
        let (r1, r2, r3) = calls.await;
        for (response, id) in [(r1, "id-1"), (r2, "id-2"), (r3, "id-3")] {
            assert_eq!(parsed(response)["id"], id);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_single_call_posts_a_bare_envelope_and_takes_an_object_response() {
        let poster = FakePoster::echoing();
        let transport = batch(&poster, BatchConfig::default());

        let response = transport.call(envelope("id-solo", "status")).await;

        let bodies = poster.bodies.borrow();
        assert_eq!(bodies.len(), 1);
        let body: Value = serde_json::from_str(&bodies[0]).unwrap();
        assert!(
            body.is_object(),
            "a chunk of one posts the bare envelope, not a 1-array"
        );
        assert_eq!(body["method"], "status");
        assert_eq!(parsed(response)["id"], "id-solo");
    }

    #[tokio::test(start_paused = true)]
    async fn an_error_envelope_reaches_the_caller_that_owns_its_id() {
        let poster = FakePoster::with(|body| {
            let Value::Array(items) = serde_json::from_str(body).unwrap() else {
                panic!("expected a batch array");
            };
            let results: Vec<Value> = items
                .iter()
                .map(|env| {
                    if env["id"] == "id-bad" {
                        json!({"jsonrpc": "2.0", "id": env["id"],
                               "error": {"code": -32603, "message": "boom", "data": null}})
                    } else {
                        result_for(env)
                    }
                })
                .collect();
            Ok(Value::Array(results).to_string())
        });
        let transport = batch(&poster, BatchConfig::default());

        let (good, bad) = tokio::join!(
            transport.call(envelope("id-good", "status")),
            transport.call(envelope("id-bad", "status")),
        );

        // The error envelope is still an Ok(text) at the transport layer; typing it into an
        // error is the response parser's job.
        let good = parsed(good);
        assert_eq!(good["id"], "id-good");
        assert!(good.get("error").is_none());
        let bad = parsed(bad);
        assert_eq!(bad["id"], "id-bad");
        assert_eq!(bad["error"]["message"], "boom");
    }

    #[tokio::test(start_paused = true)]
    async fn a_top_level_error_reaches_every_caller_in_the_chunk() {
        // A node that rejects the whole batch answers with one id-null error object rather than
        // a per-id array; every caller in the chunk must surface it.
        let poster = FakePoster::with(|_| {
            Ok(json!({"jsonrpc": "2.0", "id": null,
                      "error": {"code": -32700, "message": "parse error"}})
            .to_string())
        });
        let transport = batch(&poster, BatchConfig::default());

        let (a, b) = tokio::join!(
            transport.call(envelope("id-a", "status")),
            transport.call(envelope("id-b", "status")),
        );

        for response in [a, b] {
            match response.unwrap_err() {
                CwError::Rpc(msg) => {
                    assert!(msg.contains("parse error"), "unexpected message: {msg}")
                }
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_partial_response_errors_only_the_missing_caller() {
        // The node answers every envelope except id-b, modeling a dropped reply in an otherwise
        // valid array.
        let poster = FakePoster::with(|body| {
            let Value::Array(items) = serde_json::from_str(body).unwrap() else {
                panic!("expected a batch array");
            };
            let results: Vec<Value> = items
                .iter()
                .filter(|env| env["id"] != "id-b")
                .map(result_for)
                .collect();
            Ok(Value::Array(results).to_string())
        });
        let transport = batch(&poster, BatchConfig::default());

        let (a, b) = tokio::join!(
            transport.call(envelope("id-a", "status")),
            transport.call(envelope("id-b", "status")),
        );

        assert_eq!(
            parsed(a)["id"],
            "id-a",
            "the answered call still resolves Ok"
        );
        match b.unwrap_err() {
            CwError::Rpc(msg) => assert!(msg.contains("id-b"), "unexpected message: {msg}"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_post_errors_every_pending_in_the_chunk() {
        let poster = FakePoster::with(|_| Err(CwError::Rpc("connection refused".into())));
        let transport = batch(&poster, BatchConfig::default());

        let (a, b) = tokio::join!(
            transport.call(envelope("id-a", "status")),
            transport.call(envelope("id-b", "status")),
        );

        for response in [a, b] {
            match response.unwrap_err() {
                CwError::Rpc(msg) => assert!(
                    msg.contains("connection refused"),
                    "unexpected message: {msg}"
                ),
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_missing_rpc_url_errors_with_the_provider_text() {
        for url in [None, Some("")] {
            let transport = HttpTransport::new(url, "nebula-9");
            match transport.call(envelope("id", "status")).await.unwrap_err() {
                CwError::Rpc(msg) => assert_eq!(
                    msg,
                    "chain 'nebula-9' has no rpc_url; use a chain preset with an endpoint"
                ),
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[test]
    fn tcp_urls_map_to_http_for_reqwest() {
        let transport = HttpTransport::new(Some("tcp://localhost:26657"), "local-1");
        assert_eq!(transport.url.as_deref(), Some("http://localhost:26657"));
        let transport = HttpTransport::new(Some("https://rpc.osmosis.zone"), "osmosis-1");
        assert_eq!(transport.url.as_deref(), Some("https://rpc.osmosis.zone"));
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_leader_leaves_the_queue_recoverable() {
        let poster = FakePoster::echoing();
        let transport = batch(&poster, BatchConfig::default());

        // Poll the first call exactly once: it enqueues, takes the leader flag, and parks
        // waiting on the first tick. Dropping it there models cancellation at an await point.
        let mut abandoned = Box::pin(transport.call(envelope("id-a", "status")));
        poll_fn(|cx| {
            assert!(abandoned.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(transport.leader.get(), "first caller took the leader flag");
        drop(abandoned);
        assert!(
            !transport.leader.get(),
            "the drop guard cleared the flag on cancellation"
        );
        assert_eq!(
            transport.queue.borrow().len(),
            1,
            "the cancelled call's envelope stays queued"
        );

        // The next caller becomes leader and flushes the abandoned envelope along with its own
        // (the abandoned response lands on a closed channel and is dropped).
        let response = transport.call(envelope("id-b", "status")).await;
        assert_eq!(parsed(response)["id"], "id-b");
        let bodies = poster.bodies.borrow();
        assert_eq!(bodies.len(), 1);
        assert!(
            bodies[0].contains("id-a"),
            "the abandoned envelope rode the recovery flush"
        );
        assert!(transport.queue.borrow().is_empty());
    }
}
