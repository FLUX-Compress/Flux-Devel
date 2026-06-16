//! # Compression Group
//!
//! ## Purpose
//! The compression group forms the core of FLUX's compression pipeline, transforming
//! preprocessed, structured data into highly compressed byte streams. It acts as the
//! bridge between structural/stride transformations (Step 6) and entropy coding (Step 8).
//!
//! ## LZ77 and PPM Complementarity
//! LZ77 and PPM target different forms of redundancy within data streams:
//! - **LZ77 (Dictionary-based Match Finder)**: Excels at finding and eliminating large-scale,
//!   exact repeating sequences. It replaces repeated sequences with back-references (length and distance pairs),
//!   performing extremely well on highly repetitive structures like code, tabular layouts, or run-length sequences.
//! - **PPM (Prediction by Partial Matching)**: Excels at modeling statistical patterns and predicting
//!   probabilities for individual symbols based on local contexts (up to order 8). It handles
//!   non-exact correlations, statistical imbalances, and text-like flows where matches are too short
//!   or noisy for LZ77.
//!
//! Together, they feed the rANS entropy coder. LZ77 handles high-frequency match-based redundancy,
//! while PPM predicts the probability distribution of unmatched literals.
//!
//! ## Content Types and Algorithm Routing
//! Different content types trigger specific paths through these compression modules:
//! - **Text / Source Code**: Routed primarily through PPM context modeling for maximum density,
//!   optionally preceded by LZ77 if long repetitions exist.
//! - **Multimedia / Stride Data (Audio, RGB, Floats)**: Run through the Secondary Symbol Estimator (SSE)
//!   and transpositions. SSE models cross-plane correlations and stride-periodic transitions, which are
//!   blended with PPM predictions in the Mixer.
//! - **Highly Redundant / Binary Blocks**: LZ77 is preferred to quickly eliminate matches before encoding.
//!
//! ## Integration with rANS (Step 8 Preview)
//! LZ77 outputs a sequence of `Lz77Token`s (literals and matches). PPM and SSE estimate the probability of each symbol
//! based on its context. These tokens and their corresponding probability distributions are fed into the rANS
//! (range Asymmetric Numeral System) entropy coder in Step 8 to pack them into a near-optimal bitstream.
//!
//! ## Architecture Diagram
//!
//! ```text
//!   Transformed Data
//!       │
//!       ├──► [LZ77] → literal/length/distance triplets
//!       │                        │
//!       ├──► [PPM]  → symbol probability distributions
//!       │                        │
//!       └─────────────────────────┘
//!                                │
//!                            [rANS] (Step 8)
//!                                │
//!                          Compressed output
//! ```

pub mod lz77;
pub mod mixer;
pub mod ppm;
pub mod rans;
pub mod secondary;
pub mod context;
pub mod clustering;
pub mod context_stats;
