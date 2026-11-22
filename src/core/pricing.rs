//! API-equivalent prices as versioned data.
//!
//! A price is never a constant in code: every entry carries the date it takes effect from and the
//! source it came from. Three layers are merged, the later winning: the built-in table below, the
//! price table Claude Code itself caches in `.claude.json` (`additionalModelCostsCache`), and the
//! user's overrides in `config.toml`.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::usage::Usage;

/// USD per million tokens; `web_search` is USD per request.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rates {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
    #[serde(default)]
    pub web_search: f64,
    /// Fast mode (`usage.speed == "fast"`) multiplies every rate.
    #[serde(default)]
    pub fast_multiplier: Option<f64>,
    /// Higher rates when the prompt exceeds a threshold.
    #[serde(default)]
    pub long_context: Option<LongContext>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LongContext {
    /// Prompt tokens (input + cache writes + cache reads) above which the multipliers apply.
    pub threshold: u64,
    /// Applies to input, cache writes and cache reads.
    pub input_multiplier: f64,
    pub output_multiplier: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum PriceSource {
    Builtin,
    /// `additionalModelCostsCache` of a Claude Code config dir.
    ClaudeCode(String),
    User,
}

impl PriceSource {
    pub fn label(&self) -> String {
        match self {
            PriceSource::Builtin => "built-in".into(),
            PriceSource::ClaudeCode(seat) => format!("claude code ({seat})"),
            PriceSource::User => "config.toml".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PriceEntry {
    /// Exact model id, or a prefix ending in `*`.
    pub model: String,
    pub from: NaiveDate,
    pub rates: Rates,
    pub source: PriceSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Cost {
    pub usd: f64,
    /// False when no price is known for the model: the cost is then 0, flagged, never silent.
    pub priced: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PriceBook {
    entries: Vec<PriceEntry>,
}

const EPOCH: NaiveDate = NaiveDate::from_ymd_opt(2023, 1, 1).unwrap();

/// Built-in prices, USD per MTok (anthropic first-party API rates; checked 2026-11-22 against the
/// Claude API model table and Claude Code's own cache). Cache writes are 1.25x (5 min) and 2x
/// (1 h) the input rate; cache reads vary by model. Web search is $10 per 1000 requests.
/// Columns: model, input, output, cache read, fast multiplier.
const BUILTIN: &[(&str, f64, f64, f64, Option<f64>)] = &[
    ("claude-fable-5-2", 8.0, 40.0, 0.20, Some(2.0)),
    ("claude-mythos-5-2", 8.0, 40.0, 0.20, Some(2.0)),
    ("claude-fable-5-1", 10.0, 50.0, 0.25, None),
    ("claude-mythos-5-1", 10.0, 50.0, 0.25, None),
    ("claude-fable-5", 10.0, 50.0, 1.00, None),
    ("claude-mythos-5", 10.0, 50.0, 1.00, None),
    ("claude-opus-5-5", 4.0, 20.0, 0.20, Some(2.0)),
    ("claude-opus-5", 5.0, 25.0, 0.50, Some(2.0)),
    ("claude-opus-4-8", 5.0, 25.0, 0.50, Some(2.0)),
    ("claude-opus-4-7", 5.0, 25.0, 0.50, None),
    ("claude-opus-4-6", 5.0, 25.0, 0.50, None),
    ("claude-opus-4-5", 5.0, 25.0, 0.50, None),
    ("claude-opus-4-1", 15.0, 75.0, 1.50, None),
    ("claude-opus-4", 15.0, 75.0, 1.50, None),
    ("claude-sonnet-5-5", 2.0, 10.0, 0.10, None),
    ("claude-sonnet-5", 2.0, 10.0, 0.20, None),
    ("claude-sonnet-4-6", 3.0, 15.0, 0.30, None),
    ("claude-sonnet-4-5", 3.0, 15.0, 0.30, None),
    ("claude-sonnet-4", 3.0, 15.0, 0.30, None),
    ("claude-3-7-sonnet", 3.0, 15.0, 0.30, None),
    ("claude-3-5-sonnet", 3.0, 15.0, 0.30, None),
    ("claude-haiku-5-5", 0.10, 0.50, 0.01, None),
    ("claude-haiku-4-5", 1.0, 5.0, 0.10, None),
    ("claude-3-5-haiku", 0.80, 4.0, 0.08, None),
    ("claude-3-haiku", 0.25, 1.25, 0.03, None),
];

impl PriceBook {
    pub fn builtin() -> Self {
        let entries = BUILTIN
            .iter()
            .map(|&(model, input, output, cache_read, fast)| PriceEntry {
                model: model.to_string(),
                from: EPOCH,
                rates: Rates {
                    input,
                    output,
                    cache_write_5m: input * 1.25,
                    cache_write_1h: input * 2.0,
                    cache_read,
                    web_search: 0.01,
                    fast_multiplier: fast,
                    // Claude Haiku 5.5: $0.50 / $2.50 above 100K prompt tokens. Whether cache
                    // rates scale the same way is (to verify); assumed yes.
                    long_context: (model == "claude-haiku-5-5").then_some(LongContext {
                        threshold: 100_000,
                        input_multiplier: 5.0,
                        output_multiplier: 5.0,
                    }),
                },
                source: PriceSource::Builtin,
            })
            .collect();
        PriceBook { entries }
    }

    pub fn push(&mut self, e: PriceEntry) {
        self.entries.push(e);
    }

    pub fn entries(&self) -> &[PriceEntry] {
        &self.entries
    }

    /// Merges the `additionalModelCostsCache` object of a `.claude.json`: per model
    /// `{inputTokens, outputTokens, promptCacheWriteTokens, promptCacheWrite1hTokens,
    /// promptCacheReadTokens, webSearchRequests}` in USD per MTok (web search per request).
    /// Fast-mode and long-context rules are not in that cache, so they are kept from the
    /// built-in entry of the same model.
    pub fn merge_claude_code_cache(&mut self, seat: &str, cache: &serde_json::Value) {
        let Some(map) = cache.as_object() else { return };
        for (model, v) in map {
            let f = |k: &str| v.get(k).and_then(|x| x.as_f64());
            let (Some(input), Some(output)) = (f("inputTokens"), f("outputTokens")) else {
                continue;
            };
            let base = self
                .lookup_source(model, &PriceSource::Builtin)
                .map(|e| e.rates);
            let rates = Rates {
                input,
                output,
                cache_write_5m: f("promptCacheWriteTokens").unwrap_or(input * 1.25),
                cache_write_1h: f("promptCacheWrite1hTokens").unwrap_or(input * 2.0),
                cache_read: f("promptCacheReadTokens").unwrap_or(input * 0.1),
                web_search: f("webSearchRequests").unwrap_or(0.01),
                fast_multiplier: base.and_then(|b| b.fast_multiplier),
                long_context: base.and_then(|b| b.long_context),
            };
            let source = PriceSource::ClaudeCode(seat.to_string());
            // Several seats carry the same cache; keep one copy per model and rates.
            if self.entries.iter().any(|e| {
                e.model == *model
                    && e.rates == rates
                    && matches!(e.source, PriceSource::ClaudeCode(_))
            }) {
                continue;
            }
            self.entries.push(PriceEntry {
                model: model.clone(),
                from: EPOCH,
                rates,
                source,
            });
        }
    }

    fn lookup_source(&self, model: &str, source: &PriceSource) -> Option<&PriceEntry> {
        self.entries
            .iter()
            .find(|e| e.model == model && e.source == *source)
    }

    /// The entry that prices `model` on `day`: a user entry beats Claude Code's cache, which beats
    /// the built-in table; then the most specific pattern; then the latest `from` not after `day`.
    /// Dated ids (`claude-haiku-4-5-20251001`) fall back to the id without the date.
    pub fn lookup(&self, model: &str, day: NaiveDate) -> Option<&PriceEntry> {
        let undated = strip_date_suffix(model);
        let mut best: Option<(&PriceEntry, (u8, usize, NaiveDate))> = None;
        for e in &self.entries {
            if e.from > day {
                continue;
            }
            let specificity = match_score(&e.model, model)
                .or_else(|| undated.and_then(|u| match_score(&e.model, u)));
            let Some(spec) = specificity else { continue };
            let prio = match e.source {
                PriceSource::User => 2,
                PriceSource::ClaudeCode(_) => 1,
                PriceSource::Builtin => 0,
            };
            let key = (prio, spec, e.from);
            if best.as_ref().is_none_or(|(_, k)| key > *k) {
                best = Some((e, key));
            }
        }
        best.map(|(e, _)| e)
    }

    pub fn cost(&self, model: &str, day: NaiveDate, usage: &Usage, fast: bool) -> Cost {
        match self.lookup(model, day) {
            Some(e) => Cost {
                usd: cost_with(&e.rates, usage, fast),
                priced: true,
            },
            None => Cost {
                usd: 0.0,
                priced: usage.is_empty(),
            },
        }
    }

    /// Changes whenever any price changes, so stored costs know when to be recomputed.
    pub fn fingerprint(&self) -> String {
        let mut h = Sha256::new();
        for e in &self.entries {
            h.update(serde_json::to_vec(e).unwrap_or_default());
        }
        hex::encode(&h.finalize()[..8])
    }
}

/// USD for one request at the given rates.
pub fn cost_with(r: &Rates, u: &Usage, fast: bool) -> f64 {
    let (mut pin, mut pout) = (1.0, 1.0);
    if let Some(lc) = r.long_context
        && u.prompt() > lc.threshold
    {
        pin = lc.input_multiplier;
        pout = lc.output_multiplier;
    }
    let speed = if fast {
        r.fast_multiplier.unwrap_or(1.0)
    } else {
        1.0
    };
    let tokens = (u.input as f64 * r.input
        + u.cache_write_5m as f64 * r.cache_write_5m
        + u.cache_write_1h as f64 * r.cache_write_1h
        + u.cache_read as f64 * r.cache_read)
        * pin
        + u.output as f64 * r.output * pout;
    (tokens / 1_000_000.0 + u.web_search as f64 * r.web_search) * speed
}

fn match_score(pattern: &str, model: &str) -> Option<usize> {
    match pattern.strip_suffix('*') {
        Some(prefix) => model.starts_with(prefix).then_some(prefix.len()),
        None => (pattern == model).then_some(usize::MAX),
    }
}

/// `claude-3-5-sonnet-20241022` → `claude-3-5-sonnet`.
pub fn strip_date_suffix(model: &str) -> Option<&str> {
    let (head, tail) = model.rsplit_once('-')?;
    (tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit())).then_some(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 22).unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    #[test]
    fn opus_5_5_request_by_hand() {
        // A real request shape: 2 input, 409 written to the 1-hour cache, 155 527 read, 192 out.
        // 2*4 + 409*8 + 155527*0.2 + 192*20 = 8 + 3272 + 31105.4 + 3840 = 38225.4 µ$.
        let u = Usage {
            input: 2,
            cache_write_1h: 409,
            cache_read: 155_527,
            output: 192,
            ..Default::default()
        };
        let c = PriceBook::builtin().cost("claude-opus-5-5", day(), &u, false);
        assert!(c.priced);
        assert!(close(c.usd, 0.0382254), "{}", c.usd);
    }

    #[test]
    fn five_minute_and_one_hour_writes_are_priced_apart() {
        // Sonnet 5.5: 1000 tokens to the 5-min cache at 2.5, 1000 to the 1-h cache at 4.0.
        let u = Usage {
            cache_write_5m: 1000,
            cache_write_1h: 1000,
            ..Default::default()
        };
        let c = PriceBook::builtin().cost("claude-sonnet-5-5", day(), &u, false);
        assert!(close(c.usd, (2500.0 + 4000.0) / 1e6), "{}", c.usd);
    }

    #[test]
    fn fast_mode_doubles_opus_5_5() {
        // $8 / $40 in fast mode: 1M in + 1M out = 48.
        let u = Usage {
            input: 1_000_000,
            output: 1_000_000,
            ..Default::default()
        };
        let c = PriceBook::builtin().cost("claude-opus-5-5", day(), &u, true);
        assert!(close(c.usd, 48.0), "{}", c.usd);
    }

    #[test]
    fn haiku_5_5_long_context() {
        // 150 000 prompt tokens > 100K: input at 0.50, output at 2.50.
        // 150000*0.5 + 1000*2.5 = 75000 + 2500 = 77500 µ$.
        let u = Usage {
            input: 150_000,
            output: 1000,
            ..Default::default()
        };
        let c = PriceBook::builtin().cost("claude-haiku-5-5", day(), &u, false);
        assert!(close(c.usd, 0.0775), "{}", c.usd);
        // At the threshold itself the base rate applies: 100000*0.1 = 10000 µ$.
        let u = Usage {
            input: 100_000,
            ..Default::default()
        };
        assert!(close(
            PriceBook::builtin()
                .cost("claude-haiku-5-5", day(), &u, false)
                .usd,
            0.01
        ));
    }

    #[test]
    fn dated_ids_fall_back_and_unknown_models_are_flagged() {
        let b = PriceBook::builtin();
        assert_eq!(
            b.lookup("claude-haiku-4-5-20251001", day()).unwrap().model,
            "claude-haiku-4-5"
        );
        assert_eq!(
            b.lookup("claude-opus-4-20250514", day()).unwrap().model,
            "claude-opus-4"
        );
        let u = Usage {
            input: 10,
            ..Default::default()
        };
        assert!(!b.cost("claude-unknown-9", day(), &u, false).priced);
        // A synthetic error message with no tokens is not "unpriced".
        assert!(
            b.cost("<synthetic>", day(), &Usage::default(), false)
                .priced
        );
    }

    #[test]
    fn claude_code_cache_beats_builtin_and_user_beats_both() {
        let mut b = PriceBook::builtin();
        let cache = serde_json::json!({"claude-opus-5-5": {"inputTokens": 3, "outputTokens": 15,
            "promptCacheWriteTokens": 3.75, "promptCacheWrite1hTokens": 6, "promptCacheReadTokens": 0.3,
            "webSearchRequests": 0.01}});
        b.merge_claude_code_cache("default", &cache);
        let e = b.lookup("claude-opus-5-5", day()).unwrap();
        assert_eq!(e.rates.input, 3.0);
        assert_eq!(
            e.rates.fast_multiplier,
            Some(2.0),
            "fast rule kept from the built-in entry"
        );
        b.push(PriceEntry {
            model: "claude-opus-*".into(),
            from: EPOCH,
            rates: Rates {
                input: 1.0,
                ..e.rates
            },
            source: PriceSource::User,
        });
        assert_eq!(b.lookup("claude-opus-5-5", day()).unwrap().rates.input, 1.0);
    }

    #[test]
    fn effective_dates_pick_the_version_in_force() {
        let mut b = PriceBook::default();
        let r = PriceBook::builtin()
            .lookup("claude-opus-5-5", day())
            .unwrap()
            .rates;
        let d = |m, dd| NaiveDate::from_ymd_opt(2026, m, dd).unwrap();
        b.push(PriceEntry {
            model: "m".into(),
            from: d(1, 1),
            rates: Rates { input: 1.0, ..r },
            source: PriceSource::User,
        });
        b.push(PriceEntry {
            model: "m".into(),
            from: d(6, 1),
            rates: Rates { input: 2.0, ..r },
            source: PriceSource::User,
        });
        assert_eq!(b.lookup("m", d(5, 31)).unwrap().rates.input, 1.0);
        assert_eq!(b.lookup("m", d(6, 1)).unwrap().rates.input, 2.0);
        assert!(
            b.lookup("m", NaiveDate::from_ymd_opt(2025, 12, 31).unwrap())
                .is_none()
        );
    }
}
