//! Prices in the index: dating Claude Code's cached prices, and pricing every request.

use chrono::{DateTime, Local, NaiveDate, Utc};
use rusqlite::{OptionalExtension, params};

use super::{Index, from_ms, ms, schema};
use crate::core::pricing::{PriceBook, PriceEntry, PriceSource, Rates};
use crate::core::usage::Usage;

impl Index {
    /// `book` with Claude Code's prices dated. Each version of a model's cached price is recorded
    /// in `observed_prices` (a fact table) the first time it is seen, and applies from that day
    /// on, so a price change in the cache never reprices the past. The first version of a model
    /// also covers the days before it was seen when nothing else prices the model then (a model
    /// newer than the built-in table), so it is never left unpriced.
    pub fn dated_prices(
        &mut self,
        book: &PriceBook,
        now: DateTime<Utc>,
    ) -> anyhow::Result<PriceBook> {
        let key = book.fingerprint();
        if let Some((k, dated)) = &self.dated
            && *k == key
        {
            return Ok(dated.clone());
        }
        let today = now.with_timezone(&Local).date_naive();
        // Claude Code's entries per model, in seat order.
        let mut current: Vec<(&str, Vec<(&PriceEntry, &str)>)> = Vec::new();
        for (e, seat) in book.entries().iter().filter_map(|e| match &e.source {
            PriceSource::ClaudeCode(seat) => Some((e, seat.as_str())),
            PriceSource::Builtin | PriceSource::User => None,
        }) {
            match current.iter_mut().find(|(m, _)| *m == e.model) {
                Some((_, v)) => v.push((e, seat)),
                None => current.push((e.model.as_str(), vec![(e, seat)])),
            }
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for (model, versions) in &current {
            let latest: Option<String> = tx
                .query_row(
                    "SELECT rates FROM observed_prices WHERE model = ?1 ORDER BY seen_at DESC LIMIT 1",
                    [model],
                    |r| r.get(0),
                )
                .optional()?;
            let latest: Option<Rates> = latest.map(|j| serde_json::from_str(&j)).transpose()?;
            // Seats may carry different versions of the cache; any of them still current is
            // enough, so two seats never take turns.
            if latest.is_some_and(|l| versions.iter().any(|(e, _)| e.rates == l)) {
                continue;
            }
            let (first, seat) = versions[0];
            tx.execute(
                "INSERT INTO observed_prices(model, seen_at, seen_on, seat, rates) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(model, seen_at) DO NOTHING",
                params![model, ms(now), today.to_string(), seat, serde_json::to_string(&first.rates)?],
            )?;
        }
        let mut observed = Vec::new();
        {
            // Newest first, so a version seen later the same day wins the tie on `from`.
            let mut st = tx.prepare(
                "SELECT model, seen_on, seat, rates,
                        seen_at = (SELECT MIN(o.seen_at) FROM observed_prices o WHERE o.model = p.model)
                 FROM observed_prices p ORDER BY model, seen_at DESC",
            )?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, bool>(4)?,
                ))
            })?;
            for row in rows {
                let (model, seen_on, seat, rates, first) = row?;
                let seen: NaiveDate = seen_on.parse()?;
                let from = if first && !book.priced_without_cache(&model, seen) {
                    None
                } else {
                    Some(seen)
                };
                observed.push(PriceEntry {
                    model,
                    from,
                    rates: serde_json::from_str(&rates)?,
                    source: PriceSource::ClaudeCode(seat),
                });
            }
        }
        tx.commit()?;
        let dated = book.with_observed(observed);
        self.dated = Some((key, dated.clone()));
        Ok(dated)
    }

    /// Prices new requests, and every request when any price changed. One transaction: other
    /// readers see either the old costs or the new ones, never a table half reset.
    pub fn price_all(&mut self, prices: &PriceBook) -> anyhow::Result<()> {
        let book = self.dated_prices(prices, Utc::now())?;
        let fp = book.fingerprint();
        // Nothing to do (the usual case, every second in the dashboard): no write at all.
        let pending = |c: &rusqlite::Connection| {
            c.prepare_cached("SELECT 1 FROM requests WHERE priced IS NULL LIMIT 1")?
                .exists([])
        };
        if self.meta("price_fp")?.as_deref() == Some(fp.as_str()) && !pending(&self.conn)? {
            return Ok(());
        }
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let stale = schema::meta(&tx, "price_fp")?.as_deref() != Some(fp.as_str());
        if stale {
            super::bump_settled(&tx)?;
            tx.execute(
                &format!("UPDATE requests SET {}", schema::unpriced_sql()),
                [],
            )?;
        }
        {
            let mut sel = tx.prepare("SELECT msg_id, model, ts, fast, input, cw5, cw1h, cread, output, thinking, web FROM requests WHERE priced IS NULL")?;
            let mut upd = tx.prepare(&format!(
                "UPDATE requests SET cost = ?1, priced = ?2, {} WHERE msg_id = ?3",
                schema::COST_PARTS
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("{c} = ?{}", i + 4))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))?;
            let rows: Vec<(String, String, i64, bool, Usage)> = sel
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get::<_, i64>(3)? != 0,
                        Usage {
                            input: r.get::<_, i64>(4)? as u64,
                            cache_write_5m: r.get::<_, i64>(5)? as u64,
                            cache_write_1h: r.get::<_, i64>(6)? as u64,
                            cache_read: r.get::<_, i64>(7)? as u64,
                            output: r.get::<_, i64>(8)? as u64,
                            thinking: r.get::<_, i64>(9)? as u64,
                            web_search: r.get::<_, i64>(10)? as u64,
                        },
                    ))
                })?
                .collect::<Result<_, _>>()?;
            for (id, model, ts, fast, usage) in rows {
                let day = from_ms(ts).with_timezone(&Local).date_naive();
                let p = book.cost_parts(&model, day, &usage, fast);
                // In the order of `schema::COST_PARTS`.
                let parts = p.map(|p| {
                    [
                        p.input,
                        p.cache_write,
                        p.cache_read,
                        p.output,
                        p.web,
                        p.cache_saved,
                    ]
                });
                let part = |i: usize| parts.map(|v| v[i]);
                upd.execute(params![
                    p.map(|p| p.total()),
                    p.is_some() as i64,
                    id,
                    part(0),
                    part(1),
                    part(2),
                    part(3),
                    part(4),
                    part(5)
                ])?;
            }
        }
        if stale {
            schema::set_meta(&tx, "price_fp", &fp)?;
        }
        tx.commit()?;
        Ok(())
    }
}
