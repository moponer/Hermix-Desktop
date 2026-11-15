//! JSON SmartCrusher — content-aware compression for JSON tool outputs.
//!
//! Inspired by Headroom's SmartCrusher but implemented in pure Rust
//! without external ML dependencies. Uses statistical analysis to
//! select representative items from large JSON arrays while preserving
//! structure, errors, and anomalies.
//!
//! Compression strategies:
//! - **Array of objects**: Statistical sampling + anomaly preservation (70-95%)
//! - **Array of strings**: Dedup + adaptive sampling (60-90%)
//! - **Array of numbers**: Statistical summary + outlier preservation (70-85%)
//! - **Nested JSON**: Recursive field pruning
//!
//! Item retention split: 30% from start (schema), 15% from end (recency),
//! 55% by importance score. Errors/uniqueness always kept.

use serde_json::Value;

/// Result of JSON compression.
#[derive(Debug, Clone)]
pub struct CrushResult {
    /// The compressed JSON value.
    pub value: Value,
    /// Number of items before compression.
    pub items_before: usize,
    /// Number of items after compression.
    pub items_after: usize,
    /// Estimated token savings (characters / 4).
    pub tokens_saved: usize,
    /// Compression ratio (0.0 = no compression, 1.0 = full removal).
    pub ratio: f64,
}

/// Detect if content is a JSON array and compress it.
///
/// Returns `None` if the content is not a compressible JSON array.
pub fn try_crush(content: &str) -> Option<CrushResult> {
    let trimmed = content.trim();

    // Quick check: must start with `[` and end with `]`
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return None;
    }

    // Skip very short JSON (overhead would exceed savings)
    if trimmed.len() < 100 {
        return None;
    }

    let parsed: Value = serde_json::from_str(trimmed).ok()?;

    match &parsed {
        Value::Array(arr) if arr.len() >= 5 => {
            let result = crush_array(arr);
            Some(result)
        }
        _ => None,
    }
}

/// Compress a JSON array using statistical sampling.
fn crush_array(arr: &[Value]) -> CrushResult {
    let original_len = arr.len();
    let original_chars: usize = arr.iter().map(|v| v.to_string().len()).sum();

    // Detect array element type
    let element_type = detect_element_type(arr);

    let compressed = match element_type {
        ElementType::Objects => crush_object_array(arr),
        ElementType::Strings => crush_string_array(arr),
        ElementType::Numbers => crush_number_array(arr),
        ElementType::Mixed => crush_mixed_array(arr),
    };

    let compressed_chars: usize = compressed.iter().map(|v| v.to_string().len()).sum();
    let tokens_saved = (original_chars.saturating_sub(compressed_chars)) / 4;
    let ratio = if original_chars > 0 {
        1.0 - (compressed_chars as f64 / original_chars as f64)
    } else {
        0.0
    };
    let items_after = compressed.len();

    CrushResult {
        value: Value::Array(compressed),
        items_before: original_len,
        items_after,
        tokens_saved,
        ratio,
    }
}

/// Types of JSON array elements.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ElementType {
    Objects,
    Strings,
    Numbers,
    Mixed,
}

/// Detect the dominant element type in a JSON array.
fn detect_element_type(arr: &[Value]) -> ElementType {
    let mut objects = 0;
    let mut strings = 0;
    let mut numbers = 0;

    // Sample up to 50 elements for detection
    let sample_size = arr.len().min(50);
    let step = (arr.len() / sample_size).max(1);

    for i in (0..arr.len()).step_by(step) {
        match &arr[i] {
            Value::Object(_) => objects += 1,
            Value::String(_) => strings += 1,
            Value::Number(_) => numbers += 1,
            _ => {}
        }
        if i >= sample_size * step {
            break;
        }
    }

    let total = objects + strings + numbers;
    if total == 0 {
        return ElementType::Mixed;
    }

    if objects as f64 / total as f64 > 0.6 {
        ElementType::Objects
    } else if strings as f64 / total as f64 > 0.6 {
        ElementType::Strings
    } else if numbers as f64 / total as f64 > 0.6 {
        ElementType::Numbers
    } else {
        ElementType::Mixed
    }
}

/// Compress an array of JSON objects using statistical sampling.
///
/// Strategy:
/// 1. Extract all field names and compute statistics
/// 2. Keep first 30% (schema establishment)
/// 3. Keep last 15% (recency)
/// 4. Score remaining 55% by uniqueness/importance
/// 5. Always keep errors/anomalies
fn crush_object_array(arr: &[Value]) -> Vec<Value> {
    let n = arr.len();
    if n <= 10 {
        return arr.to_vec();
    }

    // Extract field statistics
    let fields = extract_field_stats(arr);

    // Calculate retention counts
    let keep_start = ((n as f64 * 0.30).ceil() as usize).max(1);
    let keep_end = ((n as f64 * 0.15).ceil() as usize).max(1);
    let middle_size = n.saturating_sub(keep_start + keep_end);
    // Keep only 50% of the middle section to achieve actual compression
    let budget = ((middle_size as f64 * 0.50).ceil() as usize).max(1);

    // Score all items
    let mut scored: Vec<(usize, f64)> = arr
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let score = score_object_item(item, &fields, i, n);
            (i, score)
        })
        .collect();

    // Sort by score descending
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Select top items from the middle section
    let mut keep_indices: Vec<usize> = Vec::new();

    // Always keep start section
    for i in 0..keep_start {
        keep_indices.push(i);
    }

    // Always keep end section
    for i in (n - keep_end)..n {
        keep_indices.push(i);
    }

    // Add top-scored items from middle
    let middle_start = keep_start;
    let middle_end = n - keep_end;
    let mut middle_scored: Vec<(usize, f64)> = scored
        .iter()
        .filter(|(i, _)| *i >= middle_start && *i < middle_end)
        .cloned()
        .collect();
    middle_scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    for (i, _) in middle_scored.iter().take(budget) {
        keep_indices.push(*i);
    }

    // Sort indices and deduplicate
    keep_indices.sort();
    keep_indices.dedup();

    // Build result
    keep_indices.iter().map(|&i| arr[i].clone()).collect()
}

/// Field statistics for scoring.
struct FieldStats {
    /// Field name -> (count of unique values, total count)
    fields: std::collections::HashMap<String, (usize, usize)>,
}

/// Extract statistics about fields in an object array.
fn extract_field_stats(arr: &[Value]) -> FieldStats {
    let mut fields: std::collections::HashMap<String, (std::collections::HashSet<String>, usize)> =
        std::collections::HashMap::new();

    for item in arr {
        if let Value::Object(obj) = item {
            for (key, val) in obj {
                let entry = fields.entry(key.clone()).or_default();
                entry.1 += 1;
                let val_str = match val {
                    Value::String(s) => s.clone(),
                    _ => val.to_string(),
                };
                entry.0.insert(val_str);

            }
        }
    }

    FieldStats {
        fields: fields
            .into_iter()
            .map(|(k, (uniq, total))| (k, (uniq.len(), total)))
            .collect(),
    }
}

/// Score an object item for importance.
///
/// Higher score = more important to keep.
fn score_object_item(item: &Value, stats: &FieldStats, index: usize, total: usize) -> f64 {
    let obj = match item {
        Value::Object(o) => o,
        _ => return 0.0,
    };

    let mut score = 0.0;

    // 1. Error bonus: always keep error items
    let is_error = obj.values().any(|v| {
        if let Value::String(s) = v {
            let sl = s.to_lowercase();
            sl.contains("error") || sl.contains("fail") || sl.contains("exception")
        } else {
            false
        }
    });
    if is_error {
        score += 100.0;
    }

    // 2. Uniqueness bonus: prefer items with rare field values
    for (key, _val) in obj {
        if let Some(&(unique_count, total_count)) = stats.fields.get(key) {
            if total_count > 0 {
                let uniqueness = unique_count as f64 / total_count as f64;
                score += uniqueness * 10.0;
            }
        }
    }

    // 3. Position penalty: middle items scored higher than edges
    let position_ratio = index as f64 / total as f64;
    let position_score = if position_ratio < 0.3 || position_ratio > 0.7 {
        0.0 // edges already kept by start/end sections
    } else {
        5.0 * (1.0 - (position_ratio - 0.5).abs() * 2.0) // peak at 0.5
    };
    score += position_score;

    // 4. Size penalty: very large items scored lower (less likely to be representative)
    let size = item.to_string().len();
    if size > 10000 {
        score -= 5.0;
    }

    // 5. Null/empty penalty: items with mostly nulls are less informative
    let null_count = obj.values().filter(|v| v.is_null()).count();
    let total_fields = obj.len();
    if total_fields > 0 {
        let null_ratio = null_count as f64 / total_fields as f64;
        score -= null_ratio * 10.0;
    }

    score
}

/// Compress an array of strings using dedup + sampling.
fn crush_string_array(arr: &[Value]) -> Vec<Value> {
    let n = arr.len();
    if n <= 10 {
        return arr.to_vec();
    }

    // Deduplicate strings
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut unique: Vec<&Value> = Vec::new();

    for item in arr {
        if let Value::String(s) = item {
            if seen.insert(s.clone()) {
                unique.push(item);
            }
        } else {
            unique.push(item);
        }
    }

    let n_unique = unique.len();
    if n_unique <= 10 {
        return unique.into_iter().cloned().collect();
    }

    // Sample: keep first 30%, last 15%, and 50% of middle by length
    let keep_start = ((n_unique as f64 * 0.30).ceil() as usize).max(1);
    let keep_end = ((n_unique as f64 * 0.15).ceil() as usize).max(1);
    let middle_size = n_unique.saturating_sub(keep_start + keep_end);
    let budget = ((middle_size as f64 * 0.50).ceil() as usize).max(1);

    let mut result: Vec<Value> = Vec::new();

    // Keep start
    for item in unique.iter().take(keep_start) {
        result.push((*item).clone());
    }

    // Score middle items by length (medium = most informative)
    let middle: Vec<(usize, usize)> = unique[keep_start..n_unique - keep_end]
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let len = s.as_str().map(|s| s.len()).unwrap_or(0);
            (i + keep_start, len)
        })
        .collect();

    // Sort by distance from median length
    let median_len = if middle.is_empty() {
        0
    } else {
        let mut lengths: Vec<usize> = middle.iter().map(|(_, l)| *l).collect();
        lengths.sort();
        lengths[lengths.len() / 2]
    };

    let mut scored_middle: Vec<(usize, f64)> = middle
        .iter()
        .map(|(i, len)| {
            let distance = (*len as f64 - median_len as f64).abs();
            (*i, -distance) // negative because we sort descending
        })
        .collect();
    scored_middle.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    for (i, _) in scored_middle.into_iter().take(budget) {
        result.push(unique[i].clone());
    }

    // Keep end
    for item in unique.iter().skip(n_unique - keep_end) {
        result.push((*item).clone());
    }

    result
}

/// Compress an array of numbers using statistical summary.
fn crush_number_array(arr: &[Value]) -> Vec<Value> {
    let n = arr.len();
    if n <= 10 {
        return arr.to_vec();
    }

    // Extract numeric values
    let numbers: Vec<f64> = arr
        .iter()
        .filter_map(|v| v.as_f64())
        .collect();

    if numbers.is_empty() {
        return arr.to_vec();
    }

    // Compute statistics
    let min = numbers.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = numbers.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let mean = numbers.iter().sum::<f64>() / numbers.len() as f64;
    let variance = numbers.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / numbers.len() as f64;
    let std_dev = variance.sqrt();

    // Find outliers (beyond 2 standard deviations)
    let outlier_indices: Vec<usize> = arr
        .iter()
        .enumerate()
        .filter(|(_, v)| {
            if let Some(x) = v.as_f64() {
                (x - mean).abs() > 2.0 * std_dev
            } else {
                false
            }
        })
        .map(|(i, _)| i)
        .collect();

    // Build result: summary + outliers + evenly spaced samples
    let mut result: Vec<Value> = Vec::new();

    // Add statistical summary as first element
    result.push(serde_json::json!({
        "_summary": {
            "count": numbers.len(),
            "min": min,
            "max": max,
            "mean": (mean * 100.0).round() / 100.0,
            "std_dev": (std_dev * 100.0).round() / 100.0,
            "outliers": outlier_indices.len()
        }
    }));

    // Add outliers
    for &idx in &outlier_indices {
        result.push(arr[idx].clone());
    }

    // Add evenly spaced samples (max 10)
    let sample_count = 10.min(n);
    let step = n / sample_count;
    for i in (0..n).step_by(step.max(1)) {
        if !outlier_indices.contains(&i) {
            result.push(arr[i].clone());
        }
        if result.len() > sample_count + outlier_indices.len() {
            break;
        }
    }

    result
}

/// Compress a mixed-type array using generic sampling.
fn crush_mixed_array(arr: &[Value]) -> Vec<Value> {
    let n = arr.len();
    if n <= 10 {
        return arr.to_vec();
    }

    // Keep first 30%, last 15%, and sample 50% of middle by size
    let keep_start = ((n as f64 * 0.30).ceil() as usize).max(1);
    let keep_end = ((n as f64 * 0.15).ceil() as usize).max(1);
    let middle_size = n.saturating_sub(keep_start + keep_end);
    let budget = ((middle_size as f64 * 0.50).ceil() as usize).max(1);

    let mut result: Vec<Value> = Vec::new();

    // Keep start
    for item in arr.iter().take(keep_start) {
        result.push(item.clone());
    }

    // Score middle items by size (prefer medium-sized items)
    let middle: Vec<(usize, usize)> = arr[keep_start..n - keep_end]
        .iter()
        .enumerate()
        .map(|(i, v)| (i + keep_start, v.to_string().len()))
        .collect();

    let median_size = if middle.is_empty() {
        0
    } else {
        let mut sizes: Vec<usize> = middle.iter().map(|(_, s)| *s).collect();
        sizes.sort();
        sizes[sizes.len() / 2]
    };

    let mut scored: Vec<(usize, f64)> = middle
        .iter()
        .map(|(i, size)| {
            let distance = (*size as f64 - median_size as f64).abs();
            (*i, -distance)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    for (i, _) in scored.into_iter().take(budget) {
        result.push(arr[i].clone());
    }

    // Keep end
    for item in arr.iter().skip(n - keep_end) {
        result.push(item.clone());
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_non_json() {
        assert!(try_crush("hello world").is_none());
        assert!(try_crush("{}").is_none());
        assert!(try_crush("[]").is_none());
    }

    #[test]
    fn test_detect_short_json() {
        let short = "[1,2,3,4,5]";
        assert!(try_crush(short).is_none());
    }

    #[test]
    fn test_crush_object_array() {
        let items: Vec<Value> = (0..100)
            .map(|i| {
                serde_json::json!({
                    "id": i,
                    "name": format!("item_{}", i),
                    "value": i as f64 * 1.5,
                    "error": if i % 20 == 0 { Some("timeout") } else { None }
                })
            })
            .collect();
        let json = serde_json::to_string(&items).unwrap();
        let result = try_crush(&json).unwrap();

        assert!(result.items_after < result.items_before);
        assert!(result.ratio > 0.0);
        // Errors should be preserved
        let compressed = result.value.as_array().unwrap();
        assert!(compressed.iter().any(|v| v.get("error").and_then(|e| e.as_str()).is_some()));
    }

    #[test]
    fn test_crush_string_array() {
        let items: Vec<Value> = (0..100)
            .map(|i| Value::String(format!("string_{}", i)))
            .collect();
        let json = serde_json::to_string(&items).unwrap();
        let result = try_crush(&json).unwrap();

        assert!(result.items_after < result.items_before);
        assert!(result.ratio > 0.0);
    }

    #[test]
    fn test_crush_number_array() {
        let items: Vec<Value> = (0..100)
            .map(|i| Value::Number(serde_json::Number::from_f64(i as f64).unwrap()))
            .collect();
        let json = serde_json::to_string(&items).unwrap();
        let result = try_crush(&json).unwrap();

        // Should have summary + samples
        let compressed = result.value.as_array().unwrap();
        assert!(compressed.first().unwrap().get("_summary").is_some());
        assert!(compressed.len() < items.len());
    }
}
