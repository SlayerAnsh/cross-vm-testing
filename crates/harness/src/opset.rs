//! Dyn-op registry: assemble a [`Harness`](crate::Harness) from standalone operation structs
//! instead of one enum plus match arms.
//!
//! Each operation is a struct implementing [`DynOp`] (its data plus its own `apply`). The
//! `'static` bounds throughout exist because a boxed trait object's type parameters must
//! outlive its (implicit `'static`) lifetime bound; `Ctx`/`World` types are owned in practice,
//! so the bounds cost nothing.

use core::fmt;
use core::future::Future;
use core::pin::Pin;
use std::collections::BTreeMap;

use crate::{CheckOutcome, Harness, HarnessError, Prng, Verdict};

/// Boxed future returned by the object-safe async methods in this module. Object safety
/// forbids `async fn` in the dyn traits, so implementations return `Box::pin(async move { .. })`.
pub type OpFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// One operation instance: its data plus its own apply. The dyn-registry counterpart of one
/// variant of an `Operation` enum plus that variant's match arm.
///
/// Implementors are named-field structs (possibly empty, `struct Ping {}`) that derive
/// `Debug`, `Clone`, and `serde::Serialize`/`serde::Deserialize`. `Debug` supplies the failure
/// dump and per-op stats label; the serde derives back `to_data` and the registered decoder so
/// the op flows through config, scenario, and replay. Write `clone_box` as
/// `Box::new(self.clone())` and `to_data` as `serde_json::to_value(self).expect("op data serializes")`.
pub trait DynOp<C: 'static, W: 'static>: fmt::Debug {
    /// The registered kind name of this op: exactly the name its [`OpDef`] is registered
    /// under (lowercase snake_case by convention, e.g. `"add"`). Config `kinds`/`weights`
    /// keys, scenario `op` tags, stats buckets, and replay artifacts all use this name.
    fn kind(&self) -> &'static str;

    /// Apply this operation against the live `ctx`, updating the persisted `world`. Same
    /// contract as [`Harness::apply`](crate::Harness::apply): `Ok` classifies the SUT response,
    /// `Err` is a confirmed bug or an infrastructure failure.
    fn apply<'a>(
        &'a self,
        ctx: &'a mut C,
        world: &'a mut W,
    ) -> OpFuture<'a, Result<Verdict, HarnessError>>;

    /// Clone into a fresh box. Powers `Clone` for `Box<dyn DynOp<C, W>>`, which the runner
    /// needs for replay and shrinking.
    fn clone_box(&self) -> Box<dyn DynOp<C, W>>;

    /// This op's own data as a JSON value, for reports and replay artifacts. Implementors
    /// derive `serde::Serialize` and write
    /// `serde_json::to_value(self).expect("op data serializes")`.
    fn to_data(&self) -> serde_json::Value;
}

impl<C: 'static, W: 'static> Clone for Box<dyn DynOp<C, W>> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// The `Operation` type of [`OpSetHarness`]: a boxed [`DynOp`] whose `Debug` leads with the
/// registered kind name. Stats, coverage, and failure dumps bucket by the leading `Debug`
/// token, so they use the same name configs use (`add`, not `Add`).
pub struct DynOperation<C: 'static, W: 'static>(pub Box<dyn DynOp<C, W>>);

impl<C: 'static, W: 'static> Clone for DynOperation<C, W> {
    fn clone(&self) -> Self {
        Self(self.0.clone_box())
    }
}

impl<C: 'static, W: 'static> fmt::Debug for DynOperation<C, W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {:?}", self.0.kind(), self.0)
    }
}

impl<C: 'static, W: 'static> core::ops::Deref for DynOperation<C, W> {
    type Target = dyn DynOp<C, W>;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

/// One named property that must always hold: the dyn-registry counterpart of one variant of
/// an `Invariant` enum plus its match arm in `check`.
///
/// Implementors derive `Debug` (coverage buckets by the leading `Debug` token, so the struct
/// name becomes the invariant label) and `Clone`, and write `clone_box` as
/// `Box::new(self.clone())`. Return [`CheckOutcome::skipped`](crate::CheckOutcome::skipped)
/// while a precondition has not happened yet.
pub trait DynInvariant<C: 'static, W: 'static>: fmt::Debug {
    /// Check the invariant against the current (post-operation) state. Same contract as
    /// [`Harness::check`](crate::Harness::check).
    fn check<'a>(&'a self, ctx: &'a mut C, world: &'a W) -> OpFuture<'a, CheckOutcome>;

    /// Clone into a fresh box. Powers `Clone` for `Box<dyn DynInvariant<C, W>>`.
    fn clone_box(&self) -> Box<dyn DynInvariant<C, W>>;
}

impl<C: 'static, W: 'static> Clone for Box<dyn DynInvariant<C, W>> {
    fn clone(&self) -> Self {
        self.clone_box()
    }
}

/// Generator stored in an [`OpDef`]: build one random op of this kind from `rng`, state-aware
/// via the world (mirrors [`Harness::generate_op`](crate::Harness::generate_op)). A plain fn
/// pointer keeps generation deterministic in `(seed, world)`.
pub type GenerateFn<C, W> = fn(&mut Prng, &W) -> Box<dyn DynOp<C, W>>;

/// Decoder stored in an [`OpDef`]: build one op of this kind from the data part of a config
/// scenario step or replay artifact. `op = { add = { n = 5 } }` passes `{"n": 5}` here;
/// `op = "ping"` passes `{}`. A plain fn pointer, like [`GenerateFn`].
pub type DecodeFn<C, W> = fn(serde_json::Value) -> Result<Box<dyn DynOp<C, W>>, String>;

/// The [`DecodeFn`] for any op struct that derives `serde::Deserialize`. Registration is
/// `OpDef::new("add", gen_add, decode_json_op::<Add, _, _>)`.
pub fn decode_json_op<T, C, W>(data: serde_json::Value) -> Result<Box<dyn DynOp<C, W>>, String>
where
    T: DynOp<C, W> + serde::de::DeserializeOwned + 'static,
    C: 'static,
    W: 'static,
{
    match serde_json::from_value::<T>(data) {
        Ok(op) => Ok(Box::new(op)),
        Err(e) => Err(e.to_string()),
    }
}

/// Dynamic selection weight stored in an [`OpDef`] (mirrors
/// [`Harness::weight`](crate::Harness::weight)): `0` excludes the kind while the state makes
/// it meaningless. Must be deterministic in `(ctx, world)`; it receives no rng by design.
pub type WeightFn<C, W> = fn(&C, &W) -> u32;

/// One registered operation kind: its name, its generator, and its dynamic weight. The
/// dyn-registry counterpart of one `OpKind` variant plus its `generate_op` and `weight` arms.
pub struct OpDef<C: 'static, W: 'static> {
    name: &'static str,
    generate: GenerateFn<C, W>,
    decode: DecodeFn<C, W>,
    weight: WeightFn<C, W>,
    description: Option<String>,
    field_docs: Vec<(String, String)>,
}

/// One op kind's introspectable documentation, surfaced by the CLI's `describe` subcommand:
/// the registered kind name plus any opt-in help attached via [`OpDef::with_help`].
/// `description` is `None` and `field_docs` empty for a plain [`OpDef::new`] with no help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpDoc {
    /// The registered kind name.
    pub kind: String,
    /// One-line summary, if [`OpDef::with_help`] set one.
    pub description: Option<String>,
    /// Per-field `(name, doc)` pairs, if [`OpDef::with_help`] set any.
    pub field_docs: Vec<(String, String)>,
}

fn weight_one<C, W>(_ctx: &C, _world: &W) -> u32 {
    1
}

impl<C: 'static, W: 'static> OpDef<C, W> {
    /// A new kind descriptor with the default weight of `1` (a uniform mix). `decode` builds
    /// this kind from a config scenario step or replay artifact; for a `Deserialize`-deriving
    /// op struct pass [`decode_json_op::<T, _, _>`](decode_json_op).
    pub fn new(name: &'static str, generate: GenerateFn<C, W>, decode: DecodeFn<C, W>) -> Self {
        Self {
            name,
            generate,
            decode,
            weight: weight_one::<C, W>,
            description: None,
            field_docs: Vec::new(),
        }
    }

    /// Override the dynamic weight (default `1`).
    pub fn with_weight(mut self, weight: WeightFn<C, W>) -> Self {
        self.weight = weight;
        self
    }

    /// Attach opt-in documentation surfaced by the CLI's `describe` subcommand: a one-line
    /// `description` and per-field `(name, doc)` pairs. Purely descriptive, unset by default, so
    /// a plain [`OpDef::new`] needs no change.
    pub fn with_help(mut self, description: &str, field_docs: &[(&str, &str)]) -> Self {
        self.description = Some(description.to_string());
        self.field_docs = field_docs
            .iter()
            .map(|(f, d)| (f.to_string(), d.to_string()))
            .collect();
        self
    }

    /// This kind's `describe` documentation: its name plus any [`with_help`](Self::with_help).
    pub fn doc(&self) -> OpDoc {
        OpDoc {
            kind: self.name.to_string(),
            description: self.description.clone(),
            field_docs: self.field_docs.clone(),
        }
    }

    /// The kind name: the `OpKind` value of [`OpSetHarness`] runs, the key config weights
    /// address, and the registry key.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Build one random op of this kind (calls the stored generator).
    pub fn generate(&self, rng: &mut Prng, world: &W) -> Box<dyn DynOp<C, W>> {
        let op = (self.generate)(rng, world);
        debug_assert_eq!(
            op.kind(),
            self.name,
            "generated op's kind() must equal its registered OpDef name"
        );
        op
    }

    /// Build one op of this kind from config data (calls the stored decoder), verifying the
    /// decoded op's `kind()` matches this def (a mismatch is a registration bug surfaced as a
    /// config error rather than a silent mislabel).
    pub fn decode(&self, data: serde_json::Value) -> Result<Box<dyn DynOp<C, W>>, String> {
        let op = (self.decode)(data)?;
        if op.kind() != self.name {
            return Err(format!(
                "decoded op reports kind `{}` but is registered as `{}`",
                op.kind(),
                self.name
            ));
        }
        Ok(op)
    }

    /// The kind's dynamic weight for the current state (calls the stored weight fn).
    pub fn weight(&self, ctx: &C, world: &W) -> u32 {
        (self.weight)(ctx, world)
    }
}

/// Advance hook for [`OpSetHarness::with_advance`]: progress time/blocks between endurance
/// operations (mirrors [`Harness::advance`](crate::Harness::advance)). Unset means no-op.
pub type AdvanceFn<C> = for<'a> fn(&'a mut C, u64) -> OpFuture<'a, Result<(), HarnessError>>;

/// A [`Harness`](crate::Harness) assembled from registered [`OpDef`]s instead of a
/// hand-written enum: `apply` dispatches to the op itself, `generate_op` and `weight` look
/// the kind up in the registry, so adding an op touches exactly one [`OpDef`].
///
/// Kinds live in a `BTreeMap`, so `op_kinds` yields sorted name order on every run: the same
/// seed draws the same op stream regardless of registration order.
///
/// Register at least one op before loading the harness into a runner. Using an empty registry is
/// a construction bug, so [`op_kinds`](Harness::op_kinds) panics (the runner calls it at the
/// start of every run), the same way [`register`](Self::register) panics on a duplicate kind.
pub struct OpSetHarness<C: 'static, W: 'static> {
    ops: BTreeMap<&'static str, OpDef<C, W>>,
    invariants: Vec<Box<dyn DynInvariant<C, W>>>,
    advance: Option<AdvanceFn<C>>,
}

impl<C: 'static, W: 'static> OpSetHarness<C, W> {
    /// An empty registry: no ops, no invariants, no advance hook.
    pub fn new() -> Self {
        Self {
            ops: BTreeMap::new(),
            invariants: Vec::new(),
            advance: None,
        }
    }

    /// Register one operation kind.
    ///
    /// # Panics
    ///
    /// Panics if a def with the same name is already registered: the registry replaces a
    /// compile-time exhaustive enum, so a name collision is a construction bug, not a
    /// runtime condition.
    pub fn register(mut self, def: OpDef<C, W>) -> Self {
        let name = def.name();
        if self.ops.insert(name, def).is_some() {
            panic!("OpSetHarness: duplicate op kind {name:?}");
        }
        self
    }

    /// Attach one invariant, checked by the runner per its `check_every` cadence.
    pub fn invariant(mut self, inv: Box<dyn DynInvariant<C, W>>) -> Self {
        self.invariants.push(inv);
        self
    }

    /// Set the endurance advance hook (progress time/blocks between operations). Without it,
    /// advance is a no-op, which is right for a pure-function `Ctx`.
    pub fn with_advance(mut self, advance: AdvanceFn<C>) -> Self {
        self.advance = Some(advance);
        self
    }
}

impl<C: 'static, W: 'static> Default for OpSetHarness<C, W> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: 'static, W: 'static> Harness for OpSetHarness<C, W> {
    type Ctx = C;
    type World = W;
    type Operation = DynOperation<C, W>;
    type Invariant = Box<dyn DynInvariant<C, W>>;
    type OpKind = &'static str;

    async fn apply(
        &self,
        ctx: &mut C,
        world: &mut W,
        op: &Self::Operation,
    ) -> Result<Verdict, HarnessError> {
        op.0.apply(ctx, world).await
    }

    // An empty registry is a construction bug, not a runtime condition (like a duplicate kind):
    // the runner calls `op_kinds` at the start of every run, so panicking here throws eagerly
    // with a clear message instead of degrading to the runner's opaque empty-mix Infra failure.
    fn op_kinds(&self) -> Vec<&'static str> {
        if self.ops.is_empty() {
            panic!("OpSetHarness: no op kinds registered; register at least one OpDef before use");
        }
        self.ops.keys().copied().collect()
    }

    fn generate_op(&self, rng: &mut Prng, world: &W, kind: &'static str) -> Self::Operation {
        let def = self
            .ops
            .get(kind)
            .unwrap_or_else(|| panic!("OpSetHarness: unknown op kind {kind:?}"));
        DynOperation(def.generate(rng, world))
    }

    // An unknown kind weighs 0 (excluded) rather than panicking: a typo in a restricted
    // run's kind list then surfaces as the runner's every-weight-zero Infra failure.
    fn weight(&self, ctx: &C, world: &W, kind: &'static str) -> u32 {
        self.ops.get(kind).map_or(0, |def| def.weight(ctx, world))
    }

    fn invariants(&self) -> Vec<Self::Invariant> {
        self.invariants.clone()
    }

    async fn check(&self, ctx: &mut C, world: &W, inv: &Self::Invariant) -> CheckOutcome {
        inv.check(ctx, world).await
    }

    async fn advance(&self, ctx: &mut C, blocks: u64) -> Result<(), HarnessError> {
        match self.advance {
            Some(advance) => advance(ctx, blocks).await,
            None => Ok(()),
        }
    }
}

/// The config/CLI codec seam: maps registered kind names and externally tagged op payloads
/// between config documents and a harness's own types. `harness-cli`'s registry requires
/// this instead of serde bounds on `Operation`/`OpKind`. [`OpSetHarness`] implements it;
/// developers never implement it by hand.
pub trait ConfigOps: Harness {
    /// Resolve a config kind name (`kinds = ["add"]`, `weights = { add = 3 }`) to this
    /// harness's `OpKind`. Errors list the available names.
    fn parse_kind(&self, name: &str) -> Result<Self::OpKind, String>;

    /// Decode one externally tagged op value: a bare kind-name string (`op = "ping"`) or a
    /// single-key table (`op = { add = { n = 5 } }`).
    fn decode_op(&self, value: &serde_json::Value) -> Result<Self::Operation, String>;

    /// Encode one op back to the same externally tagged shape, for reports and replay
    /// artifacts. Always the single-key object form, `{"add": {"n": 5}}`.
    fn encode_op(&self, op: &Self::Operation) -> serde_json::Value;

    /// Every registered op kind name, sorted. Powers the CLI's `describe` subcommand without a
    /// live `(ctx, world)`.
    fn kind_names(&self) -> Vec<String>;

    /// Every registered op kind's `describe` documentation, sorted by kind name.
    fn op_docs(&self) -> Vec<OpDoc>;
}

impl<C: 'static, W: 'static> OpSetHarness<C, W> {
    fn available_kinds(&self) -> String {
        self.kind_names().join(", ")
    }
}

impl<C: 'static, W: 'static> ConfigOps for OpSetHarness<C, W> {
    fn parse_kind(&self, name: &str) -> Result<&'static str, String> {
        self.ops
            .get_key_value(name)
            .map(|(k, _)| *k)
            .ok_or_else(|| {
                format!(
                    "unknown op kind `{name}`; available: {}",
                    self.available_kinds()
                )
            })
    }

    fn decode_op(&self, value: &serde_json::Value) -> Result<DynOperation<C, W>, String> {
        let (name, data) = match value {
            serde_json::Value::String(name) => (
                name.as_str(),
                serde_json::Value::Object(serde_json::Map::new()),
            ),
            serde_json::Value::Object(map) if map.len() == 1 => {
                let (name, data) = map.iter().next().expect("len == 1 checked above");
                (name.as_str(), data.clone())
            }
            _ => {
                return Err(
                    "an op must be a bare kind-name string (`op = \"ping\"`) or a \
                     single-key table (`op = { add = { n = 5 } }`)"
                        .to_string(),
                )
            }
        };
        let def = self.ops.get(name).ok_or_else(|| {
            format!(
                "unknown op kind `{name}`; available: {}",
                self.available_kinds()
            )
        })?;
        let op = def
            .decode(data)
            .map_err(|e| format!("op kind `{name}`: {e}"))?;
        Ok(DynOperation(op))
    }

    fn encode_op(&self, op: &DynOperation<C, W>) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert(op.0.kind().to_string(), op.0.to_data());
        serde_json::Value::Object(map)
    }

    fn kind_names(&self) -> Vec<String> {
        self.ops.keys().map(|k| k.to_string()).collect()
    }

    fn op_docs(&self) -> Vec<OpDoc> {
        self.ops.values().map(OpDef::doc).collect()
    }
}
