//! How much of this the agent may do, and what it has done lately.
//!
//! ```text
//!   plan ──► budget::state ──► allowance()  ──► how many jobs this pass may take
//!                │                 │
//!                │                 └── 0, and capped() names which limit said so
//!                ▼
//!        reply_suggestion_outcomes WHERE kind = 'generated'
//!        (one row per completed model call, with what it cost)
//! ```
//!
//! # Why a count is the primary limit and dollars are the secondary one
//!
//! The owner runs this against a Claude subscription, through Claude Code. What
//! runs out on that path is quota, not money, and it running out breaks the
//! Claude Code he works in all day — so the limit that has to hold is denominated
//! in the thing being consumed, which is calls.
//!
//! There *is* a dollar figure on that path, because the CLI reports
//! `total_cost_usd` for every run, and it is recorded. But it is what the same
//! tokens would have cost on the API rather than anything he will be invoiced
//! for, so it is the secondary limit: set high enough that it never binds on the
//! intended model, and there to catch [`super::MODEL_KEY`] being pointed at
//! something ten times the price, which the count limit cannot see.
//!
//! The one case with no dollars at all is `/v1/messages` on a subscription
//! bearer token: tokens, no rate, no figure. Those rows carry NULL, the spend
//! limit skips them, and the count limit is what protects him.
//!
//! # Why the windows roll rather than resetting at midnight
//!
//! A calendar day needs a timezone, and a timezone is a thing to get wrong on a
//! laptop that travels. A rolling window needs neither, cannot be gamed by a
//! flood that starts at 23:55, and answers "when does this resume" exactly: the
//! oldest generation still inside the window, plus the window.
//!
//! # The burst limit is the one that stops a flood
//!
//! A daily cap alone still lets an hour of nonsense through, and an hour is
//! plenty of time to notice nothing and spend everything. The hourly limit is
//! what actually bounds the damage; the daily one is what stops a slow leak from
//! adding up over a working day.

use rusqlite::Connection;

use crate::db::Result as DbResult;

// ===========================================================================
// The numbers
// ===========================================================================

/// Generations allowed in any rolling hour.
///
/// # The arithmetic
///
/// Measured against his own store: of the messages that would have earned a
/// suggestion, the busiest single hour in a year of mail held **17**, the 99th
/// percentile hour held 6, and the median hour with any at all held 1. Twenty
/// clears the worst hour that has ever happened to him and is three times the
/// 99th percentile.
///
/// The other end of the range is what makes it a defence: the existing bound is
/// [`super::MAX_PER_PASS`] against a sixty-second poll, so an unbounded flood
/// runs at four a minute — 240 an hour, which is twelve times this.
pub const MAX_PER_HOUR: usize = 20;

/// Generations allowed in any rolling day.
///
/// # The arithmetic
///
/// Same measurement: the busiest day in a year held **43**, the 99th percentile
/// day 29, the 95th 22, and the median day 8. Fifty clears the worst day on
/// record with room to spare and is six times a normal one — it would not have
/// bothered him on any day that has actually happened.
///
/// Against the flood, it is the same twelve-fold gap: four a minute sustained is
/// 5,760 in a day.
pub const MAX_PER_DAY: usize = 50;

/// Dollars allowed in any rolling day, where dollars are known at all.
///
/// # The arithmetic
///
/// One generation on Sonnet is about two thousand tokens of prompt and four
/// hundred of reply: **$0.012** at list price. [`MAX_PER_DAY`] of those is
/// $0.60, so two dollars is a little over three times the expected ceiling and
/// never fires on the intended model.
///
/// Put the other way round, which is the useful way: this trips when a
/// generation averages more than **$0.04**, three times what one should cost.
///
/// # What it does and does not catch
///
/// It catches a model whose *price* changed by that much — Fable at $10/$50 is
/// $0.04 a generation on the same prompt — and it catches any model that starts
/// running to the full
/// [`MAX_STRUCTURED_TOKENS`](crate::ipc::agent::engine::complete::MAX_STRUCTURED_TOKENS)
/// of output, which is where a long thread and a full set of voice examples end
/// up.
///
/// It does **not** catch Opus at ordinary output: $0.02 a generation, $1.00 for
/// a day of fifty, comfortably under. That is not a hole to be plugged by
/// lowering the number — $1.00 is the honest cost of fifty Opus replies, and a
/// dollar limit tight enough to refuse it would refuse a heavy Sonnet day too.
/// The count limits are what bound that case, which is the whole reason they are
/// the primary ones.
pub const MAX_USD_PER_DAY: f64 = 2.00;

/// Overrides, for a machine rather than a person.
///
/// Deliberately not preferences. Three more controls in ⌘, would be three
/// controls nobody opens, on a panel whose whole job is to be got through — and
/// the numbers above are chosen against his real mail rather than as a guess
/// somebody is expected to tune. A variable is enough for the case this is
/// actually for, which is a test and a QA instance.
pub const ENV_PER_HOUR: &str = "MACH_SUGGEST_MAX_PER_HOUR";
pub const ENV_PER_DAY: &str = "MACH_SUGGEST_MAX_PER_DAY";
pub const ENV_USD_PER_DAY: &str = "MACH_SUGGEST_MAX_USD_PER_DAY";

pub const HOUR_MS: i64 = 60 * 60 * 1_000;
pub const DAY_MS: i64 = 24 * HOUR_MS;

/// The limits in force. A struct rather than three constants read at the point
/// of use, so a test can state a whole policy in one line and the environment is
/// read once.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub per_hour: usize,
    pub per_day: usize,
    pub usd_per_day: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            per_hour: MAX_PER_HOUR,
            per_day: MAX_PER_DAY,
            usd_per_day: MAX_USD_PER_DAY,
        }
    }
}

impl Limits {
    /// The defaults, with any of the three moved by the environment.
    ///
    /// A variable that will not parse is ignored rather than fatal — a typo in a
    /// shell must not be a way to switch the cap off, and it must not be a way
    /// to stop the app either.
    pub fn from_env() -> Limits {
        let mut limits = Limits::default();
        if let Some(n) = env_usize(ENV_PER_HOUR) {
            limits.per_hour = n;
        }
        if let Some(n) = env_usize(ENV_PER_DAY) {
            limits.per_day = n;
        }
        if let Some(n) = env_f64(ENV_USD_PER_DAY) {
            limits.usd_per_day = n;
        }
        limits
    }
}

/// Zero is a meaningful value — "generate nothing" — so this filters only on
/// parseability, not on being positive.
fn env_usize(key: &str) -> Option<usize> {
    std::env::var(key).ok()?.trim().parse().ok()
}

fn env_f64(key: &str) -> Option<f64> {
    let value: f64 = std::env::var(key).ok()?.trim().parse().ok()?;
    value.is_finite().then_some(value)
}

// ===========================================================================
// The state
// ===========================================================================

/// Which limit is holding the door shut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capped {
    /// The burst limit. Resumes within the hour.
    Hour,
    /// The day's total.
    Day,
    /// The day's spend, where spend is known.
    Spend,
}

impl Capped {
    /// A short, stable name — for a log line, for the IPC payload, and for a
    /// test to assert on.
    pub fn as_str(self) -> &'static str {
        match self {
            Capped::Hour => "hour",
            Capped::Day => "day",
            Capped::Spend => "spend",
        }
    }
}

/// What has been generated lately, and what the limits are.
#[derive(Debug, Clone, PartialEq)]
pub struct Budget {
    pub limits: Limits,
    /// Generations in the last hour.
    pub hour_count: usize,
    /// Generations in the last day.
    pub day_count: usize,
    /// Dollars in the last day, over the rows that carried a figure.
    pub day_spend_usd: f64,
    /// How many of the day's generations reported a cost at all. Zero means the
    /// spend limit has nothing to act on — the subscription path — and
    /// [`Budget::capped`] will never return [`Capped::Spend`].
    pub day_priced: usize,
    /// When the oldest generation in the hour window leaves it.
    hour_frees_at: Option<i64>,
    /// When the oldest generation in the day window leaves it.
    day_frees_at: Option<i64>,
}

impl Budget {
    /// Nothing generated, nothing spent — the state of a fresh store, and the
    /// honest answer when the ledger cannot be read.
    pub fn empty(limits: Limits) -> Budget {
        Budget {
            limits,
            hour_count: 0,
            day_count: 0,
            day_spend_usd: 0.0,
            day_priced: 0,
            hour_frees_at: None,
            day_frees_at: None,
        }
    }

    /// Which limit is currently refusing, if any.
    ///
    /// Order matters only for what gets reported, not for whether anything runs:
    /// the burst limit is named first because it is the one that resolves
    /// soonest, so it is the more useful thing to have said.
    pub fn capped(&self) -> Option<Capped> {
        if self.hour_count >= self.limits.per_hour {
            return Some(Capped::Hour);
        }
        if self.day_count >= self.limits.per_day {
            return Some(Capped::Day);
        }
        // Only where a figure exists. A day of subscription generations has
        // `day_priced == 0` and a spend of zero, and calling that "under the
        // limit" would be as wrong as calling it over.
        if self.day_priced > 0 && self.day_spend_usd >= self.limits.usd_per_day {
            return Some(Capped::Spend);
        }
        None
    }

    /// Which limit would refuse the next generation, having already taken
    /// `taken` of them since this budget was read.
    ///
    /// [`Budget::capped`] answers for `taken == 0`. This exists because a pass
    /// that starts with two of its twenty left and finds three messages worth
    /// answering is capped *during* the pass, and the panel should say which
    /// limit did it rather than reporting the state from before the pass ran.
    ///
    /// Spend cannot move with `taken` — what the next call will cost is not
    /// knowable before it is made — so this only sharpens the count limits.
    pub fn binding_at(&self, taken: usize) -> Option<Capped> {
        Budget {
            hour_count: self.hour_count + taken,
            day_count: self.day_count + taken,
            ..self.clone()
        }
        .capped()
    }

    /// How many more generations may run right now.
    ///
    /// The smaller of the two count headrooms, and zero whenever anything is
    /// capped — including by spend, which has no headroom to express in
    /// generations.
    pub fn allowance(&self) -> usize {
        if self.capped().is_some() {
            return 0;
        }
        let hour = self.limits.per_hour.saturating_sub(self.hour_count);
        let day = self.limits.per_day.saturating_sub(self.day_count);
        hour.min(day)
    }

    /// When the capped state ends, as a millisecond timestamp.
    ///
    /// Exact rather than approximate: a rolling window frees a slot the moment
    /// its oldest member falls out, and that moment is a row's `at_ms` plus the
    /// window. `None` when nothing is capped, and also when the limit is zero —
    /// waiting does not help if the allowance is zero to begin with.
    pub fn resumes_at(&self) -> Option<i64> {
        match self.capped()? {
            Capped::Hour => self.hour_frees_at,
            // Spend rides the day's window: it is dollars over the same
            // twenty-four hours, so it clears when the day's oldest does.
            Capped::Day | Capped::Spend => self.day_frees_at,
        }
    }
}

/// Read the ledger.
///
/// One query over the day window, with the hour counted inside it — the hour is
/// a subset of the day, so a second scan would read the same rows again. Takes a
/// connection and a clock and returns a value, like everything else in this
/// module's neighbourhood, so a test drives it with a hand-seeded table and no
/// engine anywhere near it.
pub fn state(conn: &Connection, now_ms: i64) -> DbResult<Budget> {
    let limits = Limits::from_env();
    state_with(conn, limits, now_ms)
}

/// The same, with the limits supplied rather than read from the environment.
///
/// Every test that asserts on a cap uses this: the environment is process-wide
/// and the test binary runs its cases in parallel, so a test that set
/// `MACH_SUGGEST_MAX_PER_HOUR` would be setting it for whatever else happened to
/// be running.
pub fn state_with(conn: &Connection, limits: Limits, now_ms: i64) -> DbResult<Budget> {
    let hour_floor = now_ms - HOUR_MS;
    let day_floor = now_ms - DAY_MS;

    let row = conn.query_row(
        "SELECT
             sum(CASE WHEN at_ms > ?1 THEN 1 ELSE 0 END),
             min(CASE WHEN at_ms > ?1 THEN at_ms END),
             count(*),
             min(at_ms),
             total(cost_usd),
             count(cost_usd)
           FROM reply_suggestion_outcomes
          WHERE kind = ?3 AND at_ms > ?2",
        rusqlite::params![hour_floor, day_floor, super::Outcome::Generated.as_str()],
        |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
                // `total()` rather than `sum()`: it returns 0.0 rather than NULL
                // over an empty set, and over a set that is entirely NULL — the
                // subscription case, where no row carries a figure.
                row.get::<_, f64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        },
    )?;
    let (hour_count, hour_oldest, day_count, day_oldest, spend, priced) = row;

    Ok(Budget {
        limits,
        hour_count: hour_count.max(0) as usize,
        day_count: day_count.max(0) as usize,
        day_spend_usd: spend,
        day_priced: priced.max(0) as usize,
        hour_frees_at: hour_oldest.map(|at| at + HOUR_MS),
        day_frees_at: day_oldest.map(|at| at + DAY_MS),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema;
    use crate::suggest::store::{self, Generation, Outcome};

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        schema::migrate(&mut conn).unwrap();
        conn
    }

    fn tight() -> Limits {
        Limits {
            per_hour: 3,
            per_day: 5,
            usd_per_day: 1.0,
        }
    }

    /// One generation, at a time, costing what it says.
    fn generated(conn: &Connection, at_ms: i64, cost: Option<f64>) {
        store::record_generation(
            conn,
            &Generation {
                model: "claude-sonnet-5".into(),
                cost_usd: cost,
                input_tokens: Some(2_000),
                output_tokens: Some(400),
            },
            at_ms,
        )
        .unwrap();
    }

    const NOW: i64 = 10 * DAY_MS;

    #[test]
    fn an_empty_ledger_has_the_whole_allowance() {
        let budget = state_with(&db(), tight(), NOW).unwrap();
        assert_eq!(budget.capped(), None);
        assert_eq!(budget.allowance(), 3);
        assert_eq!(budget.resumes_at(), None);
        assert_eq!(budget.day_spend_usd, 0.0);
    }

    #[test]
    fn the_hourly_limit_caps_a_burst() {
        let conn = db();
        for n in 0..3 {
            generated(&conn, NOW - 60_000 * (n + 1), Some(0.02));
        }
        let budget = state_with(&conn, tight(), NOW).unwrap();
        assert_eq!(budget.hour_count, 3);
        assert_eq!(budget.capped(), Some(Capped::Hour));
        assert_eq!(budget.allowance(), 0);
        // The oldest of the three was three minutes ago, so a slot frees an hour
        // after that and not a moment sooner.
        assert_eq!(budget.resumes_at(), Some(NOW - 180_000 + HOUR_MS));
    }

    #[test]
    fn the_hourly_limit_lets_go_when_its_window_rolls() {
        let conn = db();
        // Three, all just over an hour ago.
        for n in 0..3 {
            generated(&conn, NOW - HOUR_MS - 1_000 * (n + 1), Some(0.02));
        }
        let budget = state_with(&conn, tight(), NOW).unwrap();
        assert_eq!(budget.hour_count, 0, "they have fallen out of the hour");
        assert_eq!(budget.day_count, 3, "and are still inside the day");
        assert_eq!(budget.capped(), None);
        assert_eq!(budget.allowance(), 2, "the day's headroom is the binding one");
    }

    #[test]
    fn the_daily_limit_holds_when_the_hour_has_rolled() {
        let conn = db();
        // Five, spread across the day so no single hour is over its own limit.
        for n in 0..5 {
            generated(&conn, NOW - HOUR_MS * 2 * (n + 1), Some(0.02));
        }
        let budget = state_with(&conn, tight(), NOW).unwrap();
        assert_eq!(budget.hour_count, 0);
        assert_eq!(budget.day_count, 5);
        assert_eq!(budget.capped(), Some(Capped::Day));
        assert_eq!(budget.allowance(), 0);
    }

    #[test]
    fn the_daily_limit_lets_go_when_its_window_rolls() {
        let conn = db();
        for n in 0..5 {
            generated(&conn, NOW - DAY_MS - 1_000 * (n + 1), Some(0.02));
        }
        let budget = state_with(&conn, tight(), NOW).unwrap();
        assert_eq!(budget.day_count, 0);
        assert_eq!(budget.capped(), None);
        assert_eq!(budget.allowance(), 3);
    }

    #[test]
    fn spend_caps_before_the_count_does_when_a_generation_got_expensive() {
        let conn = db();
        // Two generations, well under both count limits, at Opus money.
        generated(&conn, NOW - HOUR_MS * 3, Some(0.60));
        generated(&conn, NOW - HOUR_MS * 2, Some(0.55));
        let budget = state_with(&conn, tight(), NOW).unwrap();
        assert_eq!(budget.day_count, 2, "well inside the count limit");
        assert!((budget.day_spend_usd - 1.15).abs() < 1e-9);
        assert_eq!(budget.capped(), Some(Capped::Spend));
        assert_eq!(budget.allowance(), 0);
    }

    #[test]
    fn a_day_of_unpriced_generations_is_never_capped_by_spend() {
        // The subscription path: the tokens were real, the dollars are not
        // knowable, and a spend of zero must not read as "spent nothing".
        let conn = db();
        for n in 0..4 {
            generated(&conn, NOW - HOUR_MS * 2 * (n + 1), None);
        }
        let budget = state_with(&conn, tight(), NOW).unwrap();
        assert_eq!(budget.day_count, 4);
        assert_eq!(budget.day_priced, 0);
        assert_eq!(budget.day_spend_usd, 0.0);
        assert_eq!(
            budget.capped(),
            None,
            "an unknown cost must not read as a free one"
        );
    }

    #[test]
    fn spend_counts_only_the_rows_that_carried_a_figure() {
        let conn = db();
        generated(&conn, NOW - HOUR_MS * 3, Some(0.30));
        generated(&conn, NOW - HOUR_MS * 2, None);
        let budget = state_with(&conn, tight(), NOW).unwrap();
        assert!((budget.day_spend_usd - 0.30).abs() < 1e-9);
        assert_eq!(budget.day_priced, 1);
        assert_eq!(budget.day_count, 2);
    }

    #[test]
    fn the_other_outcomes_are_not_generations() {
        // Pressing a stance costs nothing and must not consume the budget.
        let conn = db();
        for _ in 0..10 {
            store::record(&conn, Outcome::Picked, Some(0), "Say yes", NOW - 1_000).unwrap();
            store::record(&conn, Outcome::Suggested, None, "", NOW - 1_000).unwrap();
        }
        let budget = state_with(&conn, tight(), NOW).unwrap();
        assert_eq!(budget.hour_count, 0);
        assert_eq!(budget.capped(), None);
    }

    #[test]
    fn a_limit_of_zero_refuses_everything_and_promises_nothing() {
        let limits = Limits {
            per_hour: 0,
            ..tight()
        };
        let budget = state_with(&db(), limits, NOW).unwrap();
        assert_eq!(budget.capped(), Some(Capped::Hour));
        assert_eq!(budget.allowance(), 0);
        assert_eq!(
            budget.resumes_at(),
            None,
            "waiting does not help when the allowance is zero"
        );
    }

    #[test]
    fn the_defaults_clear_his_worst_real_day_and_stop_a_flood() {
        let limits = Limits::default();
        // Measured against his own store: the busiest day in a year of mail
        // that would have earned suggestions held 43, the busiest hour 17.
        assert!(limits.per_day > 43, "{} would have bitten", limits.per_day);
        assert!(limits.per_hour > 17, "{} would have bitten", limits.per_hour);
        // And the flood the cap exists for: MAX_PER_PASS every sixty seconds.
        let flood_per_hour = crate::suggest::MAX_PER_PASS * 60;
        assert!(
            flood_per_hour > limits.per_hour * 4,
            "the cap is not far enough below an unbounded flood"
        );
    }

    // -----------------------------------------------------------------------
    // The dollar limit against the price table
    //
    // These live here rather than beside the table because what they assert is
    // a property of *this* module's numbers. The table is the input.
    // -----------------------------------------------------------------------

    use crate::ipc::agent::engine::complete::Usage;
    use crate::ipc::agent::engine::config::{AgentConfig, Credential};
    use crate::ipc::agent::engine::price;

    fn with_key() -> AgentConfig {
        AgentConfig {
            credential: Credential::ApiKey("k".into()),
            model: "claude-opus-5".into(),
            effort: "medium".into(),
            max_tokens: 32_000,
            base_url: "https://api.anthropic.test".into(),
            fallbacks: true,
        }
    }

    fn usage(input: i64, output: i64) -> Usage {
        Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            ..Default::default()
        }
    }

    #[test]
    fn the_dollar_cap_sits_above_a_full_day_of_the_intended_model() {
        // A day at MAX_PER_DAY on Sonnet must not trip the dollar limit —
        // otherwise the limit is a second count limit with a worse name.
        let sonnet = price::cost_usd(&with_key(), "claude-sonnet-5", &usage(2_000, 400)).unwrap();
        let per_day = MAX_PER_DAY as f64;
        assert!(
            sonnet * per_day < MAX_USD_PER_DAY / 3.0,
            "a normal day costs {:.2} against a {MAX_USD_PER_DAY:.2} cap — too close",
            sonnet * per_day
        );
    }

    #[test]
    fn the_dollar_cap_catches_a_generation_that_got_much_more_expensive() {
        // What it is actually for: the per-generation cost tripling, whether
        // from a pricier model or from output running to the structured cap.
        let per_day = MAX_PER_DAY as f64;
        for (model, usage) in [
            ("claude-fable-5", usage(2_000, 400)),
            ("claude-sonnet-5", usage(4_000, 2_400)),
            ("claude-opus-5", usage(4_000, 2_400)),
        ] {
            let cost = price::cost_usd(&with_key(), model, &usage).unwrap();
            assert!(
                cost * per_day >= MAX_USD_PER_DAY,
                "{model} at {cost:.4} would run a full day under the cap"
            );
        }
    }

    #[test]
    fn opus_at_ordinary_length_is_under_the_cap_and_that_is_the_counts_job() {
        // Stated as a test so nobody later "fixes" the dollar limit downward:
        // fifty Opus replies really do cost about a dollar, and a limit tight
        // enough to refuse that would refuse a busy Sonnet day as well.
        let opus = price::cost_usd(&with_key(), "claude-opus-5", &usage(2_000, 400)).unwrap();
        assert!(opus * (MAX_PER_DAY as f64) < MAX_USD_PER_DAY);
    }

    #[test]
    fn every_capped_state_has_a_name() {
        use std::collections::BTreeSet;
        let all = [Capped::Hour, Capped::Day, Capped::Spend];
        let names: BTreeSet<&str> = all.iter().map(|c| c.as_str()).collect();
        assert_eq!(names.len(), all.len());
    }
}
