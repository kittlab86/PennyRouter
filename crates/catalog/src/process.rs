//! Deterministic processing: payloads into fixed indexed columns.
//!
//! Pure-code extraction only (Phase 2a). Model-judged signals plug in
//! later (Phase 2b) behind the same per-endpoint update.

use crate::store::{Catalog, EndpointIndex};
use crate::Error;
use serde_json::Value;
use std::collections::hash_map::Entry;

pub struct ProcessSummary {
    pub processed: usize,
}

/// Extract indexed values for every endpoint. Idempotent: re-running
/// overwrites the same columns. Model-level signals (decision
/// modality, Iter 24) resolve through a per-model payload cache so
/// one lookup serves all of a model's endpoints.
pub fn run_processing(catalog: &Catalog, processed_at: &str) -> Result<ProcessSummary, Error> {
    let endpoints = catalog.all_endpoints()?;
    // One transaction for the whole run (plus its meta): a mid-run
    // failure rolls every update back instead of half-processing.
    catalog.transaction(|| {
        let mut models: std::collections::HashMap<String, Option<Value>> =
            std::collections::HashMap::new();
        let mut processed = 0;
        for ep in &endpoints {
            let payload: Value = serde_json::from_str(&ep.payload)?;
            // A broken model lookup fails the run like a broken
            // endpoint payload: silently caching `None` would mark
            // every endpoint unknown with no signal.
            let model_payload = match models.entry(ep.model_id.clone()) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => {
                    let row = catalog.get_model(&ep.model_id)?;
                    let parsed = row
                        .map(|model| serde_json::from_str(&model.payload))
                        .transpose()?;
                    entry.insert(parsed)
                }
            };
            catalog.update_endpoint_index(
                ep.id,
                &EndpointIndex {
                    price_prompt: extract_price(&payload, "prompt"),
                    price_completion: extract_price(&payload, "completion"),
                    price_cache_read: extract_price(&payload, "input_cache_read"),
                    price_cache_write: extract_price(&payload, "input_cache_write"),
                    precision_class: Some(classify_precision(&payload)),
                    promo_discount: extract_discount(&payload),
                    price_override_tokens: extract_override_tokens(&payload),
                    processed_at: processed_at.to_string(),
                    supports_tools: extract_tool_support(&payload),
                    supports_decisions: extract_decision_support(model_payload.as_ref()),
                },
            )?;
            processed += 1;
        }
        catalog.set_meta("processed_at", processed_at)?;
        catalog.set_meta("processed_count", &processed.to_string())?;
        Ok(ProcessSummary { processed })
    })
}

/// Per-token price as f64; accepts a string or a number, else None.
/// Negative or non-finite values are rejected (Iter 11, A02): an
/// operator-imported "-1" must never sort ahead of valid prices.
fn extract_price(payload: &Value, key: &str) -> Option<f64> {
    let price = match payload.get("pricing")?.get(key)? {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }?;
    (price.is_finite() && price >= 0.0).then_some(price)
}

/// Tool support per endpoint (Iter 22): OpenRouter lists the request
/// parameters each endpoint accepts in `supported_parameters`. A
/// `"tools"` entry means function calling works there; the flag
/// varies per provider on the same model, so it is indexed per
/// endpoint. Missing or non-array field is `None` (unknown), never
/// assumed capable or incapable.
fn extract_tool_support(payload: &Value) -> Option<i64> {
    let params = payload.get("supported_parameters")?.as_array()?;
    Some(i64::from(
        params.iter().any(|p| p.as_str() == Some("tools")),
    ))
}

/// Decision-modality support per endpoint (Iter 24): modality lives
/// on the parent *model* object (`architecture.output_modalities`),
/// so the caller passes the model's payload. A `"decisions"` entry
/// marks behavior-scoring models (e.g. Respan Span); routing matches
/// the `decision` tier on it. Missing model, architecture, or array
/// is `None` (unknown), never assumed.
fn extract_decision_support(model: Option<&Value>) -> Option<i64> {
    let modalities = model?
        .get("architecture")?
        .get("output_modalities")?
        .as_array()?;
    Some(i64::from(
        modalities.iter().any(|m| m.as_str() == Some("decisions")),
    ))
}

fn extract_discount(payload: &Value) -> Option<f64> {
    let discount = match payload.get("pricing")?.get("discount")? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }?;
    // Same 0.0..=1.0 range the promo windows enforce: an out-of-range
    // value (e.g. an imported 2.0) extracts as unknown instead of
    // clamping to cost 0 and publishing as permanently free.
    (discount.is_finite() && (0.0..=1.0).contains(&discount)).then_some(discount)
}

/// First long-context price tier (Q13-C): the smallest
/// `pricing.overrides[].min_prompt_tokens`, if any. Past this many
/// input tokens the endpoint bills its override price instead of the
/// base price, so the gateway skips the offering beyond it rather
/// than silently billing ~2×. Missing/empty/garbage overrides mean
/// no tier (`None`); a zero threshold means the base price never
/// applies, which the gateway treats as never fitting.
fn extract_override_tokens(payload: &Value) -> Option<i64> {
    payload
        .get("pricing")?
        .get("overrides")?
        .as_array()?
        .iter()
        .filter_map(|entry| entry.get("min_prompt_tokens"))
        .filter_map(|value| match value {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.parse().ok(),
            _ => None,
        })
        .filter(|tokens| *tokens >= 0)
        .min()
}

/// Quantization label to precision class. Anything unrecognized (including
/// missing) is "unknown" — never silently treated as a known class.
fn classify_precision(payload: &Value) -> String {
    let q = payload
        .get("quantization")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    if q.contains("fp32") {
        "fp32"
    } else if q.contains("bf16") || q.contains("fp16") {
        "fp16"
    } else if q.contains("fp8") {
        "fp8"
    } else if q.contains("int8") {
        "int8"
    } else if q.contains("int4") || q.contains("nf4") || q.contains("fp4") {
        "int4"
    } else {
        "unknown"
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn price_accepts_string_number_or_nothing() {
        let payload = json!({"pricing": {
            "prompt": "0.0000005",
            "completion": 0.25,
            "input_cache_read": null,
            "input_cache_write": "garbage",
        }});
        assert_eq!(extract_price(&payload, "prompt"), Some(0.0000005));
        assert_eq!(extract_price(&payload, "completion"), Some(0.25));
        assert_eq!(extract_price(&payload, "input_cache_read"), None);
        assert_eq!(extract_price(&payload, "input_cache_write"), None);
        assert_eq!(extract_price(&payload, "missing"), None);
        assert_eq!(extract_price(&json!({}), "prompt"), None);
    }

    /// A02: negative or non-finite prices are unpriceable, never
    /// cheapest-first.
    #[test]
    fn negative_and_nonfinite_prices_rejected() {
        assert_eq!(
            extract_price(&json!({"pricing": {"prompt": -1}}), "prompt"),
            None
        );
        assert_eq!(
            extract_price(&json!({"pricing": {"prompt": "-1"}}), "prompt"),
            None
        );
        assert_eq!(
            extract_price(&json!({"pricing": {"prompt": "NaN"}}), "prompt"),
            None
        );
        assert_eq!(
            extract_price(&json!({"pricing": {"prompt": "inf"}}), "prompt"),
            None
        );
        assert_eq!(
            extract_discount(&json!({"pricing": {"discount": -0.5}})),
            None
        );
        // Valid zero still prices (free is real).
        assert_eq!(
            extract_price(&json!({"pricing": {"prompt": 0}}), "prompt"),
            Some(0.0)
        );
    }

    /// Q13-C: the long-context tier threshold indexes the smallest
    /// `min_prompt_tokens` across overrides; missing/empty/garbage
    /// means no tier.
    #[test]
    fn override_threshold_indexes_minimum() {
        assert_eq!(
            extract_override_tokens(&json!({"pricing": {"overrides": [
                {"min_prompt_tokens": 500000},
                {"min_prompt_tokens": 272000},
            ]}})),
            Some(272000)
        );
        assert_eq!(
            extract_override_tokens(&json!({"pricing": {"overrides": []}})),
            None
        );
        assert_eq!(extract_override_tokens(&json!({"pricing": {}})), None);
        assert_eq!(extract_override_tokens(&json!({})), None);
        assert_eq!(
            extract_override_tokens(
                &json!({"pricing": {"overrides": [{"min_prompt_tokens": "abc"}]}})
            ),
            None
        );
    }

    #[test]
    fn precision_maps_known_quantizations() {
        let class = |q: &str| classify_precision(&json!({"quantization": q}));
        assert_eq!(class("fp32"), "fp32");
        assert_eq!(class("fp16"), "fp16");
        assert_eq!(class("BF16"), "fp16");
        assert_eq!(class("fp8"), "fp8");
        assert_eq!(class("fp8_e4m3"), "fp8");
        assert_eq!(class("int8"), "int8");
        assert_eq!(class("nf4"), "int4");
    }

    #[test]
    fn precision_falls_back_to_unknown() {
        assert_eq!(
            classify_precision(&json!({"quantization": "unknown"})),
            "unknown"
        );
        assert_eq!(
            classify_precision(&json!({"quantization": "w4a8-exotic"})),
            "unknown"
        );
        assert_eq!(classify_precision(&json!({"quantization": ""})), "unknown");
        assert_eq!(classify_precision(&json!({})), "unknown");
    }
}
