//! Pure parallel-variant metadata. Isolation is an execution policy, not a backend concern.

pub const VARIANT_LETTERS: [char; 3] = ['A', 'B', 'C'];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantMetadata {
    pub group_id: String,
    pub variant: String,
    pub task: String,
    pub isolated: bool,
}

pub fn variant_metadata(group_id: &str, task: &str, count: usize) -> Vec<VariantMetadata> {
    VARIANT_LETTERS
        .into_iter()
        .take(count.clamp(1, VARIANT_LETTERS.len()))
        .map(|variant| VariantMetadata {
            group_id: group_id.to_owned(),
            variant: variant.to_string(),
            task: match variant {
                'A' => task.to_owned(),
                'B' => format!("{task}\n\nApproach hint: prefer the minimal, surgical change."),
                'C' => format!("{task}\n\nApproach hint: prefer a thorough, structural approach."),
                _ => task.to_owned(),
            },
            isolated: true,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_are_bounded_and_always_isolated() {
        let variants = variant_metadata("group", "compare", 99);
        assert_eq!(variants.len(), 3);
        assert_eq!(variants[0].variant, "A");
        assert!(variants[1].task.contains("minimal, surgical"));
        assert!(variants[2].task.contains("thorough, structural"));
        assert!(variants.iter().all(|variant| variant.isolated));
    }
}
