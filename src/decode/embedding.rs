//! Decoding a `kind: embedding` response.
//!
//! What matters here is not the vectors — it is their *shape*. Does the endpoint
//! answer, how many vectors did it return, how wide are they, are the values
//! actually numbers, and does the same input twice give the same answer. A
//! replica quietly serving a different model than its siblings shows up as a
//! determinism failure and nothing else.
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
    /// Position in the response.
    pub index: usize,
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
    /// Number of vectors. Derived — this is what you compare to the number of
    /// inputs you sent.
    pub count: usize,
    /// Vector width. Derived.
    pub dimensions: Dimensions,
    /// How the vectors arrived on the wire.
    pub encoding: VectorEncoding,
    /// Token accounting.
    pub usage: Option<Usage>,
    /// One summary per vector.
    pub vectors: Vec<VectorSummary>,
    /// The full vectors, present only when the caller explicitly asked.
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
    /// Determinism starts [`CheckOutcome::Skipped`]; the caller fills it in if it
    /// sent the request more than once.
    #[must_use]
    pub fn evaluate(
        embedding: &Embedding,
        inputs: usize,
        expected_dimensions: Option<usize>,
    ) -> Self {
        let count = if inputs == 0 {
            CheckOutcome::skipped("no input was sent, so there is nothing to count against")
        } else {
            CheckOutcome::from(embedding.count == inputs, || {
                format!(
                    "sent {inputs} input(s), got {} vector(s) back",
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

        let ragged: Vec<usize> = embedding
            .vectors
            .iter()
            .filter(|vector| !vector.finite)
            .map(|vector| vector.index)
            .collect();
        let finite = CheckOutcome::from(ragged.is_empty(), || {
            format!("vector(s) {ragged:?} contain a value that is not a finite number")
        });

        let zeroed: Vec<usize> = embedding
            .vectors
            .iter()
            .filter(|vector| vector.norm == 0.0)
            .map(|vector| vector.index)
            .collect();
        let non_zero_norm = CheckOutcome::from(zeroed.is_empty(), || {
            format!("vector(s) {zeroed:?} have a zero norm")
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
#[derive(Debug, Clone, Default)]
pub struct Vectors(Vec<Vec<f32>>);

impl Vectors {
    /// Wraps vectors decoded some other way — by a script, typically.
    #[must_use]
    pub fn new(vectors: Vec<Vec<f32>>) -> Self {
        Self(vectors)
    }

    /// The decoded vectors.
    #[must_use]
    pub fn as_slice(&self) -> &[Vec<f32>] {
        &self.0
    }

    /// Largest absolute difference against another set of vectors.
    ///
    /// `None` when the two do not have the same shape, which is itself a
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
            for (a, b) in left.iter().zip(right) {
                worst = worst.max((a - b).abs());
            }
        }
        Some(worst)
    }
}

/// Decodes `raw` according to `spec`.
///
/// Never fails, for the same reason chat decoding does not: an unfamiliar shape
/// is something to look at, not something to hide behind an error.
#[must_use]
pub fn decode(
    raw: &Value,
    spec: &DecodeSpec,
    include_vectors: bool,
) -> (Embedding, Vectors, DecodeTrace) {
    let mut trace = DecodeTrace::default();

    let (vectors, encoding) = decode_vectors(raw, spec, &mut trace);
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
    Embedding {
        count: vectors.0.len(),
        dimensions: dimensions(&vectors.0),
        encoding,
        usage,
        full: include_vectors.then(|| vectors.0.clone()),
        vectors: vectors
            .0
            .iter()
            .enumerate()
            .map(|(index, vector)| summarise(index, vector))
            .collect(),
    }
}

/// Resolves the vector cascade and flattens whatever shape it selected.
///
/// Three shapes are covered without a script: one node per vector
/// (`$.data[*].embedding`), one node holding a list of vectors (`$.embeddings`),
/// and a bare vector at the root.
fn decode_vectors(
    raw: &Value,
    spec: &DecodeSpec,
    trace: &mut DecodeTrace,
) -> (Vectors, VectorEncoding) {
    let Some((path, nodes)) = resolve(raw, &spec.vectors) else {
        trace.miss(DecodeField::Vectors, paths::sources(&spec.vectors));
        return (Vectors::default(), VectorEncoding::None);
    };

    // One array node whose first element is itself a vector means the node is a
    // *list* of vectors, not a vector.
    let items: Vec<&Value> = match nodes.as_slice() {
        [Value::Array(array)]
            if matches!(array.first(), Some(Value::Array(_) | Value::String(_))) =>
        {
            array.iter().collect()
        }
        other => other.to_vec(),
    };

    let mut vectors = Vec::with_capacity(items.len());
    let mut encoding = VectorEncoding::None;

    for item in items {
        match item {
            Value::Array(values) => {
                encoding = VectorEncoding::Float;
                vectors.push(values.iter().map(number_to_f32).collect());
            }
            Value::String(encoded) => match decode_base64_f32(encoded) {
                Ok(vector) => {
                    encoding = VectorEncoding::Base64;
                    vectors.push(vector);
                }
                Err(message) => trace.issue(DecodeField::Vectors, path.source(), message),
            },
            other => trace.issue(
                DecodeField::Vectors,
                path.source(),
                format!(
                    "expected a vector or a base64 string, found {}",
                    kind_of(other)
                ),
            ),
        }
    }

    if vectors.is_empty() {
        trace.miss(DecodeField::Vectors, paths::sources(&spec.vectors));
    } else {
        trace.hit(DecodeField::Vectors, path.source());
    }

    (Vectors(vectors), encoding)
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

fn dimensions(vectors: &[Vec<f32>]) -> Dimensions {
    let mut widths = vectors.iter().map(Vec::len);
    let Some(first) = widths.next() else {
        return Dimensions::Unknown;
    };
    if widths.all(|width| width == first) {
        Dimensions::Uniform { value: first }
    } else {
        Dimensions::Ragged {
            values: vectors.iter().map(Vec::len).collect(),
        }
    }
}

fn summarise(index: usize, vector: &[f32]) -> VectorSummary {
    let finite = vector.iter().all(|value| value.is_finite());
    let norm = vector
        .iter()
        .filter(|value| value.is_finite())
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>()
        .sqrt();

    VectorSummary {
        index,
        dimensions: vector.len(),
        norm,
        sample: vector.iter().take(SAMPLE_LEN).copied().collect(),
        finite,
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

        let (embedding, _, trace) = decode(&raw, &spec(), false);
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
        let (embedding, _, trace) = decode(&raw, &spec(), false);

        assert_eq!(embedding.count, 2);
        assert_eq!(embedding.dimensions, Dimensions::Uniform { value: 2 });
        assert_eq!(trace.matched[&DecodeField::Vectors], "$.embeddings");
    }

    #[test]
    fn decodes_a_bare_vector_at_the_root() {
        let raw = serde_json::json!([0.3, 0.4]);
        let (embedding, _, _) = decode(&raw, &spec(), false);

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

        let (embedding, vectors, _) = decode(&raw, &spec(), true);
        assert_eq!(embedding.encoding, VectorEncoding::Base64);
        assert_eq!(embedding.dimensions, Dimensions::Uniform { value: 3 });
        assert!((embedding.vectors[1].norm - 3.0).abs() < 1e-6);
        assert_eq!(vectors.as_slice()[0], vec![1.0, 0.0, 0.0]);
    }

    #[test]
    fn a_truncated_base64_payload_is_reported() {
        // Five bytes: not a whole number of f32 values.
        let raw = serde_json::json!({"embeddings": [BASE64.encode([1_u8, 2, 3, 4, 5])]});
        let (embedding, _, trace) = decode(&raw, &spec(), false);

        assert_eq!(embedding.count, 0);
        assert!(trace.issues[0].message.contains("whole number of f32"));
    }

    #[test]
    fn inconsistent_widths_are_surfaced_not_averaged_away() {
        let raw = serde_json::json!({"embeddings": [[1.0, 2.0, 3.0], [1.0, 2.0]]});
        let (embedding, _, _) = decode(&raw, &spec(), false);

        assert_eq!(
            embedding.dimensions,
            Dimensions::Ragged { values: vec![3, 2] }
        );
        assert!(embedding.dimensions.uniform().is_none());
    }

    #[test]
    fn a_null_inside_a_vector_keeps_its_width_and_fails_finiteness() {
        let raw = serde_json::json!({"embeddings": [[1.0, null, 3.0]]});
        let (embedding, _, _) = decode(&raw, &spec(), false);

        assert_eq!(embedding.dimensions, Dimensions::Uniform { value: 3 });
        assert!(!embedding.vectors[0].finite);
        // The norm ignores the hole rather than becoming NaN itself.
        assert!(embedding.vectors[0].norm.is_finite());
    }

    #[test]
    fn a_zero_vector_has_a_zero_norm() {
        let raw = serde_json::json!({"embeddings": [[0.0, 0.0, 0.0]]});
        let (embedding, _, _) = decode(&raw, &spec(), false);
        assert!((embedding.vectors[0].norm - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn full_vectors_are_absent_unless_asked_for() {
        let raw = serde_json::json!({"embeddings": [[1.0, 2.0]]});

        let (without, _, _) = decode(&raw, &spec(), false);
        assert!(without.full.is_none());
        let rendered = serde_json::to_string(&without).unwrap();
        assert!(!rendered.contains("full"), "{rendered}");

        let (with, _, _) = decode(&raw, &spec(), true);
        assert_eq!(with.full.unwrap(), vec![vec![1.0, 2.0]]);
    }

    #[test]
    fn a_long_vector_is_only_ever_previewed() {
        let values: Vec<f32> = (0..1024_u16).map(|i| f32::from(i) / 1024.0).collect();
        let raw = serde_json::json!({"embeddings": [values]});

        let (embedding, _, _) = decode(&raw, &spec(), false);
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
    fn max_deviation_compares_shape_first() {
        let a = Vectors(vec![vec![1.0, 2.0]]);
        let b = Vectors(vec![vec![1.0, 2.000_01]]);
        let c = Vectors(vec![vec![1.0, 2.0, 3.0]]);

        assert!(a.max_deviation(&b).unwrap() > 0.0);
        assert!(a.max_deviation(&a).unwrap() < f32::EPSILON);
        assert!(a.max_deviation(&c).is_none());
    }
}
