//! Token usage of one API response, as Claude Code records it in `message.usage`.

use serde::Serialize;

/// Token counts of one request (or a sum of many).
///
/// `cache_write_5m` and `cache_write_1h` split `cache_creation_input_tokens` by TTL, because the
/// two TTLs are billed differently (1.25x and 2x the input rate).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Usage {
    pub input: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub cache_read: u64,
    pub output: u64,
    /// Reported inside `output` by the API; kept separately for statistics only.
    pub thinking: u64,
    pub web_search: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input += o.input;
        self.cache_write_5m += o.cache_write_5m;
        self.cache_write_1h += o.cache_write_1h;
        self.cache_read += o.cache_read;
        self.output += o.output;
        self.thinking += o.thinking;
        self.web_search += o.web_search;
    }

    /// Claude Code writes one record per content block of a response, all carrying the usage of
    /// the same response. Older versions streamed growing partial counts, so duplicates are merged
    /// field by field with `max`, never summed.
    pub fn merge_max(&mut self, o: &Usage) {
        self.input = self.input.max(o.input);
        self.cache_write_5m = self.cache_write_5m.max(o.cache_write_5m);
        self.cache_write_1h = self.cache_write_1h.max(o.cache_write_1h);
        self.cache_read = self.cache_read.max(o.cache_read);
        self.output = self.output.max(o.output);
        self.thinking = self.thinking.max(o.thinking);
        self.web_search = self.web_search.max(o.web_search);
    }

    pub fn cache_write(&self) -> u64 {
        self.cache_write_5m + self.cache_write_1h
    }

    /// Everything the model read for this request: the context size.
    pub fn prompt(&self) -> u64 {
        self.input + self.cache_write() + self.cache_read
    }

    pub fn total(&self) -> u64 {
        self.prompt() + self.output
    }

    /// Share of the prompt served from the cache.
    pub fn cache_hit_ratio(&self) -> Option<f64> {
        let p = self.prompt();
        (p > 0).then(|| self.cache_read as f64 / p as f64)
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0 && self.web_search == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_merge_with_max_not_sum() {
        let a = Usage {
            input: 2,
            cache_read: 100,
            output: 10,
            ..Default::default()
        };
        let b = Usage {
            input: 2,
            cache_read: 100,
            output: 393,
            ..Default::default()
        };
        let mut m = a;
        m.merge_max(&b);
        assert_eq!(
            m,
            Usage {
                input: 2,
                cache_read: 100,
                output: 393,
                ..Default::default()
            }
        );
    }

    #[test]
    fn prompt_and_hit_ratio() {
        let u = Usage {
            input: 10,
            cache_write_5m: 20,
            cache_write_1h: 30,
            cache_read: 140,
            output: 5,
            ..Default::default()
        };
        assert_eq!(u.prompt(), 200);
        assert_eq!(u.total(), 205);
        assert_eq!(u.cache_hit_ratio(), Some(0.7));
        assert_eq!(Usage::default().cache_hit_ratio(), None);
    }
}
