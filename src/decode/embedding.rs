//! Decoding a `kind: embedding` response.
//!
//! What matters here is not the vectors — it is their *shape*. Does the endpoint
//! answer, how many vectors did it return, how wide are they, are the values
//! actually numbers, and does the same input twice give the same answer. A
//! replica quietly serving a different model than its siblings shows up as a
//! determinism failure and nothing else.
//!
//! # One item per input, one or more vectors per item
//!
//! A pooled endpoint answers one vector per input. A multi-vector one — late
//! interaction, or a server with `pooling: none` — answers one vector per
//! *token*, so an input comes back as a list of vectors. Both decode into the
//! same shape: `count` is the number of items and stays comparable to the number
//! of inputs sent, and `vectors_per_item` says how many vectors each item holds.
//!
//! # Vectors are never rendered whole
//!
//! [`Embedding`] serialises per-vector *summaries*: width, norm, a short sample,
//! a distribution histogram. Nobody wants 1024 floats in a log line, a response
//! body or a web page. The full vectors are only ever attached when the caller
//! explicitly asks for them.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::paths::{self, resolve};
use super::{DecodeField, DecodeTrace, Usage};
use crate::profile::DecodeSpec;

/// How many leading values are shown as a preview.
const SAMPLE_LEN: usize = 8;

/// Buckets in the per-vector distribution histogram.
const HISTOGRAM_BUCKETS: usize = 24;

/// The same count, pre-converted, so bucketing needs no `usize as f32`.
const HISTOGRAM_BUCKETS_F: f32 = 24.0;

/// How many of one item's vectors are summarised.
///
/// A multi-vector endpoint answers one vector per token, and five hundred
/// histograms help nobody. The count itself stays exact in
/// [`Embedding::vectors_per_item`], and the checks still read every vector —
/// only the preview stops here.
const MAX_SUMMARIES_PER_ITEM: usize = 8;

/// How the endpoint encoded its vectors. Worth surfacing: a backend that
/// silently switches to base64 is a change you want to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum VectorEncoding {
    /// JSON arrays of numbers.
    Float,
    /// Base64 of little-endian `f32`, as `encoding_format: base64` produces.
    Base64,
    /// Nothing was decoded.
    None,
}

/// Vector width, derived rather than read from the response.
///
/// [`Dimensions::Ragged`] exists because an endpoint returning inconsistent
/// widths is exactly the bug worth catching, and a single `usize` would hide it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Dimensions {
    /// Every vector has the same width.
    Uniform {
        /// The common width.
        value: usize,
    },
    /// Widths differ. The value per vector, in order.
    Ragged {
        /// One width per vector.
        values: Vec<usize>,
    },
    /// No vector was decoded.
    Unknown,
}

impl Dimensions {
    /// The common width, when there is one.
    #[must_use]
    pub fn uniform(&self) -> Option<usize> {
        match self {
            Self::Uniform { value } => Some(*value),
            _ => None,
        }
    }
}

/// Value distribution across one vector.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Histogram {
    /// Smallest value in the vector.
    pub min: f32,
    /// Largest value in the vector.
    pub max: f32,
    /// Counts per equal-width bucket between `min` and `max`.
    pub buckets: Vec<u32>,
}

/// Everything about one vector that is safe to render.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct VectorSummary {
    /// Position among all the vectors that came back, whatever their item.
    pub index: usize,
    /// The item this vector belongs to — the input it answers.
    pub item: usize,
    /// Position within that item. Always `0` for a pooled endpoint.
    pub position: usize,
    /// Width.
    pub dimensions: usize,
    /// L2 norm. Zero means the endpoint answered with nothing useful.
    pub norm: f64,
    /// The leading values, as a preview. Never the whole vector.
    pub sample: Vec<f32>,
    /// `true` when every value is a finite number.
    pub finite: bool,
    /// Value distribution.
    pub histogram: Histogram,
}

/// Normalised embedding response.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Embedding {
    /// Number of items. Derived — this is what you compare to the number of
    /// inputs you sent, and it stays one per input however many vectors a
    /// multi-vector endpoint packs into each.
    pub count: usize,
    /// Total number of vectors, across every item. Equal to `count` unless the
    /// endpoint answered more than one vector per input.
    pub vector_count: usize,
    /// How many vectors each item holds, in order. All ones for a pooled
    /// endpoint; anything else is the multi-vector shape — and it is also what
    /// says how many vectors the capped summaries below stand for.
    pub vectors_per_item: Vec<usize>,
    /// Vector width. Derived.
    pub dimensions: Dimensions,
    /// How the vectors arrived on the wire.
    pub encoding: VectorEncoding,
    /// Token accounting.
    pub usage: Option<Usage>,
    /// One summary per vector, at most [`MAX_SUMMARIES_PER_ITEM`] per item.
    pub vectors: Vec<VectorSummary>,
    /// The full vectors, flattened in wire order, present only when the caller
    /// explicitly asked. `vectors_per_item` says how to group them back.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full: Option<Vec<Vec<f32>>>,
}

/// Result of one shape check.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum CheckOutcome {
    /// The check ran and held.
    Pass,
    /// The check ran and did not hold.
    Fail {
        /// What was expected, and what was found.
        detail: String,
    },
    /// The check could not run — nothing to compare against.
    Skipped {
        /// Why, and what to set to enable it.
        reason: String,
    },
}

impl CheckOutcome {
    /// `Pass` when `held`, otherwise `Fail` with `detail`.
    #[must_use]
    pub fn from(held: bool, detail: impl FnOnce() -> String) -> Self {
        if held {
            Self::Pass
        } else {
            Self::Fail { detail: detail() }
        }
    }

    /// A skipped check.
    #[must_use]
    pub fn skipped(reason: impl Into<String>) -> Self {
        Self::Skipped {
            reason: reason.into(),
        }
    }

    /// `true` when the check ran and did not hold.
    #[must_use]
    pub fn failed(&self) -> bool {
        matches!(self, Self::Fail { .. })
    }
}

/// The checks that make `kind: embedding` worth having as its own kind.
///
/// All derived from the response and what was asked for — there is no assertion
/// engine here, and none is needed for any of these.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingChecks {
    /// One vector per input sent.
    pub count: CheckOutcome,
    /// Width matches the profile's `expect.dimensions`.
    pub dimensions: CheckOutcome,
    /// Every value is a finite number — no `NaN`, no `null`.
    pub finite: CheckOutcome,
    /// No vector is all zeros.
    pub non_zero_norm: CheckOutcome,
    /// The same input twice gives the same vectors. Needs `repeat: 2`.
    pub determinism: CheckOutcome,
}

impl EmbeddingChecks {
    /// Evaluates everything derivable from a single response.
    ///
    /// The vectors are read here rather than the summaries: those are capped per
    /// item, and a check that only looked at the preview would call a response
    /// finite because its first eight vectors were.
    ///
    /// Determinism starts [`CheckOutcome::Skipped`]; the caller fills it in if it
    /// sent the request more than once.
    #[must_use]
    pub fn evaluate(
        embedding: &Embedding,
        vectors: &Vectors,
        inputs: usize,
        expected_dimensions: Option<usize>,
    ) -> Self {
        let count = if inputs == 0 {
            CheckOutcome::skipped("no input was sent, so there is nothing to count against")
        } else {
            CheckOutcome::from(embedding.count == inputs, || {
                format!(
                    "sent {inputs} input(s), got {} item(s) back",
                    embedding.count
                )
            })
        };

        let dimensions = match (expected_dimensions, &embedding.dimensions) {
            (None, _) => {
                CheckOutcome::skipped("set `expect.dimensions` in the profile to check this")
            }
            (Some(expected), Dimensions::Uniform { value }) => {
                CheckOutcome::from(*value == expected, || {
                    format!("expected {expected} dimensions, got {value}")
                })
            }
            (Some(expected), Dimensions::Ragged { values }) => CheckOutcome::Fail {
                detail: format!(
                    "expected {expected} dimensions, got inconsistent widths {values:?}"
                ),
            },
            (Some(_), Dimensions::Unknown) => CheckOutcome::Fail {
                detail: "no vector was decoded".to_owned(),
            },
        };

        let holes: Vec<String> = vectors
            .enumerated()
            .filter(|(.., vector)| !vector.iter().all(|value| value.is_finite()))
            .map(|(item, position, _)| vectors.label(item, position))
            .collect();
        let finite = CheckOutcome::from(holes.is_empty(), || {
            format!(
                "vector(s) [{}] contain a value that is not a finite number",
                holes.join(", ")
            )
        });

        let zeroed: Vec<String> = vectors
            .enumerated()
            .filter(|(.., vector)| norm(vector) == 0.0)
            .map(|(item, position, _)| vectors.label(item, position))
            .collect();
        let non_zero_norm = CheckOutcome::from(zeroed.is_empty(), || {
            format!("vector(s) [{}] have a zero norm", zeroed.join(", "))
        });

        Self {
            count,
            dimensions,
            finite,
            non_zero_norm,
            determinism: CheckOutcome::skipped("send `repeat: 2` to check this"),
        }
    }

    /// `true` when at least one check ran and did not hold.
    #[must_use]
    pub fn any_failed(&self) -> bool {
        [
            &self.count,
            &self.dimensions,
            &self.finite,
            &self.non_zero_norm,
            &self.determinism,
        ]
        .iter()
        .any(|outcome| outcome.failed())
    }
}

/// Numeric arrays longer than this are elided from the raw response.
const MAX_INLINE_VALUES: usize = 16;

/// Strings longer than this are elided from the raw response — that is what a
/// base64-encoded vector looks like.
const MAX_INLINE_CHARS: usize = 256;

/// Replaces bulk vector payloads in a raw response with a short placeholder.
///
/// Without this the "never render a whole vector" rule would be decorative: the
/// summaries are careful, and then `raw` hands over all 1024 floats anyway. What
/// survives is everything you actually want to read in the raw tree — the model
/// name, `usage`, the per-item `index` and `object` fields — with the payload
/// itself replaced by its size.
#[must_use]
pub fn elide(value: &Value) -> Value {
    match value {
        Value::Array(items)
            if items.len() > MAX_INLINE_VALUES && items.iter().all(Value::is_number) =>
        {
            Value::String(format!(
                "<{} values elided; set includeVectors to see them>",
                items.len()
            ))
        }
        Value::Array(items) => Value::Array(items.iter().map(elide).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, nested)| (key.clone(), elide(nested)))
                .collect(),
        ),
        Value::String(text) if text.len() > MAX_INLINE_CHARS => Value::String(format!(
            "<{} characters elided; set includeVectors to see them>",
            text.len()
        )),
        other => other.clone(),
    }
}

/// The vectors themselves, kept out of [`Embedding`] so they cannot be
/// serialised by accident.
///
/// Grouped by item, one item per input: a pooled endpoint puts a single vector
/// in each, a multi-vector one puts one per token. Keeping the grouping is what
/// lets `count` stay comparable to the number of inputs sent instead of
/// collapsing into a pile of vectors nobody can attribute.
#[derive(Debug, Clone, Default)]
pub struct Vectors(Vec<Vec<Vec<f32>>>);

impl Vectors {
    /// Wraps one vector per item — a pooled response decoded some other way, by
    /// a script typically.
    #[must_use]
    pub fn new(vectors: Vec<Vec<f32>>) -> Self {
        Self(vectors.into_iter().map(|vector| vec![vector]).collect())
    }

    /// Wraps vectors already grouped by item.
    #[must_use]
    pub fn grouped(items: Vec<Vec<Vec<f32>>>) -> Self {
        Self(items)
    }

    /// The items, each holding its own vectors.
    #[must_use]
    pub fn items(&self) -> &[Vec<Vec<f32>>] {
        &self.0
    }

    /// Every vector in wire order, as `(item, position, vector)`.
    pub fn enumerated(&self) -> impl Iterator<Item = (usize, usize, &Vec<f32>)> {
        self.0.iter().enumerate().flat_map(|(item, vectors)| {
            vectors
                .iter()
                .enumerate()
                .map(move |(position, vector)| (item, position, vector))
        })
    }

    /// Every vector in wire order, with the grouping flattened away.
    pub fn flat(&self) -> impl Iterator<Item = &Vec<f32>> {
        self.0.iter().flatten()
    }

    /// `true` when any item holds anything other than exactly one vector.
    #[must_use]
    pub fn multi(&self) -> bool {
        self.0.iter().any(|item| item.len() != 1)
    }

    /// How a check names one vector: its item alone when there is one vector per
    /// item, `item#position` when there are several and the item no longer
    /// identifies it.
    #[must_use]
    fn label(&self, item: usize, position: usize) -> String {
        if self.multi() {
            format!("{item}#{position}")
        } else {
            item.to_string()
        }
    }

    /// Largest absolute difference against another set of vectors.
    ///
    /// `None` when the two do not have the same shape — a different number of
    /// items, of vectors within an item, or of dimensions — which is itself a
    /// determinism failure.
    #[must_use]
    pub fn max_deviation(&self, other: &Self) -> Option<f32> {
        if self.0.len() != other.0.len() {
            return None;
        }
        let mut worst = 0.0_f32;
        for (left, right) in self.0.iter().zip(&other.0) {
            if left.len() != right.len() {
                return None;
            }
            for (left, right) in left.iter().zip(right) {
                if left.len() != right.len() {
                    return None;
                }
                for (a, b) in left.iter().zip(right) {
                    worst = worst.max((a - b).abs());
                }
            }
        }
        Some(worst)
    }
}

/// Decodes `raw` according to `spec`.
///
/// `inputs` is how many texts were sent. It is not read as a truth — the checks
/// exist precisely to catch a response that disagrees with it — but it is what
/// tells one list of vectors apart from another: see [`split_batch`].
///
/// Never fails, for the same reason chat decoding does not: an unfamiliar shape
/// is something to look at, not something to hide behind an error.
#[must_use]
pub fn decode(
    raw: &Value,
    spec: &DecodeSpec,
    inputs: usize,
    include_vectors: bool,
) -> (Embedding, Vectors, DecodeTrace) {
    let mut trace = DecodeTrace::default();

    let (vectors, encoding) = decode_vectors(raw, spec, inputs, &mut trace);
    let usage = decode_usage(raw, spec, &mut trace);

    let embedding = summarise_all(&vectors, encoding, usage, include_vectors);
    (embedding, vectors, trace)
}

/// Derives the shape of an already-decoded set of vectors.
///
/// Shared with the script path: however the vectors were obtained, `count`,
/// `dimensions` and the per-vector summaries are computed here and nowhere else.
#[must_use]
pub fn summarise_all(
    vectors: &Vectors,
    encoding: VectorEncoding,
    usage: Option<Usage>,
    include_vectors: bool,
) -> Embedding {
    let widths: Vec<usize> = vectors.flat().map(Vec::len).collect();

    Embedding {
        count: vectors.items().len(),
        vector_count: widths.len(),
        vectors_per_item: vectors.items().iter().map(Vec::len).collect(),
        dimensions: dimensions(&widths),
        encoding,
        usage,
        full: include_vectors.then(|| vectors.flat().cloned().collect()),
        vectors: vectors
            .enumerated()
            .enumerate()
            .filter(|(_, (_, position, _))| *position < MAX_SUMMARIES_PER_ITEM)
            .map(|(index, (item, position, vector))| summarise(index, item, position, vector))
            .collect(),
    }
}

/// Resolves the vector cascade and groups whatever shape it selected into items.
///
/// Four shapes are covered without a script: one node per item
/// (`$.data[*].embedding`), one node holding the whole batch (`$.embeddings`), a
/// bare vector at the root, and any of those with an item that is a *list* of
/// vectors rather than one — the multi-vector case.
fn decode_vectors(
    raw: &Value,
    spec: &DecodeSpec,
    inputs: usize,
    trace: &mut DecodeTrace,
) -> (Vectors, VectorEncoding) {
    let Some((path, nodes)) = resolve(raw, &spec.vectors) else {
        trace.miss(DecodeField::Vectors, paths::sources(&spec.vectors));
        return (Vectors::default(), VectorEncoding::None);
    };

    // Several nodes are already one per item. A single one may be the whole
    // batch, and only then is there anything to split.
    let selected: Vec<&Value> = match nodes.as_slice() {
        [node] => split_batch(node, inputs),
        other => other.to_vec(),
    };

    let mut items = Vec::with_capacity(selected.len());
    let mut encoding = VectorEncoding::None;

    for node in selected {
        let mut vectors = Vec::new();
        match node {
            Value::Array(values) if holds_vectors(node) => {
                for value in values {
                    push_vector(value, &mut vectors, &mut encoding, path.source(), trace);
                }
            }
            _ => push_vector(node, &mut vectors, &mut encoding, path.source(), trace),
        }
        if !vectors.is_empty() {
            items.push(vectors);
        }
    }

    if items.is_empty() {
        trace.miss(DecodeField::Vectors, paths::sources(&spec.vectors));
    } else {
        trace.hit(DecodeField::Vectors, path.source());
    }

    (Vectors::grouped(items), encoding)
}

/// Splits the single node that holds everything into one node per item.
///
/// A list of *lists of vectors* is unambiguous: one entry per item, several
/// vectors in each. A list of plain vectors is not — it is a batch of pooled
/// vectors under `$.embeddings`, and byte for byte the same JSON is one input's
/// token vectors from a multi-vector endpoint. The number of inputs sent settles
/// it; when it settles nothing, the batch reading wins and the `count` check is
/// what reports the disagreement.
fn split_batch(node: &Value, inputs: usize) -> Vec<&Value> {
    let Value::Array(entries) = node else {
        return vec![node];
    };

    match entries.first() {
        // One entry per item, each holding its own vectors: nothing to guess.
        Some(first) if holds_vectors(first) => entries.iter().collect(),
        // A flat list of vectors: a batch, unless a single input was sent, in
        // which case they are all that input's.
        Some(Value::Array(_) | Value::String(_)) if inputs != 1 => entries.iter().collect(),
        _ => vec![node],
    }
}

/// `true` when the node is a *list* of vectors rather than a vector: its first
/// element is itself an array or a base64 string.
fn holds_vectors(node: &Value) -> bool {
    matches!(node, Value::Array(entries) if matches!(entries.first(), Some(Value::Array(_) | Value::String(_))))
}

/// Decodes one vector — floats or base64 — onto an item, or traces why it could
/// not.
fn push_vector(
    node: &Value,
    vectors: &mut Vec<Vec<f32>>,
    encoding: &mut VectorEncoding,
    source: &str,
    trace: &mut DecodeTrace,
) {
    match node {
        Value::Array(values) => {
            *encoding = VectorEncoding::Float;
            vectors.push(values.iter().map(number_to_f32).collect());
        }
        Value::String(encoded) => match decode_base64_f32(encoded) {
            Ok(vector) => {
                *encoding = VectorEncoding::Base64;
                vectors.push(vector);
            }
            Err(message) => trace.issue(DecodeField::Vectors, source, message),
        },
        other => trace.issue(
            DecodeField::Vectors,
            source,
            format!(
                "expected a vector or a base64 string, found {}",
                kind_of(other)
            ),
        ),
    }
}

/// A non-numeric entry becomes `NaN` rather than being dropped, so the vector
/// keeps its width and the finiteness check is what reports the problem.
fn number_to_f32(value: &Value) -> f32 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "embeddings are f32 on the wire; the tolerance used for determinism is well above f32 epsilon"
    )]
    value.as_f64().map_or(f32::NAN, |number| number as f32)
}

/// Decodes the `encoding_format: base64` payload: little-endian `f32`.
fn decode_base64_f32(encoded: &str) -> Result<Vec<f32>, String> {
    let bytes = BASE64
        .decode(encoded.trim())
        .map_err(|error| format!("the vector is not valid base64: {error}"))?;

    if bytes.len() % 4 != 0 {
        return Err(format!(
            "base64 payload of {} bytes is not a whole number of f32 values",
            bytes.len()
        ));
    }

    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn dimensions(widths: &[usize]) -> Dimensions {
    let Some((first, rest)) = widths.split_first() else {
        return Dimensions::Unknown;
    };
    if rest.iter().all(|width| width == first) {
        Dimensions::Uniform { value: *first }
    } else {
        Dimensions::Ragged {
            values: widths.to_vec(),
        }
    }
}

/// L2 norm over the finite values only: one hole should not turn the norm into
/// `NaN` and hide the fact that the rest of the vector is perfectly ordinary.
fn norm(vector: &[f32]) -> f64 {
    vector
        .iter()
        .filter(|value| value.is_finite())
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt()
}

fn summarise(index: usize, item: usize, position: usize, vector: &[f32]) -> VectorSummary {
    VectorSummary {
        index,
        item,
        position,
        dimensions: vector.len(),
        norm: norm(vector),
        sample: vector.iter().take(SAMPLE_LEN).copied().collect(),
        finite: vector.iter().all(|value| value.is_finite()),
        histogram: histogram(vector),
    }
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the scaled fraction is in [0, HISTOGRAM_BUCKETS] by construction, and clamped right after"
)]
fn histogram(vector: &[f32]) -> Histogram {
    let finite: Vec<f32> = vector
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect();

    if finite.is_empty() {
        return Histogram {
            min: 0.0,
            max: 0.0,
            buckets: vec![0; HISTOGRAM_BUCKETS],
        };
    }

    let (min, max) = finite.iter().fold((f32::MAX, f32::MIN), |(lo, hi), value| {
        (lo.min(*value), hi.max(*value))
    });

    let mut buckets = vec![0_u32; HISTOGRAM_BUCKETS];
    let span = max - min;
    for value in finite {
        // A constant vector collapses into one bucket rather than dividing by zero.
        let position = if span > 0.0 {
            ((value - min) / span * HISTOGRAM_BUCKETS_F) as usize
        } else {
            0
        };
        buckets[position.min(HISTOGRAM_BUCKETS - 1)] += 1;
    }

    Histogram { min, max, buckets }
}

fn decode_usage(raw: &Value, spec: &DecodeSpec, trace: &mut DecodeTrace) -> Option<Usage> {
    let (path, node) = super::paths::resolve_one(raw, &spec.usage).or_else(|| {
        trace.miss(DecodeField::Usage, paths::sources(&spec.usage));
        None
    })?;
    trace.hit(DecodeField::Usage, path.source());
    Some(Usage::from_value(node))
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> DecodeSpec {
        serde_yaml_ng::from_str(
            r#"
vectors: ["$.data[*].embedding", "$.embeddings", "$"]
usage: ["$.usage"]
"#,
        )
        .unwrap()
    }

    fn base64_of(values: &[f32]) -> String {
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        BASE64.encode(bytes)
    }

    #[test]
    fn decodes_the_openai_shape() {
        let raw = serde_json::json!({
            "data": [
                {"embedding": [1.0, 0.0, 0.0]},
                {"embedding": [0.0, 2.0, 0.0]}
            ],
            "usage": {"prompt_tokens": 7}
        });

        let (embedding, _, trace) = decode(&raw, &spec(), 2, false);
        assert_eq!(embedding.count, 2);
        assert_eq!(embedding.dimensions, Dimensions::Uniform { value: 3 });
        assert_eq!(embedding.encoding, VectorEncoding::Float);
        assert!((embedding.vectors[1].norm - 2.0).abs() < 1e-9);
        assert_eq!(trace.matched[&DecodeField::Vectors], "$.data[*].embedding");
        assert_eq!(embedding.usage.unwrap().prompt_tokens, Some(7));
    }

    #[test]
    fn decodes_a_single_node_holding_a_list_of_vectors() {
        let raw = serde_json::json!({"embeddings": [[1.0, 0.0], [0.0, 1.0]]});
        let (embedding, _, trace) = decode(&raw, &spec(), 2, false);

        assert_eq!(embedding.count, 2);
        assert_eq!(embedding.dimensions, Dimensions::Uniform { value: 2 });
        assert_eq!(trace.matched[&DecodeField::Vectors], "$.embeddings");
    }

    #[test]
    fn decodes_a_bare_vector_at_the_root() {
        let raw = serde_json::json!([0.3, 0.4]);
        let (embedding, _, _) = decode(&raw, &spec(), 1, false);

        assert_eq!(embedding.count, 1);
        assert_eq!(embedding.dimensions, Dimensions::Uniform { value: 2 });
        assert!((embedding.vectors[0].norm - 0.5).abs() < 1e-6);
    }

    #[test]
    fn decodes_base64_vectors() {
        let raw = serde_json::json!({
            "data": [
                {"embedding": base64_of(&[1.0, 0.0, 0.0])},
                {"embedding": base64_of(&[0.0, 0.0, 3.0])}
            ]
        });

        let (embedding, vectors, _) = decode(&raw, &spec(), 2, true);
        assert_eq!(embedding.encoding, VectorEncoding::Base64);
        assert_eq!(embedding.dimensions, Dimensions::Uniform { value: 3 });
        assert!((embedding.vectors[1].norm - 3.0).abs() < 1e-6);
        assert_eq!(vectors.items()[0][0], vec![1.0, 0.0, 0.0]);
    }

    #[test]
    fn a_truncated_base64_payload_is_reported() {
        // Five bytes: not a whole number of f32 values.
        let raw = serde_json::json!({"embeddings": [BASE64.encode([1_u8, 2, 3, 4, 5])]});
        let (embedding, _, trace) = decode(&raw, &spec(), 1, false);

        assert_eq!(embedding.count, 0);
        assert!(trace.issues[0].message.contains("whole number of f32"));
    }

    #[test]
    fn inconsistent_widths_are_surfaced_not_averaged_away() {
        let raw = serde_json::json!({"embeddings": [[1.0, 2.0, 3.0], [1.0, 2.0]]});
        let (embedding, _, _) = decode(&raw, &spec(), 2, false);

        assert_eq!(
            embedding.dimensions,
            Dimensions::Ragged { values: vec![3, 2] }
        );
        assert!(embedding.dimensions.uniform().is_none());
    }

    #[test]
    fn a_null_inside_a_vector_keeps_its_width_and_fails_finiteness() {
        let raw = serde_json::json!({"embeddings": [[1.0, null, 3.0]]});
        let (embedding, _, _) = decode(&raw, &spec(), 1, false);

        assert_eq!(embedding.dimensions, Dimensions::Uniform { value: 3 });
        assert!(!embedding.vectors[0].finite);
        // The norm ignores the hole rather than becoming NaN itself.
        assert!(embedding.vectors[0].norm.is_finite());
    }

    #[test]
    fn a_zero_vector_has_a_zero_norm() {
        let raw = serde_json::json!({"embeddings": [[0.0, 0.0, 0.0]]});
        let (embedding, _, _) = decode(&raw, &spec(), 1, false);
        assert!((embedding.vectors[0].norm - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn full_vectors_are_absent_unless_asked_for() {
        let raw = serde_json::json!({"embeddings": [[1.0, 2.0]]});

        let (without, _, _) = decode(&raw, &spec(), 1, false);
        assert!(without.full.is_none());
        let rendered = serde_json::to_string(&without).unwrap();
        assert!(!rendered.contains("full"), "{rendered}");

        let (with, _, _) = decode(&raw, &spec(), 1, true);
        assert_eq!(with.full.unwrap(), vec![vec![1.0, 2.0]]);
    }

    #[test]
    fn a_long_vector_is_only_ever_previewed() {
        let values: Vec<f32> = (0..1024_u16).map(|i| f32::from(i) / 1024.0).collect();
        let raw = serde_json::json!({"embeddings": [values]});

        let (embedding, _, _) = decode(&raw, &spec(), 1, false);
        assert_eq!(embedding.vectors[0].dimensions, 1024);
        assert_eq!(embedding.vectors[0].sample.len(), SAMPLE_LEN);
        assert_eq!(
            embedding.vectors[0].histogram.buckets.len(),
            HISTOGRAM_BUCKETS
        );
        assert_eq!(
            embedding.vectors[0].histogram.buckets.iter().sum::<u32>(),
            1024
        );
    }

    #[test]
    fn an_item_holding_several_vectors_is_still_one_item() {
        // What a multi-vector endpoint answers: one vector per token, grouped
        // per input.
        let raw = serde_json::json!({
            "data": [
                {"embedding": [[1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]},
                {"embedding": [[0.0, 2.0], [2.0, 0.0]]}
            ]
        });

        let (embedding, vectors, trace) = decode(&raw, &spec(), 2, false);
        assert_eq!(embedding.count, 2);
        assert_eq!(embedding.vector_count, 5);
        assert_eq!(embedding.vectors_per_item, vec![3, 2]);
        assert_eq!(embedding.dimensions, Dimensions::Uniform { value: 2 });
        assert!(vectors.multi());
        assert_eq!(trace.matched[&DecodeField::Vectors], "$.data[*].embedding");

        // Every vector knows which input it answers.
        assert_eq!(
            (embedding.vectors[3].item, embedding.vectors[3].position),
            (1, 0)
        );
    }

    #[test]
    fn the_count_check_counts_items_not_vectors() {
        let raw = serde_json::json!({
            "data": [
                {"embedding": [[1.0, 0.0], [0.0, 1.0]]},
                {"embedding": [[1.0, 1.0]]}
            ]
        });

        let (embedding, vectors, _) = decode(&raw, &spec(), 2, false);
        let checks = EmbeddingChecks::evaluate(&embedding, &vectors, 2, Some(2));
        assert!(
            matches!(checks.count, CheckOutcome::Pass),
            "{:?}",
            checks.count
        );
        assert!(matches!(checks.dimensions, CheckOutcome::Pass));
    }

    #[test]
    fn a_single_input_owns_the_whole_list_of_vectors() {
        // The same JSON either way: three pooled vectors for three inputs, or one
        // input's three token vectors. What was sent settles it.
        let raw = serde_json::json!({"embeddings": [[1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]});

        let (one, _, _) = decode(&raw, &spec(), 1, false);
        assert_eq!(one.count, 1);
        assert_eq!(one.vectors_per_item, vec![3]);

        let (three, _, _) = decode(&raw, &spec(), 3, false);
        assert_eq!(three.count, 3);
        assert_eq!(three.vectors_per_item, vec![1, 1, 1]);
    }

    #[test]
    fn a_batch_of_multi_vector_items_needs_no_hint() {
        // Nesting one level deeper is unambiguous, whatever was sent.
        let raw = serde_json::json!({"embeddings": [[[1.0, 0.0], [0.0, 1.0]], [[1.0, 1.0]]]});

        let (embedding, _, _) = decode(&raw, &spec(), 1, false);
        assert_eq!(embedding.count, 2);
        assert_eq!(embedding.vectors_per_item, vec![2, 1]);
    }

    #[test]
    fn only_the_leading_vectors_of_an_item_are_summarised() {
        let token_vectors: Vec<Vec<f32>> = (0..20_u8).map(|i| vec![f32::from(i), 1.0]).collect();
        let raw = serde_json::json!({"data": [{"embedding": token_vectors}]});

        let (embedding, _, _) = decode(&raw, &spec(), 1, false);
        assert_eq!(embedding.vector_count, 20);
        assert_eq!(embedding.vectors_per_item, vec![20]);
        assert_eq!(embedding.vectors.len(), MAX_SUMMARIES_PER_ITEM);
    }

    #[test]
    fn a_check_reads_the_vectors_the_summaries_stopped_at() {
        let mut token_vectors: Vec<Value> =
            (0..12).map(|_| serde_json::json!([1.0, 1.0])).collect();
        token_vectors[10] = serde_json::json!([1.0, null]);
        let raw = serde_json::json!({"data": [{"embedding": token_vectors}]});

        let (embedding, vectors, _) = decode(&raw, &spec(), 1, false);
        let checks = EmbeddingChecks::evaluate(&embedding, &vectors, 1, None);

        // Past the summary cap, and named by item and position because the item
        // alone no longer identifies a vector.
        let CheckOutcome::Fail { detail } = &checks.finite else {
            panic!("expected a failure, got {:?}", checks.finite);
        };
        assert!(detail.contains("0#10"), "{detail}");
    }

    #[test]
    fn max_deviation_compares_shape_first() {
        let a = Vectors::new(vec![vec![1.0, 2.0]]);
        let b = Vectors::new(vec![vec![1.0, 2.000_01]]);
        let c = Vectors::new(vec![vec![1.0, 2.0, 3.0]]);

        assert!(a.max_deviation(&b).unwrap() > 0.0);
        assert!(a.max_deviation(&a).unwrap() < f32::EPSILON);
        assert!(a.max_deviation(&c).is_none());

        // Same vectors, different grouping: two items of one, or one item of two.
        let split = Vectors::grouped(vec![vec![vec![1.0, 2.0]], vec![vec![3.0, 4.0]]]);
        let together = Vectors::grouped(vec![vec![vec![1.0, 2.0], vec![3.0, 4.0]]]);
        assert!(split.max_deviation(&together).is_none());
    }
}
