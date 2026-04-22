//! In-memory catalogue of skill summaries — supports filtering by
//! category, fuzzy name search, and simple counting.
//!
//! A [`Catalog`] is the normalised output of [`SkillSource::list`]; the
//! CLI builds one at every invocation so that downstream commands never
//! have to re-ask the source about what is installable.

use std::collections::BTreeMap;

use crate::skill::{Category, SkillSummary};

/// Sorted, filterable view over a set of skill summaries.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    entries: Vec<SkillSummary>,
}

impl Catalog {
    /// Build a catalogue from a raw list of summaries. Duplicates (by
    /// name) are silently deduplicated — the last occurrence wins, so
    /// layered sources can override earlier entries by simply appearing
    /// later in the composition order.
    pub fn from_summaries(mut summaries: Vec<SkillSummary>) -> Self {
        // Deduplicate by name, keeping the last occurrence.
        let mut by_name: BTreeMap<String, SkillSummary> = BTreeMap::new();
        for s in summaries.drain(..) {
            by_name.insert(s.name.clone(), s);
        }
        let mut entries: Vec<SkillSummary> = by_name.into_values().collect();
        entries.sort_by(|a, b| (a.category, &a.name).cmp(&(b.category, &b.name)));
        Self { entries }
    }

    /// Total number of skills after deduplication.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the catalogue contains no skills.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over every skill in catalogue order (category, then name).
    pub fn iter(&self) -> impl Iterator<Item = &SkillSummary> {
        self.entries.iter()
    }

    /// Iterate over the skills in a specific category.
    pub fn by_category(&self, category: Category) -> impl Iterator<Item = &SkillSummary> {
        self.entries.iter().filter(move |s| s.category == category)
    }

    /// Look a skill up by exact name.
    pub fn get(&self, name: &str) -> Option<&SkillSummary> {
        self.entries.iter().find(|s| s.name == name)
    }

    /// Return every (category, count) pair for skills in the catalogue.
    pub fn counts_per_category(&self) -> BTreeMap<Category, usize> {
        let mut out: BTreeMap<Category, usize> = BTreeMap::new();
        for s in &self.entries {
            *out.entry(s.category).or_insert(0) += 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum(name: &str, cat: Category, version: u32) -> SkillSummary {
        SkillSummary {
            name: name.to_string(),
            category: cat,
            version,
            description: format!("desc {name}"),
        }
    }

    #[test]
    fn catalog_sorts_by_category_then_name() {
        let cat = Catalog::from_summaries(vec![
            sum("b", Category::IssueTracking, 1),
            sum("a", Category::SelfBootstrap, 1),
            sum("c", Category::SelfBootstrap, 1),
        ]);
        let names: Vec<&str> = cat.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a", "c", "b"]);
    }

    #[test]
    fn catalog_deduplicates_by_name_keeping_last() {
        let cat = Catalog::from_summaries(vec![
            sum("a", Category::SelfBootstrap, 1),
            sum("a", Category::SelfBootstrap, 7), // wins
        ]);
        assert_eq!(cat.len(), 1);
        assert_eq!(cat.get("a").unwrap().version, 7);
    }

    #[test]
    fn catalog_filters_by_category() {
        let cat = Catalog::from_summaries(vec![
            sum("a", Category::SelfBootstrap, 1),
            sum("b", Category::IssueTracking, 1),
            sum("c", Category::SelfBootstrap, 1),
        ]);
        let only: Vec<&str> = cat
            .by_category(Category::SelfBootstrap)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(only, vec!["a", "c"]);
    }

    #[test]
    fn counts_per_category_is_accurate() {
        let cat = Catalog::from_summaries(vec![
            sum("a", Category::SelfBootstrap, 1),
            sum("b", Category::SelfBootstrap, 1),
            sum("c", Category::IssueTracking, 1),
        ]);
        let counts = cat.counts_per_category();
        assert_eq!(counts[&Category::SelfBootstrap], 2);
        assert_eq!(counts[&Category::IssueTracking], 1);
    }
}
