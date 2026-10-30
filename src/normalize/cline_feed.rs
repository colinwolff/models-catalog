//! Re-exports Cline feed parsing and merging from [`crate::vendors::cline::feed`].

pub use crate::vendors::cline::feed::{
    RawClineCollection, RawClineItem, RawClineModelEntry, RawClineRecommendedFeed,
    merge_cline_feed, parse_and_merge_cline_feed,
};
