//! The built-in price table: dated rows, one per model and price change, checked by hand against
//! the providers' lists (`tools/prices-from-litellm.py` compares them with LiteLLM's).

use chrono::NaiveDate;

#[expect(
    clippy::panic,
    reason = "evaluated at compile time for the built-in table: a bad date fails the build"
)]
const fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    match NaiveDate::from_ymd_opt(y, m, d) {
        Some(x) => x,
        None => panic!("invalid date in the built-in price table"),
    }
}

/// Built-in prices, USD per MTok (anthropic first-party API rates; checked 2026-11-22 against the
/// Claude API model table and Claude Code's own cache). Cache writes are 1.25x (5 min) and 2x
/// (1 h) the input rate; cache reads vary by model. Web search is $10 per 1000 requests.
///
/// The date is the first day the price applies: the date of the model's first snapshot (the
/// suffix of its dated id, a few days before the public launch), or its launch date, so no
/// request of the model predates it. Rows marked (to verify) carry a safe lower bound instead of
/// a checked launch date: the launch of the model they succeed (Opus 4.6, 2026-02-05, for the
/// later Opus and every Fable and Mythos model, which share the Opus 4.7 tokenizer), or for
/// Opus 5.5 the first day of its knowledge-cutoff month (June 2026). Too early only means the
/// price covers days without requests; too late would leave requests unpriced.
/// Columns: model, from, input, output, cache read, fast multiplier.
pub(super) type BuiltinRow = (&'static str, NaiveDate, f64, f64, f64, Option<f64>);
pub(super) const BUILTIN: &[BuiltinRow] = &[
    // (to verify) launch dates: lower bounds, see above.
    (
        "claude-fable-5-2",
        date(2026, 2, 5),
        8.0,
        40.0,
        0.20,
        Some(2.0),
    ),
    (
        "claude-mythos-5-2",
        date(2026, 2, 5),
        8.0,
        40.0,
        0.20,
        Some(2.0),
    ),
    ("claude-fable-5-1", date(2026, 2, 5), 10.0, 50.0, 0.25, None),
    (
        "claude-mythos-5-1",
        date(2026, 2, 5),
        10.0,
        50.0,
        0.25,
        None,
    ),
    ("claude-fable-5", date(2026, 2, 5), 10.0, 50.0, 1.00, None),
    ("claude-mythos-5", date(2026, 2, 5), 10.0, 50.0, 1.00, None),
    (
        "claude-opus-5-5",
        date(2026, 6, 1),
        4.0,
        20.0,
        0.20,
        Some(2.0),
    ),
    (
        "claude-opus-5",
        date(2026, 2, 5),
        5.0,
        25.0,
        0.50,
        Some(2.0),
    ),
    (
        "claude-opus-4-8",
        date(2026, 2, 5),
        5.0,
        25.0,
        0.50,
        Some(2.0),
    ),
    ("claude-opus-4-7", date(2026, 2, 5), 5.0, 25.0, 0.50, None),
    // Checked launch dates and snapshot dates from here on, unless marked.
    ("claude-opus-4-6", date(2026, 2, 5), 5.0, 25.0, 0.50, None),
    ("claude-opus-4-5", date(2025, 11, 1), 5.0, 25.0, 0.50, None),
    ("claude-opus-4-1", date(2025, 8, 5), 15.0, 75.0, 1.50, None),
    ("claude-opus-4", date(2025, 5, 14), 15.0, 75.0, 1.50, None),
    // (to verify) Sonnet 5.5 and 5: lower bound = the Sonnet 4.6 launch.
    (
        "claude-sonnet-5-5",
        date(2026, 2, 17),
        2.0,
        10.0,
        0.10,
        None,
    ),
    ("claude-sonnet-5", date(2026, 2, 17), 2.0, 10.0, 0.20, None),
    (
        "claude-sonnet-4-6",
        date(2026, 2, 17),
        3.0,
        15.0,
        0.30,
        None,
    ),
    (
        "claude-sonnet-4-5",
        date(2025, 9, 29),
        3.0,
        15.0,
        0.30,
        None,
    ),
    ("claude-sonnet-4", date(2025, 5, 14), 3.0, 15.0, 0.30, None),
    (
        "claude-3-7-sonnet",
        date(2025, 2, 19),
        3.0,
        15.0,
        0.30,
        None,
    ),
    (
        "claude-3-5-sonnet",
        date(2024, 6, 20),
        3.0,
        15.0,
        0.30,
        None,
    ),
    // (to verify) Haiku 5.5: lower bound = the Haiku 4.5 snapshot.
    (
        "claude-haiku-5-5",
        date(2025, 10, 1),
        0.10,
        0.50,
        0.01,
        None,
    ),
    ("claude-haiku-4-5", date(2025, 10, 1), 1.0, 5.0, 0.10, None),
    (
        "claude-3-5-haiku",
        date(2024, 10, 22),
        0.80,
        4.0,
        0.08,
        None,
    ),
    ("claude-3-haiku", date(2024, 3, 7), 0.25, 1.25, 0.03, None),
    // Google Gemini API, paid tier, standard (USD per MTok), as listed on
    // aistudio.google.com/docs/pricing and LiteLLM's `gemini/…` entries on 2026-11-24. The
    // cache column is the cached-input price; Antigravity transcripts record no cache writes.
    // Dates are each model's general availability, earlier where a preview ran first.
    ("gemini-4-neon", date(2026, 11, 18), 3.00, 15.00, 0.15, None),
    // Carbon and Argon: introductory price through 2026-11-30, doubled from 2026-12-01.
    // (to verify) the preview start; the GA of Carbon was 2026-11-10.
    (
        "gemini-4-carbon",
        date(2026, 9, 30),
        2.00,
        10.00,
        0.10,
        None,
    ),
    (
        "gemini-4-carbon",
        date(2026, 12, 1),
        4.00,
        20.00,
        0.20,
        None,
    ),
    ("gemini-4-argon", date(2026, 9, 30), 2.00, 10.00, 0.10, None),
    ("gemini-4-argon", date(2026, 12, 1), 4.00, 20.00, 0.20, None),
    (
        "gemini-4-hydrogen",
        date(2026, 11, 12),
        1.50,
        7.50,
        0.075,
        None,
    ),
    // (to verify) the 3.x release dates; the Flash prices double from 2027-01-01.
    (
        "gemini-3.8-flash",
        date(2026, 1, 1),
        0.75,
        3.75,
        0.075,
        None,
    ),
    ("gemini-3.8-flash", date(2027, 1, 1), 1.50, 7.50, 0.15, None),
    (
        "gemini-3.7-flash",
        date(2026, 1, 1),
        0.75,
        3.75,
        0.075,
        None,
    ),
    (
        "gemini-3.6-flash",
        date(2026, 1, 1),
        0.75,
        3.75,
        0.075,
        None,
    ),
    ("gemini-3.6-flash", date(2027, 1, 1), 1.50, 7.50, 0.15, None),
    (
        "gemini-3.5-flash-lite",
        date(2026, 1, 1),
        0.30,
        2.50,
        0.03,
        None,
    ),
    ("gemini-3.5-flash", date(2026, 1, 1), 1.50, 9.00, 0.15, None),
    ("gemini-3.1-pro*", date(2026, 1, 1), 2.00, 12.00, 0.20, None),
    (
        "gemini-3.1-flash-lite*",
        date(2026, 1, 1),
        0.25,
        1.50,
        0.025,
        None,
    ),
    ("gemini-3-pro*", date(2025, 11, 18), 2.00, 12.00, 0.20, None),
    (
        "gemini-3-flash*",
        date(2025, 12, 17),
        0.50,
        3.00,
        0.05,
        None,
    ),
    (
        "gemini-2.5-pro",
        date(2025, 3, 25),
        1.25,
        10.00,
        0.125,
        None,
    ),
    (
        "gemini-2.5-flash-lite",
        date(2025, 6, 17),
        0.10,
        0.40,
        0.01,
        None,
    ),
    (
        "gemini-2.5-flash",
        date(2025, 4, 17),
        0.30,
        2.50,
        0.03,
        None,
    ),
    // OpenAI API, standard tier (USD per MTok), as listed on developers.openai.com/api/docs/pricing
    // and LiteLLM's entries on 2026-11-24: the models Codex offers and has offered. The cache
    // column is the cached-input price. Cache writes cost 1.25x the input rate from GPT-5.6 on;
    // before, OpenAI billed none (see `writes_at_input`). The priority tier (Codex's fast mode)
    // costs 2x (2.5x for GPT-5.5). Dates are each model's snapshot; rows marked (to verify) carry
    // the snapshot of the model they succeed, a lower bound.
    (
        "gpt-6.2-astra",
        date(2026, 9, 29),
        8.00,
        40.00,
        0.40,
        Some(2.0),
    ), // (to verify)
    (
        "gpt-6.2-sol",
        date(2026, 9, 29),
        2.00,
        10.00,
        0.10,
        Some(2.0),
    ), // (to verify)
    (
        "gpt-6.1-sol",
        date(2026, 9, 29),
        2.00,
        10.00,
        0.10,
        Some(2.0),
    ),
    (
        "gpt-6.1-terra",
        date(2026, 9, 29),
        1.00,
        5.00,
        0.05,
        Some(2.0),
    ), // (to verify)
    (
        "gpt-6-astra",
        date(2026, 9, 3),
        10.00,
        50.00,
        1.00,
        Some(2.0),
    ),
    ("gpt-6-sol", date(2026, 9, 22), 2.00, 10.00, 0.20, Some(2.0)),
    ("gpt-6-luna", date(2026, 9, 22), 0.10, 0.50, 0.01, Some(2.0)),
    (
        "gpt-5.6-sol",
        date(2026, 7, 9),
        4.00,
        20.00,
        0.40,
        Some(2.0),
    ),
    (
        "gpt-5.6-terra",
        date(2026, 7, 9),
        2.00,
        12.00,
        0.20,
        Some(2.0),
    ),
    (
        "gpt-5.6-luna",
        date(2026, 7, 9),
        0.20,
        1.20,
        0.02,
        Some(2.0),
    ),
    ("gpt-5.6", date(2026, 7, 9), 4.00, 20.00, 0.40, Some(2.0)),
    ("gpt-5.5", date(2026, 4, 23), 5.00, 30.00, 0.50, Some(2.5)),
    (
        "gpt-5.4-mini",
        date(2026, 3, 17),
        0.75,
        4.50,
        0.075,
        Some(2.0),
    ),
    ("gpt-5.4", date(2026, 3, 5), 2.50, 15.00, 0.25, Some(2.0)),
    (
        "gpt-5.3-codex",
        date(2025, 12, 11),
        1.75,
        14.00,
        0.175,
        Some(2.0),
    ), // (to verify)
    (
        "gpt-5.2-codex",
        date(2025, 12, 11),
        1.75,
        14.00,
        0.175,
        None,
    ), // (to verify)
    ("gpt-5.2", date(2025, 12, 11), 1.75, 14.00, 0.175, Some(2.0)),
    (
        "gpt-5.1-codex-max",
        date(2025, 11, 13),
        1.25,
        10.00,
        0.125,
        None,
    ), // (to verify)
    (
        "gpt-5.1-codex-mini",
        date(2025, 11, 13),
        0.25,
        2.00,
        0.025,
        None,
    ),
    (
        "gpt-5.1-codex",
        date(2025, 11, 13),
        1.25,
        10.00,
        0.125,
        None,
    ),
    ("gpt-5.1", date(2025, 11, 13), 1.25, 10.00, 0.125, Some(2.0)),
    ("gpt-5-codex", date(2025, 8, 7), 1.25, 10.00, 0.125, None), // (to verify)
    ("gpt-5-mini", date(2025, 8, 7), 0.25, 2.00, 0.025, Some(1.8)),
    ("gpt-5", date(2025, 8, 7), 1.25, 10.00, 0.125, Some(2.0)),
];

/// Prices above a prompt size: (model as in `BUILTIN`, prompt tokens, input and output
/// multipliers; cache reads scale with input).
pub(super) const LONG_CONTEXT: &[(&str, u64, f64, f64)] = &[
    // Claude Haiku 5.5: $0.50 / $2.50 above 100K prompt tokens. Whether cache rates scale the
    // same way is (to verify); assumed yes.
    ("claude-haiku-5-5", 100_000, 5.0, 5.0),
    // Gemini Pro above 200K: 2.5 Pro $2.50 / $15.00, 3.x Pro $4.00 / $18.00 (cache ×2).
    ("gemini-2.5-pro", 200_000, 2.0, 1.5),
    ("gemini-3.1-pro*", 200_000, 2.0, 1.5),
    ("gemini-3-pro*", 200_000, 2.0, 1.5),
    // OpenAI above 272K prompt tokens: input (and cached input, cache writes) x2, output x1.5.
    ("gpt-6.2-astra", 272_000, 2.0, 1.5),
    ("gpt-6.2-sol", 272_000, 2.0, 1.5),
    ("gpt-6.1-sol", 272_000, 2.0, 1.5),
    ("gpt-6.1-terra", 272_000, 2.0, 1.5),
    ("gpt-6-astra", 272_000, 2.0, 1.5),
    ("gpt-6-sol", 272_000, 2.0, 1.5),
    ("gpt-6-luna", 272_000, 2.0, 1.5),
    ("gpt-5.6-sol", 272_000, 2.0, 1.5),
    ("gpt-5.6-terra", 272_000, 2.0, 1.5),
    ("gpt-5.6-luna", 272_000, 2.0, 1.5),
    ("gpt-5.6", 272_000, 2.0, 1.5),
    ("gpt-5.5", 272_000, 2.0, 1.5),
    ("gpt-5.4", 272_000, 2.0, 1.5),
];

/// Whether cache writes cost the input rate (no write premium): Gemini (Antigravity records no
/// cache writes), and OpenAI models before GPT-5.6, for which OpenAI lists no cache-write price.
pub(super) fn writes_at_input(model: &str) -> bool {
    model.starts_with("gemini-") || (model.starts_with("gpt-5") && !model.starts_with("gpt-5.6"))
}
