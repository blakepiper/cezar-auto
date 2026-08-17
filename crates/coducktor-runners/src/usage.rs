//! Ported from `packages/cezar/src/core/usage.ts`.

/// Anthropic bills cache-read input at ~10% of standard input cost and cache creation at ~125%.
/// Weighting raw counts by these keeps the token number shown to the user roughly proportional
/// to dollar cost.
pub const CACHE_READ_WEIGHT: f64 = 0.1;
pub const CACHE_CREATION_WEIGHT: f64 = 1.25;

/// A loosely-typed usage record as emitted by the claude CLI stream.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RawUsage {
    pub input_tokens: Option<f64>,
    pub output_tokens: Option<f64>,
    pub cache_creation_input_tokens: Option<f64>,
    pub cache_read_input_tokens: Option<f64>,
}

/// Collapse a raw usage record into a single cost-weighted token count. Rounding happens once on
/// the total, not per field.
pub fn cost_weighted_tokens(usage: Option<&RawUsage>) -> f64 {
    let Some(usage) = usage else {
        return 0.0;
    };
    (usage.input_tokens.unwrap_or(0.0)
        + usage.output_tokens.unwrap_or(0.0)
        + usage.cache_creation_input_tokens.unwrap_or(0.0) * CACHE_CREATION_WEIGHT
        + usage.cache_read_input_tokens.unwrap_or(0.0) * CACHE_READ_WEIGHT)
        .round()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_or_empty_usage_is_zero() {
        assert_eq!(cost_weighted_tokens(None), 0.0);
        assert_eq!(cost_weighted_tokens(Some(&RawUsage::default())), 0.0);
    }

    #[test]
    fn input_and_output_tokens_count_at_face_value() {
        assert_eq!(
            cost_weighted_tokens(Some(&RawUsage {
                input_tokens: Some(1000.0),
                ..Default::default()
            })),
            1000.0
        );
        assert_eq!(
            cost_weighted_tokens(Some(&RawUsage {
                output_tokens: Some(250.0),
                ..Default::default()
            })),
            250.0
        );
    }

    #[test]
    fn cache_reads_are_discounted_and_creation_is_surcharged() {
        assert_eq!(
            cost_weighted_tokens(Some(&RawUsage {
                cache_read_input_tokens: Some(1000.0),
                ..Default::default()
            })),
            100.0
        );
        assert_eq!(
            cost_weighted_tokens(Some(&RawUsage {
                cache_creation_input_tokens: Some(1000.0),
                ..Default::default()
            })),
            1250.0
        );
    }

    #[test]
    fn sums_every_field_into_one_weighted_count() {
        let usage = RawUsage {
            input_tokens: Some(500.0),
            output_tokens: Some(200.0),
            cache_creation_input_tokens: Some(400.0),
            cache_read_input_tokens: Some(8000.0),
        };
        assert_eq!(
            cost_weighted_tokens(Some(&usage)),
            500.0 + 200.0 + 500.0 + 800.0
        );
    }

    #[test]
    fn ignores_fields_the_stream_omits() {
        let usage = RawUsage {
            input_tokens: Some(10.0),
            cache_read_input_tokens: Some(20.0),
            ..Default::default()
        };
        assert_eq!(cost_weighted_tokens(Some(&usage)), 12.0);
    }

    #[test]
    fn rounds_the_total_not_each_term() {
        assert_eq!(
            cost_weighted_tokens(Some(&RawUsage {
                cache_read_input_tokens: Some(15.0),
                ..Default::default()
            })),
            2.0
        );
        assert_eq!(
            cost_weighted_tokens(Some(&RawUsage {
                cache_read_input_tokens: Some(14.0),
                ..Default::default()
            })),
            1.0
        );
        assert_eq!(
            cost_weighted_tokens(Some(&RawUsage {
                cache_creation_input_tokens: Some(3.0),
                ..Default::default()
            })),
            4.0
        );
        assert_eq!(
            cost_weighted_tokens(Some(&RawUsage {
                cache_read_input_tokens: Some(5.0),
                cache_creation_input_tokens: Some(2.0),
                ..Default::default()
            })),
            3.0
        );
    }

    #[test]
    fn exposes_the_weights_it_applies() {
        assert_eq!(CACHE_READ_WEIGHT, 0.1);
        assert_eq!(CACHE_CREATION_WEIGHT, 1.25);
    }
}
